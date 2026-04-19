//! End-to-end integration test: the host drives a `VirtualBus`-paired
//! [`StubDevice`] through the full ISO-TP + protocol stack.
//!
//! This is the first test that exercises every layer landed so far:
//!
//! - `protocol::ids` — build / decode 11-bit IDs
//! - `protocol::isotp` — segment outgoing commands, reassemble incoming
//!   responses
//! - `protocol::commands` — payload builders
//! - `protocol::responses` — parse what came back
//! - `transport::CanBackend` — the trait itself
//! - `transport::virtual_bus` — in-process loopback
//! - `transport::stub_device` — minimum bootloader impl
//!
//! When a new real backend lands (SLCAN, SocketCAN, PCAN) the
//! bootloader-side of this test stays the stub; the only thing that
//! swaps is the adapter under the `CanBackend` on the host side.

use std::time::Duration;

use tokio::sync::oneshot;

use can_flasher::protocol::commands::{
    cmd_connect_self, cmd_disconnect, cmd_discover, cmd_flash_erase, PROTOCOL_VERSION_MAJOR,
    PROTOCOL_VERSION_MINOR,
};
use can_flasher::protocol::ids::{FrameId, MessageType};
use can_flasher::protocol::isotp::{IsoTpSegmenter, ReassembleOutcome, Reassembler};
use can_flasher::protocol::opcodes::{CommandOpcode, NackCode};
use can_flasher::protocol::{CanFrame, Response, BROADCAST_NODE_ID, HOST_NODE_ID};
use can_flasher::transport::{CanBackend, StubDevice, TransportError, VirtualBus};

const STUB_NODE_ID: u8 = 0x3;
const FRAME_TIMEOUT: Duration = Duration::from_millis(200);

/// Test-side "client" that owns a host backend, a reassembler, and
/// enough glue to send a protocol message and wait for the matching
/// reply. Not a public API — the real flasher builds its own higher-
/// level session abstraction in feat/9.
struct Client {
    backend: Box<dyn CanBackend>,
    reasm: Reassembler,
    /// See StubDevice::pending_msg_type — CFs ride as TYPE=DATA on the
    /// wire, so the message type has to be captured from the SF/FF.
    pending_msg_type: Option<MessageType>,
}

impl Client {
    fn new(backend: Box<dyn CanBackend>) -> Self {
        Self {
            backend,
            reasm: Reassembler::with_timeout(1_000),
            pending_msg_type: None,
        }
    }

    /// Send an ISO-TP-segmented message with a chosen `MessageType`
    /// on the ID.
    async fn send_message(
        &self,
        message_type: MessageType,
        dst: u8,
        payload: &[u8],
    ) -> Result<(), TransportError> {
        let initial_id = FrameId::new(message_type, HOST_NODE_ID, dst)
            .unwrap()
            .encode();
        let cf_id = FrameId::new(MessageType::Data, HOST_NODE_ID, dst)
            .unwrap()
            .encode();

        let seg = IsoTpSegmenter::new(payload).expect("segment");
        for (idx, frame_bytes) in seg.enumerate() {
            let id = if idx == 0 { initial_id } else { cf_id };
            let frame = CanFrame {
                id,
                data: frame_bytes,
                len: frame_bytes.len() as u8,
            };
            self.backend.send(frame).await?;
        }
        Ok(())
    }

    /// Drain frames from the backend until the reassembler completes
    /// a message or `deadline` elapses.
    async fn recv_message(
        &mut self,
        deadline: Duration,
    ) -> Result<(MessageType, Vec<u8>), TransportError> {
        let start = std::time::Instant::now();
        loop {
            let remaining = deadline.checked_sub(start.elapsed()).unwrap_or_default();
            if remaining.is_zero() {
                return Err(TransportError::Timeout(deadline));
            }
            let frame = self.backend.recv(remaining).await?;
            let id = FrameId::decode(frame.id).expect("valid id from stub");

            // Capture the TYPE from SF (PCI 0x0_) and FF (PCI 0x1_);
            // CFs (PCI 0x2_) ride as TYPE=DATA so taking id.message_type
            // from those would lose the original type.
            let payload_bytes = frame.payload();
            if let Some(pci_hi) = payload_bytes.first().map(|b| b & 0xF0) {
                if pci_hi == 0x00 || pci_hi == 0x10 {
                    self.pending_msg_type = Some(id.message_type);
                }
            }

            match self.reasm.feed(payload_bytes, tick_ms()) {
                Ok(ReassembleOutcome::Ongoing) => continue,
                Ok(ReassembleOutcome::Complete(bytes)) => {
                    let msg_type = self.pending_msg_type.take().unwrap_or(id.message_type);
                    return Ok((msg_type, bytes));
                }
                Err(err) => panic!("reassembler bailed: {err:?}"),
            }
        }
    }
}

/// Synthetic monotonic ms — tokio timers tick inside the test.
fn tick_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    let start = *START.get_or_init(Instant::now);
    start.elapsed().as_millis() as u64
}

/// Spin up a fresh bus, stub device, and host-side client. Returns
/// the client and a cancel handle the test should fire on teardown.
async fn setup() -> (Client, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let bus = VirtualBus::new();
    let host_backend = Box::new(bus.host_backend());
    let device_backend = Box::new(bus.device_backend());
    // Leaking the bus keeps the channel endpoints alive for the
    // lifetime of the test. In a real integration harness we'd keep
    // the VirtualBus on the stack of the test fn.
    std::mem::forget(bus);

    let stub = StubDevice::new(device_backend, STUB_NODE_ID);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let handle = tokio::spawn(async move {
        stub.run(cancel_rx).await.expect("stub run");
    });
    (Client::new(host_backend), cancel_tx, handle)
}

#[tokio::test]
async fn connect_elicits_ack_with_protocol_version() {
    let (mut client, cancel, handle) = setup().await;

    client
        .send_message(MessageType::Cmd, STUB_NODE_ID, &cmd_connect_self())
        .await
        .unwrap();

    let (mt, payload) = client.recv_message(FRAME_TIMEOUT).await.unwrap();
    assert_eq!(mt, MessageType::Ack);

    match Response::parse(mt, &payload).unwrap() {
        Response::Ack { opcode, payload } => {
            assert_eq!(opcode, CommandOpcode::Connect.as_byte());
            assert_eq!(
                payload,
                vec![PROTOCOL_VERSION_MAJOR, PROTOCOL_VERSION_MINOR]
            );
        }
        other => panic!("expected Ack, got {other:?}"),
    }

    drop(cancel);
    let _ = handle.await;
}

#[tokio::test]
async fn discover_broadcast_elicits_discover_reply() {
    let (mut client, cancel, handle) = setup().await;

    client
        .send_message(MessageType::Discover, BROADCAST_NODE_ID, &cmd_discover())
        .await
        .unwrap();

    let (mt, payload) = client.recv_message(FRAME_TIMEOUT).await.unwrap();
    assert_eq!(mt, MessageType::Discover);
    match Response::parse(mt, &payload).unwrap() {
        Response::Discover {
            node_id,
            proto_major,
            proto_minor,
        } => {
            assert_eq!(node_id, STUB_NODE_ID);
            assert_eq!(proto_major, PROTOCOL_VERSION_MAJOR);
            assert_eq!(proto_minor, PROTOCOL_VERSION_MINOR);
        }
        other => panic!("expected Discover, got {other:?}"),
    }

    drop(cancel);
    let _ = handle.await;
}

#[tokio::test]
async fn disconnect_clears_session_and_acks() {
    let (mut client, cancel, handle) = setup().await;

    // First connect so there's an active session to clear.
    client
        .send_message(MessageType::Cmd, STUB_NODE_ID, &cmd_connect_self())
        .await
        .unwrap();
    let _ = client.recv_message(FRAME_TIMEOUT).await.unwrap();

    // Now disconnect.
    client
        .send_message(MessageType::Cmd, STUB_NODE_ID, &cmd_disconnect())
        .await
        .unwrap();
    let (mt, payload) = client.recv_message(FRAME_TIMEOUT).await.unwrap();
    match Response::parse(mt, &payload).unwrap() {
        Response::Ack { opcode, .. } => {
            assert_eq!(opcode, CommandOpcode::Disconnect.as_byte());
        }
        other => panic!("expected Ack(Disconnect), got {other:?}"),
    }

    drop(cancel);
    let _ = handle.await;
}

#[tokio::test]
async fn unknown_opcode_earns_nack_unsupported() {
    let (mut client, cancel, handle) = setup().await;

    // The stub only implements CONNECT / DISCONNECT / DISCOVER in
    // feat/4 — FLASH_ERASE is still "not implemented" on the stub,
    // which dispatches it to `NACK(UNSUPPORTED)`.
    client
        .send_message(
            MessageType::Cmd,
            STUB_NODE_ID,
            &cmd_flash_erase(0x0802_0000, 0x20000),
        )
        .await
        .unwrap();

    let (mt, payload) = client.recv_message(FRAME_TIMEOUT).await.unwrap();
    assert_eq!(mt, MessageType::Nack);
    match Response::parse(mt, &payload).unwrap() {
        Response::Nack {
            rejected_opcode,
            code,
        } => {
            assert_eq!(rejected_opcode, CommandOpcode::FlashErase.as_byte());
            assert_eq!(code, NackCode::Unsupported);
        }
        other => panic!("expected Nack, got {other:?}"),
    }

    drop(cancel);
    let _ = handle.await;
}

#[tokio::test]
async fn version_mismatch_earns_nack_protocol_version() {
    let (mut client, cancel, handle) = setup().await;

    // Craft a CONNECT claiming major=99 — stub should NACK.
    let bad_connect = vec![CommandOpcode::Connect.as_byte(), 99, 0];
    client
        .send_message(MessageType::Cmd, STUB_NODE_ID, &bad_connect)
        .await
        .unwrap();

    let (mt, payload) = client.recv_message(FRAME_TIMEOUT).await.unwrap();
    assert_eq!(mt, MessageType::Nack);
    match Response::parse(mt, &payload).unwrap() {
        Response::Nack {
            rejected_opcode,
            code,
        } => {
            assert_eq!(rejected_opcode, CommandOpcode::Connect.as_byte());
            assert_eq!(code, NackCode::ProtocolVersion);
        }
        other => panic!("expected Nack(ProtocolVersion), got {other:?}"),
    }

    drop(cancel);
    let _ = handle.await;
}

#[tokio::test]
async fn frame_addressed_to_other_node_is_ignored() {
    let (mut client, cancel, handle) = setup().await;

    // Send CONNECT to node 0x5 instead of the stub's 0x3. No reply
    // should come back — the stub's `addressed_to` check drops it.
    client
        .send_message(MessageType::Cmd, 0x5, &cmd_connect_self())
        .await
        .unwrap();

    let err = client
        .recv_message(Duration::from_millis(50))
        .await
        .unwrap_err();
    assert!(matches!(err, TransportError::Timeout(_)));

    drop(cancel);
    let _ = handle.await;
}
