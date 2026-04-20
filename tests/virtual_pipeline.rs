//! End-to-end integration test: the host [`Session`] drives a
//! [`VirtualBus`]-paired [`StubDevice`] through the full ISO-TP +
//! protocol stack.
//!
//! Exercises every layer landed so far:
//!
//! - `protocol::ids` / `protocol::isotp` / `protocol::commands` /
//!   `protocol::responses` / `protocol::opcodes` (the wire format)
//! - `transport::CanBackend` + `transport::virtual_bus` (in-process
//!   loopback)
//! - `transport::stub_device` (minimum bootloader impl)
//! - `session::Session` (connect / send_command / broadcast /
//!   disconnect / notification subscription)
//!
//! When a new real backend lands the bootloader-side of this test
//! stays the stub; the only thing that swaps is the adapter under
//! the `CanBackend` on the host side.

use std::time::Duration;

use tokio::sync::oneshot;

use can_flasher::protocol::commands::{PROTOCOL_VERSION_MAJOR, PROTOCOL_VERSION_MINOR};
use can_flasher::protocol::ids::MessageType;
use can_flasher::protocol::opcodes::NackCode;
use can_flasher::protocol::Response;
use can_flasher::session::{Session, SessionConfig, SessionError};
use can_flasher::transport::{CanBackend, StubDevice, VirtualBus};

const STUB_NODE: u8 = 0x3;
const COLLECT_WINDOW: Duration = Duration::from_millis(150);

/// Build a fresh session over a Virtual bus with a running stub on
/// the device side. Caller must drop `cancel` + await `handle` at
/// the end of the test to tear down the stub cleanly.
async fn setup() -> (Session, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let bus = VirtualBus::new();
    let host = bus.host_backend();
    let device: Box<dyn CanBackend> = Box::new(bus.device_backend());
    drop(bus);

    let stub = StubDevice::new(device, STUB_NODE);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let handle = tokio::spawn(async move {
        let _ = stub.run(cancel_rx).await;
    });

    // Tight timings so tests finish quickly. Production defaults use
    // 5 s keepalive / 500 ms command timeout; 250 ms / 200 ms here
    // still exercises the same code paths without adding real
    // latency to the test run.
    let config = SessionConfig {
        target_node: STUB_NODE,
        keepalive_interval: Duration::from_millis(250),
        command_timeout: Duration::from_millis(200),
        host_major: PROTOCOL_VERSION_MAJOR,
        host_minor: PROTOCOL_VERSION_MINOR,
    };
    let session = Session::attach(Box::new(host), config);
    (session, cancel_tx, handle)
}

async fn teardown(
    session: Session,
    cancel: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
) {
    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn connect_elicits_ack_with_protocol_version() {
    let (session, cancel, handle) = setup().await;
    let (major, minor) = session.connect().await.expect("connect");
    assert_eq!(major, PROTOCOL_VERSION_MAJOR);
    assert_eq!(minor, PROTOCOL_VERSION_MINOR);
    assert!(session.is_connected());
    teardown(session, cancel, handle).await;
}

#[tokio::test]
async fn discover_broadcast_elicits_discover_reply() {
    let (session, cancel, handle) = setup().await;
    let replies = session
        .broadcast(
            &can_flasher::protocol::commands::cmd_discover(),
            MessageType::DiscoverRequest,
            COLLECT_WINDOW,
        )
        .await
        .expect("broadcast");
    assert_eq!(replies.len(), 1, "one stub → one reply");
    match &replies[0] {
        Response::Discover {
            node_id,
            proto_major,
            proto_minor,
        } => {
            assert_eq!(*node_id, STUB_NODE);
            assert_eq!(*proto_major, PROTOCOL_VERSION_MAJOR);
            assert_eq!(*proto_minor, PROTOCOL_VERSION_MINOR);
        }
        other => panic!("expected Discover, got {other:?}"),
    }
    teardown(session, cancel, handle).await;
}

#[tokio::test]
async fn disconnect_clears_session_and_acks() {
    let (session, cancel, handle) = setup().await;
    session.connect().await.expect("connect");
    assert!(session.is_connected());
    // `disconnect` consumes the session; re-check `is_connected`
    // via `stub_quiet` instead — after disconnect the stub will
    // NACK subsequent commands with nothing to say if we'd pushed
    // one. For this test we just confirm disconnect returns Ok.
    session.disconnect().await.expect("disconnect");
    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn unknown_opcode_earns_nack_unsupported() {
    let (session, cancel, handle) = setup().await;
    session.connect().await.expect("connect");

    // Opcode 0x20 isn't defined in `CommandOpcode`, so the stub's
    // dispatch takes the `Err(_) → NACK(UNSUPPORTED)` fallthrough.
    // Ride it on a 9-byte payload so ISO-TP segments into FF + CF
    // and we still exercise the multi-frame reassembly path.
    // (Previous iterations used `cmd_flash_erase` and `cmd_jump`,
    // but both now have real stub handlers; an undefined byte is
    // the only thing guaranteed to take the fallthrough.)
    let mut payload = vec![0u8; 9];
    payload[0] = 0x20; // undefined opcode
    let resp = session.send_command(&payload).await.expect("send");
    match resp {
        Response::Nack {
            rejected_opcode,
            code,
        } => {
            assert_eq!(rejected_opcode, 0x20);
            assert_eq!(code, NackCode::Unsupported);
        }
        other => panic!("expected Nack, got {other:?}"),
    }

    teardown(session, cancel, handle).await;
}

#[tokio::test]
async fn version_mismatch_earns_nack_protocol_version() {
    let bus = VirtualBus::new();
    let host = bus.host_backend();
    let device: Box<dyn CanBackend> = Box::new(bus.device_backend());
    drop(bus);

    let stub = StubDevice::new(device, STUB_NODE);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let handle = tokio::spawn(async move {
        let _ = stub.run(cancel_rx).await;
    });

    // Claim a major version the stub doesn't recognise.
    let config = SessionConfig {
        target_node: STUB_NODE,
        keepalive_interval: Duration::from_millis(250),
        command_timeout: Duration::from_millis(200),
        host_major: 99,
        host_minor: 0,
    };
    let session = Session::attach(Box::new(host), config);
    let err = session.connect().await.expect_err("bad-major connect");
    assert!(
        matches!(err, SessionError::ProtocolVersionMismatch { .. }),
        "got {err:?}"
    );
    assert!(!session.is_connected());

    drop(session);
    let _ = cancel_tx.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn command_addressed_to_other_node_times_out() {
    // Build a session pointing at a node the stub isn't answering
    // as. The stub's addressed_to filter drops our CONNECT; we see
    // a CommandTimeout instead of a NACK.
    let bus = VirtualBus::new();
    let host = bus.host_backend();
    let device: Box<dyn CanBackend> = Box::new(bus.device_backend());
    drop(bus);

    let stub = StubDevice::new(device, STUB_NODE);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let handle = tokio::spawn(async move {
        let _ = stub.run(cancel_rx).await;
    });

    let config = SessionConfig {
        target_node: 0x5, // stub is 0x3, so 0x5 is silently dropped
        keepalive_interval: Duration::from_millis(250),
        command_timeout: Duration::from_millis(100),
        host_major: PROTOCOL_VERSION_MAJOR,
        host_minor: PROTOCOL_VERSION_MINOR,
    };
    let session = Session::attach(Box::new(host), config);
    let err = session
        .connect()
        .await
        .expect_err("routed-elsewhere connect");
    match err {
        SessionError::CommandTimeout(_) => {}
        other => panic!("expected CommandTimeout, got {other:?}"),
    }

    drop(session);
    let _ = cancel_tx.send(());
    let _ = handle.await;
}
