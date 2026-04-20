//! Integration test for the `verify` subcommand's wire path against
//! the stub, plus firmware-loader round-trips and CLI exit-code
//! assertions that depend on the full crate's public API.
//!
//! Happy path and mismatch path both drive `FLASH_VERIFY` through
//! the session layer; the stub's `expected_verify` gate lets the
//! mismatch case fire deterministically.
//!
//! The bottom section spawns the real binary to pin the exit-code
//! contract from REQUIREMENTS.md § Exit codes — feat/15 added the
//! per-segment sector-0 rejection, so malformed linker files now
//! exit 3 (ProtectionViolation) before any frame is sent.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::oneshot;

use can_flasher::cli::ExitCodeHint;
use can_flasher::firmware::{self, crc32, loader, Image, ImageError};
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

// ---- Firmware loader address-space rejection ----

#[test]
fn loader_rejects_bin_at_sector_0_with_protection_violation_hint() {
    // A raw .bin "installed" at flash base overlaps sector 0.
    // `load_bin` runs `validate_segments` internally, so the error
    // comes back wrapped as `Validation(TouchesBootloaderSector…)`
    // and `classify` maps that to `ProtectionViolation` (exit 3).
    let err = loader::load_bin(&[0xAA; 32], 0x0800_0000).unwrap_err();
    assert!(matches!(
        err,
        loader::LoaderError::Validation(ImageError::TouchesBootloaderSector { .. })
    ));
    assert_eq!(loader::classify(&err), ExitCodeHint::ProtectionViolation);
}

#[test]
fn loader_rejects_bin_past_app_region_with_protection_violation_hint() {
    // A .bin that extends past BL_APP_END + 1 would clobber the
    // metadata sector the bootloader writes on successful verify.
    // 0x080D_FFF0 + 0x40 = 0x080E_0030 — past the boundary.
    let err = loader::load_bin(&[0xBB; 0x40], 0x080D_FFF0).unwrap_err();
    assert!(matches!(
        err,
        loader::LoaderError::Validation(ImageError::BeyondAppRegion { .. })
    ));
    assert_eq!(loader::classify(&err), ExitCodeHint::ProtectionViolation);
}

#[test]
fn loader_without_address_hint_classifies_as_input_file_error() {
    // Invoke `load` on a .bin path without --address — top-level
    // dispatch surfaces `BinaryNeedsAddress`, which classifies as
    // InputFileError (exit 8) per REQUIREMENTS.md table.
    let tmp = std::env::temp_dir().join(format!(
        "can-flasher-verify-bin-no-address-{}.bin",
        std::process::id()
    ));
    fs::write(&tmp, [0u8; 16]).unwrap();
    let err = loader::load(&tmp, None).unwrap_err();
    let _ = fs::remove_file(&tmp);
    assert!(matches!(err, loader::LoaderError::BinaryNeedsAddress));
    assert_eq!(loader::classify(&err), ExitCodeHint::InputFileError);
}

#[test]
fn image_sector_range_matches_app_region_layout() {
    // An image based at BL_APP_BASE with exactly BL_SECTOR_SIZE
    // bytes occupies only sector 1. Sanity-check the helper the
    // flash manager will lean on in feat/16.
    let img = Image {
        base_addr: firmware::BL_APP_BASE,
        data: vec![0u8; firmware::BL_SECTOR_SIZE as usize],
        fw_info: None,
    };
    assert_eq!(img.sector_range(), Some(1..=1));

    // A full-size app image spans sectors 1..=6.
    let full = Image {
        base_addr: firmware::BL_APP_BASE,
        data: vec![0u8; firmware::BL_APP_MAX_SIZE as usize],
        fw_info: None,
    };
    assert_eq!(full.sector_range(), Some(1..=6));
}

// ---- CLI exit-code contract (spawns the real binary) ----

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_can-flasher")
}

fn temp_bin_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    p.push(format!(
        "can-flasher-verify-{tag}-{}-{stamp}.bin",
        std::process::id()
    ));
    p
}

#[test]
fn verify_cli_exits_3_for_binary_at_sector_0() {
    // Raw .bin pinned at the bootloader's own sector. `verify`
    // rejects pre-wire with exit 3 (ProtectionViolation) so CI
    // pipelines can distinguish a bad linker file from a verify
    // mismatch (exit 2).
    let path = temp_bin_path("sector0");
    fs::write(&path, [0xAAu8; 32]).unwrap();

    let out = Command::new(bin())
        .args([
            "--interface",
            "virtual",
            "verify",
            "--address",
            "0x08000000",
        ])
        .arg(&path)
        .output()
        .expect("spawn can-flasher");
    let _ = fs::remove_file(&path);

    let code = out.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        3,
        "expected exit 3 (ProtectionViolation); stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("bootloader sector"),
        "stderr should name the bootloader sector:\n{stderr}"
    );
}

#[test]
fn verify_cli_exits_8_for_bin_without_address() {
    // .bin extension + no --address → exit 8 (InputFileError).
    let path = temp_bin_path("noaddr");
    fs::write(&path, [0xBBu8; 16]).unwrap();

    let out = Command::new(bin())
        .args(["--interface", "virtual", "verify"])
        .arg(&path)
        .output()
        .expect("spawn can-flasher");
    let _ = fs::remove_file(&path);

    let code = out.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        8,
        "expected exit 8 (InputFileError); stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("--address"),
        "stderr should mention the missing --address:\n{stderr}"
    );
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
