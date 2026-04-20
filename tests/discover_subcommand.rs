//! Integration test for the `discover` subcommand's enrichment
//! pipeline against a live [`StubDevice`].
//!
//! The stub implements `CMD_DISCOVER` but NACKs `GET_FW_INFO` and
//! `GET_HEALTH` with `UNSUPPORTED` — so the per-responder enrichment
//! step exercises the degrade-gracefully path end-to-end. When the
//! stub grows real `GET_FW_INFO` / `GET_HEALTH` handlers (later feat
//! branches), this test will keep passing and additionally assert
//! against the populated fields.

use std::time::Duration;

use tokio::sync::oneshot;

use can_flasher::protocol::commands::{cmd_discover, cmd_get_fw_info, cmd_get_health};
use can_flasher::protocol::ids::MessageType;
use can_flasher::protocol::opcodes::NackCode;
use can_flasher::protocol::records::HealthRecord;
use can_flasher::protocol::Response;
use can_flasher::session::{Session, SessionConfig};
use can_flasher::transport::{CanBackend, StubDevice, VirtualBus};

const STUB_NODE: u8 = 0x3;

async fn spawn_session_and_stub() -> (Session, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
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
        target_node: 0x0,
        keepalive_interval: Duration::from_millis(5_000),
        command_timeout: Duration::from_millis(200),
        ..SessionConfig::default()
    };
    let session = Session::attach(Box::new(host), config);
    (session, cancel_tx, handle)
}

#[tokio::test]
async fn broadcast_discover_finds_stub() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    let replies = session
        .broadcast(
            &cmd_discover(),
            MessageType::Discover,
            Duration::from_millis(150),
        )
        .await
        .expect("broadcast");
    assert_eq!(replies.len(), 1);
    match &replies[0] {
        Response::Discover { node_id, .. } => assert_eq!(*node_id, STUB_NODE),
        other => panic!("expected Discover, got {other:?}"),
    }
    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn stub_nacks_fw_info_with_unsupported() {
    // Exercises the degrade-gracefully path of discover's enrichment
    // loop. The stub represents a bootloader with no app installed,
    // so GET_FW_INFO returns NACK(UNSUPPORTED) (functionally
    // equivalent to NoValidApp from the host's point of view). The
    // discover subcommand maps this to fw_error; this test asserts
    // the NACK surfaces correctly at the session layer.
    let (session, cancel, handle) = spawn_session_and_stub().await;

    let fw = session
        .send_command_to(STUB_NODE, &cmd_get_fw_info())
        .await
        .expect("GET_FW_INFO");
    match fw {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::Unsupported),
        other => panic!("expected Nack for GET_FW_INFO against stub, got {other:?}"),
    }

    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn stub_answers_get_health_with_synthetic_record() {
    // As of feat/11 the stub answers GET_HEALTH with a realistic
    // 32-byte HealthRecord (uptime from the monotonic clock, reset
    // cause POWER_ON, session flag reflecting internal state). This
    // test confirms the wire format round-trips cleanly.
    let (session, cancel, handle) = spawn_session_and_stub().await;

    let resp = session
        .send_command_to(STUB_NODE, &cmd_get_health())
        .await
        .expect("GET_HEALTH");
    match resp {
        Response::Ack { opcode, payload } => {
            assert_eq!(opcode, 0x05, "GET_HEALTH ACK opcode");
            let record = HealthRecord::parse(&payload).expect("parse HealthRecord");
            // Fresh session — not connected. `session_active` should
            // be false; reset cause should be POWER_ON (stub's
            // latched default).
            assert!(!record.session_active());
            assert_eq!(record.reset_cause().map(|r| r.as_str()), Some("POWER_ON"),);
        }
        other => panic!("expected Ack for GET_HEALTH against stub, got {other:?}"),
    }

    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn send_command_to_an_absent_node_times_out() {
    // No node 0x7 on the bus — the stub is 0x3. The session should
    // time out cleanly instead of wedging.
    let (session, cancel, handle) = spawn_session_and_stub().await;
    let err = session
        .send_command_to(0x7, &cmd_get_fw_info())
        .await
        .expect_err("expected timeout");
    match err {
        can_flasher::session::SessionError::CommandTimeout(d) => {
            assert_eq!(d, Duration::from_millis(200));
        }
        other => panic!("expected CommandTimeout, got {other:?}"),
    }
    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}
