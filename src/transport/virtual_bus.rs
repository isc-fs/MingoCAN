//! In-process loopback bus + backend. Used by integration tests and
//! CI to exercise the full host-side pipeline without any CAN
//! hardware attached.
//!
//! Model: the bus is two independent unbounded-ish `mpsc` channels,
//! one per direction. Each endpoint's "send" feeds one channel and
//! its "recv" drains the other. That's the minimum needed for a
//! host ↔ single-stub-device pair — a real multi-node CAN bus
//! (1 host + N devices) needs a broadcast channel with per-node
//! self-filtering, which we'll revisit once integration tests start
//! covering multi-node scenarios.
//!
//! Why not use `tokio::sync::broadcast` today?
//!
//! - Broadcast gives every subscriber every frame, which is how a
//!   real CAN bus works — but it means each node would have to
//!   filter its own transmissions back out to avoid loopback. That's
//!   extra complexity we don't need at 2 nodes.
//! - Broadcast has bounded buffer semantics; a slow receiver missing
//!   frames becomes a silent failure. On `mpsc` a dropped frame
//!   surfaces as a `TrySendError` we can surface as
//!   [`TransportError::Other`].
//!
//! When multi-node tests land, this file gains a
//! `VirtualBus::broadcast_backend(node_id)` that layers filtering on
//! top of a broadcast channel, and [`VirtualBus::host_backend`] /
//! [`VirtualBus::device_backend`] stay as the 2-node convenience
//! shortcut.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::sync::Mutex;

use crate::protocol::CanFrame;

use super::{CanBackend, Result, TransportError};

/// Channel depth. Generous because we're in-process — back-pressuring
/// the sender turns a missed decision about bus pacing into a
/// test-time deadlock. 1024 frames ≈ 8 KB of live buffer per
/// direction, which is fine.
const CHANNEL_DEPTH: usize = 1024;

/// A 2-node virtual bus. Hand out endpoints via
/// [`VirtualBus::host_backend`] / [`VirtualBus::device_backend`];
/// each endpoint implements [`CanBackend`].
pub struct VirtualBus {
    host_tx: mpsc::Sender<CanFrame>,
    host_rx: Arc<Mutex<mpsc::Receiver<CanFrame>>>,
    device_tx: mpsc::Sender<CanFrame>,
    device_rx: Arc<Mutex<mpsc::Receiver<CanFrame>>>,
}

impl VirtualBus {
    /// Create a fresh bus with two unconnected-yet endpoints. Call
    /// `host_backend()` / `device_backend()` to get them.
    pub fn new() -> Self {
        // h2d: host → device direction. host writes, device reads.
        let (h2d_tx, h2d_rx) = mpsc::channel::<CanFrame>(CHANNEL_DEPTH);
        // d2h: device → host direction.
        let (d2h_tx, d2h_rx) = mpsc::channel::<CanFrame>(CHANNEL_DEPTH);

        Self {
            host_tx: h2d_tx,
            host_rx: Arc::new(Mutex::new(d2h_rx)),
            device_tx: d2h_tx,
            device_rx: Arc::new(Mutex::new(h2d_rx)),
        }
    }

    /// Host-side endpoint. Host transmits go into the host→device
    /// channel; host receives come from the device→host channel.
    pub fn host_backend(&self) -> VirtualBackend {
        VirtualBackend {
            tx: self.host_tx.clone(),
            rx: Arc::clone(&self.host_rx),
            description: "virtual bus (host endpoint)".to_string(),
        }
    }

    /// Device-side endpoint. Symmetric to `host_backend` but on the
    /// other direction pair — attach the [`StubDevice`] or any test
    /// peer here.
    ///
    /// [`StubDevice`]: super::StubDevice
    pub fn device_backend(&self) -> VirtualBackend {
        VirtualBackend {
            tx: self.device_tx.clone(),
            rx: Arc::clone(&self.device_rx),
            description: "virtual bus (device endpoint)".to_string(),
        }
    }
}

impl Default for VirtualBus {
    fn default() -> Self {
        Self::new()
    }
}

/// One endpoint of a [`VirtualBus`]. Cheap to clone; the channel
/// handles share an `Arc`-backed receiver so concurrent clones
/// coordinate through the same mutex.
pub struct VirtualBackend {
    tx: mpsc::Sender<CanFrame>,
    rx: Arc<Mutex<mpsc::Receiver<CanFrame>>>,
    description: String,
}

#[async_trait]
impl CanBackend for VirtualBackend {
    async fn send(&self, frame: CanFrame) -> Result<()> {
        self.tx
            .send(frame)
            .await
            .map_err(|_| TransportError::Disconnected)
    }

    async fn recv(&self, timeout: Duration) -> Result<CanFrame> {
        // Lock held only for the duration of the recv await. Works
        // because a VirtualBackend has one logical reader — we don't
        // support two concurrent recvers on the same endpoint, and
        // the mutex exists solely to let `recv` take `&self` rather
        // than `&mut self` in the trait surface.
        let mut rx = self.rx.lock().await;
        match tokio::time::timeout(timeout, rx.recv()).await {
            Ok(Some(frame)) => Ok(frame),
            Ok(None) => Err(TransportError::Disconnected),
            Err(_elapsed) => Err(TransportError::Timeout(timeout)),
        }
    }

    async fn set_bitrate(&self, _nominal_bps: u32) -> Result<()> {
        // Virtual bus is rate-agnostic; accept any value.
        Ok(())
    }

    fn description(&self) -> String {
        self.description.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::FrameId;
    use crate::protocol::MessageType;

    fn make_frame(message_type: MessageType, dst: u8, payload: &[u8]) -> CanFrame {
        let id = FrameId::from_host(message_type, dst).unwrap().encode();
        CanFrame::new(id, payload).unwrap()
    }

    #[tokio::test]
    async fn host_send_is_device_recv() {
        let bus = VirtualBus::new();
        let host = bus.host_backend();
        let device = bus.device_backend();

        let frame = make_frame(MessageType::Cmd, 0x3, &[0x01, 0, 1]);
        host.send(frame).await.unwrap();

        let got = device.recv(Duration::from_millis(100)).await.unwrap();
        assert_eq!(got, frame);
    }

    #[tokio::test]
    async fn device_send_is_host_recv() {
        let bus = VirtualBus::new();
        let host = bus.host_backend();
        let device = bus.device_backend();

        let frame = make_frame(MessageType::Ack, 0x0, &[0x01, 0, 1]);
        device.send(frame).await.unwrap();

        let got = host.recv(Duration::from_millis(100)).await.unwrap();
        assert_eq!(got, frame);
    }

    #[tokio::test]
    async fn recv_returns_timeout_when_idle() {
        let bus = VirtualBus::new();
        let host = bus.host_backend();
        let err = host.recv(Duration::from_millis(10)).await.unwrap_err();
        match err {
            TransportError::Timeout(d) => assert_eq!(d, Duration::from_millis(10)),
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_fails_after_endpoint_drop() {
        let bus = VirtualBus::new();
        let host = bus.host_backend();
        // Drop the device backend AND the bus handle that owns the
        // device_rx Arc. Without either holding on, the mpsc receiver
        // goes away and host send fails.
        drop(bus);
        let frame = make_frame(MessageType::Cmd, 0x3, &[]);
        let err = host.send(frame).await.unwrap_err();
        assert!(matches!(err, TransportError::Disconnected));
    }

    #[tokio::test]
    async fn two_frames_preserve_order() {
        let bus = VirtualBus::new();
        let host = bus.host_backend();
        let device = bus.device_backend();

        let f1 = make_frame(MessageType::Cmd, 0x3, &[0x01]);
        let f2 = make_frame(MessageType::Cmd, 0x3, &[0x02]);
        host.send(f1).await.unwrap();
        host.send(f2).await.unwrap();

        assert_eq!(device.recv(Duration::from_millis(50)).await.unwrap(), f1);
        assert_eq!(device.recv(Duration::from_millis(50)).await.unwrap(), f2);
    }

    #[tokio::test]
    async fn endpoints_dont_see_their_own_transmissions() {
        let bus = VirtualBus::new();
        let host = bus.host_backend();
        let _device = bus.device_backend();

        let frame = make_frame(MessageType::Cmd, 0x3, &[0x01]);
        host.send(frame).await.unwrap();

        // Host's own send should not echo back into its recv. We wait
        // a short window; no frame is the right answer.
        let err = host.recv(Duration::from_millis(10)).await.unwrap_err();
        assert!(matches!(err, TransportError::Timeout(_)));
    }
}
