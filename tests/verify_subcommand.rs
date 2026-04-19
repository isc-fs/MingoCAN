//! Integration test for the `verify` subcommand's wire path against
//! the stub, plus a couple of firmware-loader round-trips that
//! depend on the full crate's public API.
//!
//! Happy path and mismatch path both drive `FLASH_VERIFY` through
//! the session layer; the stub's `expected_verify` gate lets the
//! mismatch case fire deterministically.

use std::time::Duration;

use tokio::sync::oneshot;

use can_flasher::firmware::{self, crc32, Image};
use can_flasher::protocol::commands::{cmd_connect_self, cmd_flash_verify};
use can_flasher::protocol::opcodes::NackCode;
use can_flasher::protocol::Response;
use can_flasher::session::{Session, SessionConfig};
use can_flasher::transport::{CanBackend, StubDevice, VirtualBus};

const STUB_NODE: u8 = 0x3;

async fn spawn_session_with_expected_verify(
    expected: Option<(u32, u32, u32)>,
) -> (Session, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let bus = VirtualBus::new();
    let host = bus.host_backend();
    let device: Box<dyn CanBackend> = Box::new(bus.device_backend());
    drop(bus);

    let mut stub = StubDevice::new(device, STUB_NODE);
    stub.set_expected_verify(expected);
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

// ---- Wire path: happy + mismatch ----

#[tokio::test]
async fn flash_verify_happy_path_against_stub() {
    // Stub configured to expect exactly (0xDEAD_BEEF, 256, 0x0001_0402);
    // send that triple and expect ACK.
    let expected = (0xDEAD_BEEFu32, 256u32, 0x0001_0402u32);
    let (session, cancel, handle) = spawn_session_with_expected_verify(Some(expected)).await;

    // CONNECT first — FLASH_VERIFY is session-gated.
    session.send_command(&cmd_connect_self()).await.unwrap();

    let resp = session
        .send_command(&cmd_flash_verify(expected.0, expected.1, expected.2))
        .await
        .unwrap();
    assert!(matches!(resp, Response::Ack { .. }));

    tear_down(session, cancel, handle).await;
}

#[tokio::test]
async fn flash_verify_mismatch_against_stub() {
    let expected = (0xDEAD_BEEFu32, 256u32, 0x0001_0402u32);
    let (session, cancel, handle) = spawn_session_with_expected_verify(Some(expected)).await;
    session.send_command(&cmd_connect_self()).await.unwrap();

    // Submit the wrong CRC — stub NACKs with CrcMismatch.
    let resp = session
        .send_command(&cmd_flash_verify(0xDEAD_C0DE, 256, 0x0001_0402))
        .await
        .unwrap();
    match resp {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::CrcMismatch),
        other => panic!("expected Nack(CrcMismatch), got {other:?}"),
    }

    tear_down(session, cancel, handle).await;
}

#[tokio::test]
async fn flash_verify_without_session_rejected() {
    let (session, cancel, handle) = spawn_session_with_expected_verify(None).await;
    // No CONNECT — stub session-gates FLASH_VERIFY.
    let resp = session
        .send_command(&cmd_flash_verify(0, 0, 0))
        .await
        .unwrap();
    match resp {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::BadSession),
        other => panic!("expected Nack(BadSession), got {other:?}"),
    }
    tear_down(session, cancel, handle).await;
}

// ---- Firmware loader public API ----

#[test]
fn image_size_crc_and_version_compose_correctly() {
    // Build a 32-byte image filled with 0xAA and sanity-check the
    // trio of values the verify subcommand sends on the wire.
    let data = vec![0xAAu8; 32];
    let crc = crc32(&data);
    let img = Image {
        base_addr: firmware::BL_APP_BASE,
        data: data.clone(),
        fw_info: None,
    };
    assert_eq!(img.size(), 32);
    assert_eq!(img.crc32(), crc);
    assert_eq!(img.packed_version(), 0);
    img.validate_fits_app_region().expect("fits app region");
}

#[test]
fn image_crc_matches_known_reference_vector() {
    // "123456789" is the canonical CRC test vector.
    let data = b"123456789".to_vec();
    assert_eq!(crc32(&data), 0xCBF4_3926);
}
