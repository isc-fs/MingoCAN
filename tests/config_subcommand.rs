//! Integration test for the `config` subcommand, driving the stub
//! through the OB + NVM wire surfaces end-to-end. The stub keeps an
//! in-memory NVM map + WRP mask so the same session can round-trip
//! writes + reads.

use std::time::Duration;

use tokio::sync::oneshot;

use can_flasher::protocol::commands::{
    cmd_connect_self, cmd_nvm_read, cmd_nvm_write, cmd_ob_apply_wrp, cmd_ob_read,
};
use can_flasher::protocol::opcodes::NackCode;
use can_flasher::protocol::records::ObStatus;
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

async fn tear_down(
    session: Session,
    cancel: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
) {
    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}

// ---- ob read ----

#[tokio::test]
async fn ob_read_returns_synthetic_status() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    let resp = session.send_command(&cmd_ob_read()).await.unwrap();
    match resp {
        Response::Ack { opcode, payload } => {
            assert_eq!(opcode, 0x50);
            let status = ObStatus::parse(&payload).unwrap();
            // Fresh stub: nothing protected, RDP level 0xAA
            // (level 0), BOR 0.
            assert_eq!(status.wrp_sector_mask, 0);
            assert_eq!(status.rdp_level, 0xAA);
            assert_eq!(status.bor_level, 0x00);
        }
        other => panic!("expected Ack, got {other:?}"),
    }
    tear_down(session, cancel, handle).await;
}

// ---- ob apply-wrp ----

#[tokio::test]
async fn ob_apply_wrp_rejects_without_session() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    // Don't CONNECT first — stub should NACK BadSession.
    let resp = session
        .send_command(&cmd_ob_apply_wrp(Some(0x01)))
        .await
        .unwrap();
    match resp {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::BadSession),
        other => panic!("expected Nack(BadSession), got {other:?}"),
    }
    tear_down(session, cancel, handle).await;
}

#[tokio::test]
async fn ob_apply_wrp_rejects_without_token() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    session.send_command(&cmd_connect_self()).await.unwrap();

    // Hand-craft a payload without the token — just [opcode] + 3
    // bytes of garbage. Stub must reject with OB_WRONG_TOKEN.
    let bad_payload = vec![0x51_u8, 0xDE, 0xAD, 0xBE];
    let resp = session.send_command(&bad_payload).await.unwrap();
    match resp {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::ObWrongToken),
        other => panic!("expected Nack(ObWrongToken), got {other:?}"),
    }
    tear_down(session, cancel, handle).await;
}

#[tokio::test]
async fn ob_apply_wrp_with_correct_token_and_mask_updates_wrp_state() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    session.send_command(&cmd_connect_self()).await.unwrap();

    // Apply WRP to sectors 0 + 2.
    let resp = session
        .send_command(&cmd_ob_apply_wrp(Some(0b101)))
        .await
        .unwrap();
    assert!(matches!(resp, Response::Ack { .. }));

    // Subsequent OB_READ (session-less — doesn't need the session)
    // should reflect the new mask.
    let resp = session.send_command(&cmd_ob_read()).await.unwrap();
    match resp {
        Response::Ack { payload, .. } => {
            let status = ObStatus::parse(&payload).unwrap();
            assert!(status.is_sector_protected(0));
            assert!(!status.is_sector_protected(1));
            assert!(status.is_sector_protected(2));
        }
        other => panic!("expected Ack, got {other:?}"),
    }
    tear_down(session, cancel, handle).await;
}

// ---- nvm read / write / erase ----

#[tokio::test]
async fn nvm_read_missing_key_returns_not_found() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    session.send_command(&cmd_connect_self()).await.unwrap();

    let resp = session.send_command(&cmd_nvm_read(0x1234)).await.unwrap();
    match resp {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::NvmNotFound),
        other => panic!("expected Nack(NvmNotFound), got {other:?}"),
    }
    tear_down(session, cancel, handle).await;
}

#[tokio::test]
async fn nvm_write_then_read_round_trips_value() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    session.send_command(&cmd_connect_self()).await.unwrap();

    let key = 0x1000;
    let value = b"rust";

    // Write
    let resp = session
        .send_command(&cmd_nvm_write(key, value))
        .await
        .unwrap();
    assert!(matches!(resp, Response::Ack { .. }));

    // Read back
    let resp = session.send_command(&cmd_nvm_read(key)).await.unwrap();
    match resp {
        Response::Ack { payload, .. } => {
            // [len, value...]
            assert_eq!(payload[0] as usize, value.len());
            assert_eq!(&payload[1..], value);
        }
        other => panic!("expected Ack on NVM_READ, got {other:?}"),
    }
    tear_down(session, cancel, handle).await;
}

#[tokio::test]
async fn nvm_erase_then_read_returns_not_found() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    session.send_command(&cmd_connect_self()).await.unwrap();

    let key = 0x2001;
    session
        .send_command(&cmd_nvm_write(key, b"hello"))
        .await
        .unwrap();
    // Erase = write with zero-length value
    session
        .send_command(&cmd_nvm_write(key, &[]))
        .await
        .unwrap();

    let resp = session.send_command(&cmd_nvm_read(key)).await.unwrap();
    match resp {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::NvmNotFound),
        other => panic!("expected Nack(NvmNotFound) after erase, got {other:?}"),
    }
    tear_down(session, cancel, handle).await;
}

#[tokio::test]
async fn nvm_write_rejects_oversize_value() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    session.send_command(&cmd_connect_self()).await.unwrap();

    // BL_NVM_MAX_VALUE_LEN = 20 bytes.
    let big_value = vec![0xAA_u8; 21];
    let resp = session
        .send_command(&cmd_nvm_write(0x3000, &big_value))
        .await
        .unwrap();
    match resp {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::Unsupported),
        other => panic!("expected Nack(Unsupported) for oversize, got {other:?}"),
    }
    tear_down(session, cancel, handle).await;
}

#[tokio::test]
async fn nvm_read_needs_session() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    // No CONNECT
    let resp = session.send_command(&cmd_nvm_read(0x1000)).await.unwrap();
    match resp {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::BadSession),
        other => panic!("expected Nack(BadSession), got {other:?}"),
    }
    tear_down(session, cancel, handle).await;
}
