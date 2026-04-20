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

// ---- CLI-facing convenience: Virtual + StubDevice in one wrapper ----

/// A `VirtualBus` paired with a running [`StubDevice`] on the device
/// side. The host-facing [`CanBackend`] methods delegate to the
/// underlying [`VirtualBackend`]; on drop the stub task is cancelled
/// via a `oneshot` handle so there's no orphan task when the caller
/// finishes.
///
/// This is the shape `open_backend(InterfaceType::Virtual, …)` hands
/// back to the CLI. Integration tests that want more control (e.g.
/// a custom stub or multiple backends per bus) construct the
/// [`VirtualBus`] + [`StubDevice`] by hand like `tests/virtual_pipeline.rs`
/// already does.
pub struct StubLoopback {
    host: VirtualBackend,
    // Kept inside Mutex<Option<…>> so Drop can take() them without
    // moving out of self. The cancel signal fires regardless of
    // whether the JoinHandle is awaited — tokio's task cleanup
    // handles the rest.
    cancel: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    _stub_handle: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    description: String,
}

impl StubLoopback {
    /// Build a new loopback. The stub is spawned on the current
    /// tokio runtime and runs until the returned value is dropped.
    /// Must be called from within a tokio runtime context —
    /// `tokio::spawn` panics otherwise.
    pub fn new(node_id: u8) -> Result<Self> {
        use crate::transport::StubDevice;

        let bus = VirtualBus::new();
        let host = bus.host_backend();
        let device: Box<dyn CanBackend> = Box::new(bus.device_backend());
        // Dropping `bus` here is safe: both endpoints hold cloned
        // channel handles (Arc-wrapped receivers, cloned senders),
        // so the channels stay alive for the life of the endpoints.

        let stub = StubDevice::new(device, node_id);
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            // Errors from the stub run loop are swallowed — they're
            // logged inside `StubDevice::run` and the CLI can't do
            // much about them. Clean shutdown (Ok) and backend
            // disconnect (Ok) are both expected outcomes.
            let _ = stub.run(cancel_rx).await;
        });

        Ok(Self {
            host,
            cancel: std::sync::Mutex::new(Some(cancel_tx)),
            _stub_handle: std::sync::Mutex::new(Some(handle)),
            description: format!("virtual bus + in-process stub bootloader (node 0x{node_id:X})"),
        })
    }
}

impl Drop for StubLoopback {
    fn drop(&mut self) {
        if let Ok(mut cancel) = self.cancel.lock() {
            if let Some(tx) = cancel.take() {
                // Best-effort: the stub's cancel receiver might
                // already be dropped if the backend disconnect tripped
                // the early-exit path. Either way the task exits
                // cleanly.
                let _ = tx.send(());
            }
        }
        // The JoinHandle is left to self-detach; no blocking join
        // because Drop is called on a sync context and we can't
        // .await here.
    }
}

#[async_trait]
impl CanBackend for StubLoopback {
    async fn send(&self, frame: CanFrame) -> Result<()> {
        self.host.send(frame).await
    }

    async fn recv(&self, timeout: Duration) -> Result<CanFrame> {
        self.host.recv(timeout).await
    }

    async fn set_bitrate(&self, nominal_bps: u32) -> Result<()> {
        self.host.set_bitrate(nominal_bps).await
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

    fn host_to_node_frame(dst: u8, payload: &[u8]) -> CanFrame {
        let id = FrameId::from_host(dst).unwrap().encode();
        CanFrame::new(id, payload).unwrap()
    }

    fn node_to_host_frame(src: u8, payload: &[u8]) -> CanFrame {
        let id = FrameId::from_node(src).unwrap().encode();
        CanFrame::new(id, payload).unwrap()
    }

    #[tokio::test]
    async fn host_send_is_device_recv() {
        let bus = VirtualBus::new();
        let host = bus.host_backend();
        let device = bus.device_backend();

        // SF CMD to node 0x3: [PCI_SF|len=3, MSG_CMD=0x00, opcode=0x01, arg=0x01]
        let frame = host_to_node_frame(0x3, &[0x03, MessageType::Cmd.as_byte(), 0x01, 0x01]);
        host.send(frame).await.unwrap();

        let got = device.recv(Duration::from_millis(100)).await.unwrap();
        assert_eq!(got, frame);
    }

    #[tokio::test]
    async fn device_send_is_host_recv() {
        let bus = VirtualBus::new();
        let host = bus.host_backend();
        let device = bus.device_backend();

        // SF ACK from node 0x3: [PCI_SF|len=2, MSG_ACK=0x01, opcode=0x01]
        let frame = node_to_host_frame(0x3, &[0x02, MessageType::Ack.as_byte(), 0x01]);
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
        let frame = host_to_node_frame(0x3, &[]);
        let err = host.send(frame).await.unwrap_err();
        assert!(matches!(err, TransportError::Disconnected));
    }

    #[tokio::test]
    async fn two_frames_preserve_order() {
        let bus = VirtualBus::new();
        let host = bus.host_backend();
        let device = bus.device_backend();

        let f1 = host_to_node_frame(0x3, &[0x01]);
        let f2 = host_to_node_frame(0x3, &[0x02]);
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

        let frame = host_to_node_frame(0x3, &[0x01]);
        host.send(frame).await.unwrap();

        // Host's own send should not echo back into its recv. We wait
        // a short window; no frame is the right answer.
        let err = host.recv(Duration::from_millis(10)).await.unwrap_err();
        assert!(matches!(err, TransportError::Timeout(_)));
    }
}
