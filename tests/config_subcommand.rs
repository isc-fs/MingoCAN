//! Integration test for the `config` subcommand, driving the stub
//! through the OB + NVM wire surfaces end-to-end. The stub keeps an
//! in-memory NVM map + WRP mask so the same session can round-trip
//! writes + reads.

use std::time::Duration;

use tokio::sync::oneshot;

use can_flasher::protocol::commands::{
    cmd_connect_self, cmd_nvm_format, cmd_nvm_read, cmd_nvm_write, cmd_ob_apply_wrp, cmd_ob_read,
    cmd_reset,
};
use can_flasher::protocol::opcodes::CommandOpcode;
use can_flasher::protocol::opcodes::NackCode;
use can_flasher::protocol::opcodes::ResetMode;
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
async fn nvm_format_wipes_every_key() {
    // CMD_NVM_FORMAT round-trip: write a couple of keys, format,
    // confirm both reads now miss with NVM_NOT_FOUND.
    let (session, cancel, handle) = spawn_session_and_stub().await;
    session.send_command(&cmd_connect_self()).await.unwrap();

    session
        .send_command(&cmd_nvm_write(0x4001, b"one"))
        .await
        .unwrap();
    session
        .send_command(&cmd_nvm_write(0x4002, b"two"))
        .await
        .unwrap();

    let resp = session.send_command(&cmd_nvm_format()).await.unwrap();
    match resp {
        Response::Ack { .. } => {}
        other => panic!("expected ACK after NVM_FORMAT, got {other:?}"),
    }

    for key in [0x4001_u16, 0x4002] {
        let resp = session.send_command(&cmd_nvm_read(key)).await.unwrap();
        match resp {
            Response::Nack { code, .. } => assert_eq!(code, NackCode::NvmNotFound),
            other => panic!("expected NVM_NOT_FOUND for 0x{key:04X} after format, got {other:?}"),
        }
    }
    tear_down(session, cancel, handle).await;
}

#[tokio::test]
async fn nvm_format_with_bad_token_nacks_wrong_token() {
    // Wrong token = NACK(NVM_WRONG_TOKEN); the in-memory store
    // must stay intact (we read back after the failed format).
    let (session, cancel, handle) = spawn_session_and_stub().await;
    session.send_command(&cmd_connect_self()).await.unwrap();

    let key = 0x4100;
    session
        .send_command(&cmd_nvm_write(key, b"keep me"))
        .await
        .unwrap();

    // Hand-craft a CMD_NVM_FORMAT payload with the wrong 4-byte
    // token (deliberately not BL_NVM_FORMAT_TOKEN).
    let mut bad = vec![CommandOpcode::NvmFormat.as_byte()];
    bad.extend_from_slice(&0xDEAD_BEEF_u32.to_le_bytes());

    let resp = session.send_command(&bad).await.unwrap();
    match resp {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::NvmWrongToken),
        other => panic!("expected NACK(NvmWrongToken) for bad token, got {other:?}"),
    }

    // Confirm the value survived the failed format. NVM_READ ACK
    // payload is `[len, value…]` (no leading opcode byte).
    let resp = session.send_command(&cmd_nvm_read(key)).await.unwrap();
    match resp {
        Response::Ack { payload, .. } => {
            assert_eq!(payload[0] as usize, "keep me".len());
            assert_eq!(&payload[1..], b"keep me", "value should be intact");
        }
        other => panic!("expected ACK with original value, got {other:?}"),
    }
    tear_down(session, cancel, handle).await;
}

#[tokio::test]
async fn nvm_format_needs_session() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    // No CONNECT
    let resp = session.send_command(&cmd_nvm_format()).await.unwrap();
    match resp {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::BadSession),
        other => panic!("expected Nack(BadSession), got {other:?}"),
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

// ---- nvm write + --reset semantics (gh #231 task 1) ----
//
// These tests replicate the wire pattern produced by
// `run_nvm_write(..., reset, ...)` in `src/cli/config.rs` and
// assert the stub observed exactly one (or zero) `CMD_RESET`
// frames after the NVM write ACK arrives. The integration goes
// through the same Session + StubDevice harness as the rest of
// this file; we only need a slightly richer setup that hands
// back the stub's `reset_counter_handle` so we can inspect it
// after tear_down.

use std::sync::atomic::AtomicU32;
use std::sync::Arc;

/// Variant of `spawn_session_and_stub` that also returns the
/// stub's reset-counter handle. Tests that need to verify "did
/// the host send a CMD_RESET" use this; the other tests in this
/// file ignore the counter and use the smaller helper.
async fn spawn_with_reset_inspector() -> (
    Session,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
    Arc<AtomicU32>,
) {
    let bus = VirtualBus::new();
    let host = bus.host_backend();
    let device: Box<dyn CanBackend> = Box::new(bus.device_backend());
    drop(bus);

    let stub = StubDevice::new(device, STUB_NODE);
    // Grab the counter handle *before* moving the stub into its
    // run-loop task — once it's moved we can't get at it.
    let reset_counter = stub.reset_counter_handle();

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
    (session, cancel_tx, handle, reset_counter)
}

#[tokio::test]
async fn nvm_write_without_reset_does_not_send_cmd_reset() {
    // Mirrors `run_nvm_write(..., reset = false, ...)`:
    //   CONNECT → NVM_WRITE → disconnect.
    let (session, cancel, handle, reset_counter) = spawn_with_reset_inspector().await;
    session.send_command(&cmd_connect_self()).await.unwrap();

    let resp = session
        .send_command(&cmd_nvm_write(0x0001, &[0x02]))
        .await
        .unwrap();
    assert!(matches!(resp, Response::Ack { .. }));

    tear_down(session, cancel, handle).await;

    // No CMD_RESET should have crossed the wire — the operator
    // didn't opt in.
    assert_eq!(
        reset_counter.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "default (reset=false) flow must not send CMD_RESET",
    );
}

#[tokio::test]
async fn nvm_write_with_reset_sends_cmd_reset_bootloader() {
    // Mirrors `run_nvm_write(..., reset = true, ...)`:
    //   CONNECT → NVM_WRITE → CMD_RESET[Bootloader] → disconnect.
    let (session, cancel, handle, reset_counter) = spawn_with_reset_inspector().await;
    session.send_command(&cmd_connect_self()).await.unwrap();

    let resp = session
        .send_command(&cmd_nvm_write(0x0001, &[0x02]))
        .await
        .unwrap();
    assert!(matches!(resp, Response::Ack { .. }));

    // CMD_RESET is the boot-only-NVM-key escape hatch. We don't
    // care whether the ACK arrives — real hardware reboots before
    // sending one — only that the host emitted the frame.
    let _ = session
        .send_command(&cmd_reset(ResetMode::Bootloader))
        .await;

    tear_down(session, cancel, handle).await;

    assert_eq!(
        reset_counter.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "reset=true flow must emit exactly one CMD_RESET frame",
    );
}
