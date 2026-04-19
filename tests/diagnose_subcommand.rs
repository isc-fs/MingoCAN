//! Integration test for the `diagnose` subcommand, exercising each
//! wire path against the [`StubDevice`] (the one-shot paths —
//! streaming log / live-data don't have the stub emitting anything
//! to subscribe to, so they're covered by manual smoke tests).

use std::time::Duration;

use tokio::sync::oneshot;

use can_flasher::protocol::commands::{
    cmd_connect_self, cmd_dtc_clear, cmd_dtc_read, cmd_get_health, cmd_live_data_start,
    cmd_log_stream_start, cmd_reset,
};
use can_flasher::protocol::opcodes::{NackCode, ResetMode};
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
        target_node: STUB_NODE,
        keepalive_interval: Duration::from_millis(5_000),
        command_timeout: Duration::from_millis(200),
        ..SessionConfig::default()
    };
    let session = Session::attach(Box::new(host), config);
    (session, cancel_tx, handle)
}

// ---- health ----

#[tokio::test]
async fn get_health_returns_synthetic_record_with_reset_cause() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    let resp = session.send_command(&cmd_get_health()).await.unwrap();
    match resp {
        Response::Ack { opcode, payload } => {
            assert_eq!(opcode, 0x05);
            let record = HealthRecord::parse(&payload).unwrap();
            assert_eq!(record.reset_cause().map(|r| r.as_str()), Some("POWER_ON"));
            assert!(!record.session_active(), "session not yet connected");
        }
        other => panic!("unexpected reply: {other:?}"),
    }
    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn health_flag_reflects_session_state_after_connect() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    session.connect().await.unwrap();

    let resp = session.send_command(&cmd_get_health()).await.unwrap();
    match resp {
        Response::Ack { payload, .. } => {
            let record = HealthRecord::parse(&payload).unwrap();
            assert!(
                record.session_active(),
                "stub should report active session after CONNECT"
            );
        }
        other => panic!("unexpected reply: {other:?}"),
    }

    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}

// ---- dtc-read / dtc-clear ----

#[tokio::test]
async fn dtc_read_returns_empty_list_from_fresh_stub() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    let resp = session.send_command(&cmd_dtc_read()).await.unwrap();
    match resp {
        Response::Ack { opcode, payload } => {
            assert_eq!(opcode, 0x40);
            // payload = [count_le16] — two bytes, both zero.
            assert_eq!(payload, vec![0, 0]);
        }
        other => panic!("unexpected reply: {other:?}"),
    }
    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn dtc_clear_needs_session_and_acks_when_connected() {
    let (session, cancel, handle) = spawn_session_and_stub().await;

    // Without a session: expect BAD_SESSION.
    let resp = session.send_command(&cmd_dtc_clear()).await.unwrap();
    match resp {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::BadSession),
        other => panic!("expected Nack(BadSession), got {other:?}"),
    }

    // After CONNECT: ACK.
    session
        .send_command(&cmd_connect_self())
        .await
        .expect("CONNECT");
    let resp = session.send_command(&cmd_dtc_clear()).await.unwrap();
    match resp {
        Response::Ack { opcode, .. } => assert_eq!(opcode, 0x41),
        other => panic!("expected Ack, got {other:?}"),
    }

    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}

// ---- log / live-data start stop (session-gated ACKs only) ----

#[tokio::test]
async fn log_stream_start_without_session_nacks() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    let resp = session
        .send_command(&cmd_log_stream_start(0))
        .await
        .unwrap();
    match resp {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::BadSession),
        other => panic!("expected Nack, got {other:?}"),
    }
    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn log_and_live_data_start_ack_once_session_active() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    session.connect().await.unwrap();

    let log = session
        .send_command(&cmd_log_stream_start(1))
        .await
        .unwrap();
    assert!(matches!(log, Response::Ack { .. }));

    let live = session
        .send_command(&cmd_live_data_start(10))
        .await
        .unwrap();
    assert!(matches!(live, Response::Ack { .. }));

    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}

// ---- reset ----

#[tokio::test]
async fn reset_modes_0_through_3_all_ack() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    for mode in [
        ResetMode::Hard,
        ResetMode::Soft,
        ResetMode::Bootloader,
        ResetMode::App,
    ] {
        let resp = session.send_command(&cmd_reset(mode)).await.unwrap();
        assert!(
            matches!(resp, Response::Ack { .. }),
            "mode {mode:?} should ACK, got {resp:?}"
        );
    }
    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}
