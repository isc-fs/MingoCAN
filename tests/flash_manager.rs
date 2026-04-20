//! Integration tests for the `FlashManager` engine.
//!
//! Every test runs against an in-process [`StubDevice`] whose flash
//! handlers (`CMD_FLASH_ERASE` / `CMD_FLASH_WRITE` / `CMD_FLASH_READ_CRC`)
//! landed in feat/16 alongside the manager itself. The stub keeps a
//! 768 KB synthetic flash buffer so every scenario is byte-accurate
//! and deterministic — no hardware, no SDK, no platform branches.
//!
//! Scenarios covered:
//!
//! - Full flash into an erased stub: every sector in the image gets
//!   erased + written + verified, the final `FLASH_VERIFY` ACKs.
//! - Idempotent re-flash with diff on: second run skips every sector.
//! - `--no-diff` forces every sector to be rewritten even when it
//!   already matches.
//! - Dry-run performs diff CRC reads but no erase / no write.
//! - CRC mismatch after a successful write surfaces as
//!   `FlashError::SectorCrcMismatch` with `ExitCodeHint::FlashError`.
//! - Transport timeout against an unreachable node bubbles up as
//!   `FlashError::Session(CommandTimeout)` with
//!   `ExitCodeHint::DeviceNotFound`.

use std::time::Duration;

use tokio::sync::oneshot;

use can_flasher::cli::ExitCodeHint;
use can_flasher::firmware::{crc32, Image, BL_APP_BASE, BL_SECTOR_SIZE};
use can_flasher::flash::{FlashConfig, FlashError, FlashManager, SectorRole};
use can_flasher::protocol::commands::cmd_connect_self;
use can_flasher::session::{Session, SessionConfig};
use can_flasher::transport::{CanBackend, StubDevice, VirtualBus};

const STUB_NODE: u8 = 0x3;

// ---- Harness ----

struct Harness {
    session: Session,
    cancel: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

/// Spin up a stub + session pair. `configure` runs on the stub
/// *before* it's spawned so tests can set expected_verify /
/// flash_crc_overrides without racing the dispatch loop.
async fn setup(configure: impl FnOnce(&mut StubDevice)) -> Harness {
    setup_with_target(STUB_NODE, configure).await
}

/// Like `setup` but the session targets a different node ID. Used
/// by the timeout test — pointing the session at a node the stub
/// doesn't answer for means every command times out cleanly.
async fn setup_with_target(target: u8, configure: impl FnOnce(&mut StubDevice)) -> Harness {
    let bus = VirtualBus::new();
    let host = bus.host_backend();
    let device: Box<dyn CanBackend> = Box::new(bus.device_backend());
    drop(bus);

    let mut stub = StubDevice::new(device, STUB_NODE);
    configure(&mut stub);

    let (cancel_tx, cancel_rx) = oneshot::channel();
    let handle = tokio::spawn(async move {
        let _ = stub.run(cancel_rx).await;
    });

    let config = SessionConfig {
        target_node: target,
        keepalive_interval: Duration::from_millis(5_000),
        command_timeout: Duration::from_millis(200),
        ..SessionConfig::default()
    };
    let session = Session::attach(Box::new(host), config);
    Harness {
        session,
        cancel: cancel_tx,
        handle,
    }
}

async fn tear_down(h: Harness) {
    let _ = h.session.disconnect().await;
    let _ = h.cancel.send(());
    let _ = h.handle.await;
}

/// Build an image that exactly fills `sectors` app sectors starting
/// at `BL_APP_BASE`. Byte pattern is a deterministic `(i & 0xFF) ^
/// 0x5A` so an accidental copy-from-buffer bug shows up as a CRC
/// mismatch rather than passing by coincidence.
fn make_image(sectors: u32) -> Image {
    let size = (BL_SECTOR_SIZE * sectors) as usize;
    let mut data = Vec::with_capacity(size);
    for i in 0..size {
        data.push(((i & 0xFF) ^ 0x5A) as u8);
    }
    Image {
        base_addr: BL_APP_BASE,
        data,
        fw_info: None,
    }
}

/// Like `make_image` but with an arbitrary byte count — lets tests
/// cover the partial-sector case where the image ends mid-sector
/// and the flash engine has to `0xFF`-pad the tail for CRC
/// computation (the hardware sees erased flash beyond the image).
fn make_image_exact(bytes: usize) -> Image {
    let mut data = Vec::with_capacity(bytes);
    for i in 0..bytes {
        data.push(((i & 0xFF) ^ 0x5A) as u8);
    }
    Image {
        base_addr: BL_APP_BASE,
        data,
        fw_info: None,
    }
}

// ---- Full flash into an erased stub ----

#[tokio::test]
async fn flash_manager_writes_every_sector_on_empty_device() {
    let h = setup(|_| {}).await;
    h.session.send_command(&cmd_connect_self()).await.unwrap();

    let image = make_image(2);
    let manager = FlashManager::new(&h.session, &image, FlashConfig::default());

    let report = manager.run(None).await.expect("flash succeeds");

    // Image covers sectors 1 and 2 — both freshly written, none
    // skipped (stub started erased so every sector's CRC differed).
    assert_eq!(report.sectors_erased, vec![1, 2]);
    assert_eq!(report.sectors_written, vec![1, 2]);
    assert!(report.sectors_skipped.is_empty());
    assert_eq!(report.size, image.size());
    assert_eq!(report.crc32, image.crc32());

    tear_down(h).await;
}

// ---- Idempotent re-flash (diff on) ----

#[tokio::test]
async fn flash_manager_idempotent_in_diff_mode_second_pass_skips_all() {
    let h = setup(|_| {}).await;
    h.session.send_command(&cmd_connect_self()).await.unwrap();

    let image = make_image(2);

    // First run programmes the flash.
    let first = FlashManager::new(&h.session, &image, FlashConfig::default())
        .run(None)
        .await
        .expect("first flash succeeds");
    assert_eq!(first.sectors_written, vec![1, 2]);

    // Second run against the now-programmed stub — every sector
    // matches, so diff short-circuits. Zero erases, zero writes.
    let second = FlashManager::new(&h.session, &image, FlashConfig::default())
        .run(None)
        .await
        .expect("second flash succeeds");
    assert!(
        second.sectors_erased.is_empty(),
        "idempotent re-flash should erase nothing: erased={:?}",
        second.sectors_erased
    );
    assert!(
        second.sectors_written.is_empty(),
        "idempotent re-flash should write nothing: written={:?}",
        second.sectors_written
    );
    assert_eq!(second.sectors_skipped, vec![1, 2]);

    tear_down(h).await;
}

// ---- --no-diff forces every sector ----

#[tokio::test]
async fn flash_manager_no_diff_rewrites_matching_sectors() {
    let h = setup(|_| {}).await;
    h.session.send_command(&cmd_connect_self()).await.unwrap();

    let image = make_image(1);

    // Seed the stub: flash once so sector 1 already matches.
    FlashManager::new(&h.session, &image, FlashConfig::default())
        .run(None)
        .await
        .unwrap();

    // Now re-flash with --no-diff — even though the sector's CRC
    // already matches the image, the engine must erase and rewrite.
    let config = FlashConfig {
        diff: false,
        ..FlashConfig::default()
    };
    let report = FlashManager::new(&h.session, &image, config)
        .run(None)
        .await
        .expect("no-diff flash succeeds");
    assert_eq!(report.sectors_erased, vec![1]);
    assert_eq!(report.sectors_written, vec![1]);
    assert!(report.sectors_skipped.is_empty());

    tear_down(h).await;
}

// ---- Dry-run ----

#[tokio::test]
async fn flash_manager_dry_run_does_not_touch_device() {
    use can_flasher::protocol::commands::cmd_flash_read_crc;
    use can_flasher::protocol::opcodes::CommandOpcode;
    use can_flasher::protocol::Response;

    let h = setup(|_| {}).await;
    h.session.send_command(&cmd_connect_self()).await.unwrap();

    let image = make_image(1);
    let config = FlashConfig {
        dry_run: true,
        diff: false, // take the write path without the diff read
        ..FlashConfig::default()
    };

    let report = FlashManager::new(&h.session, &image, config)
        .run(None)
        .await
        .expect("dry-run succeeds");

    // Dry-run reports the sectors that would have been written,
    // but no actual erases happened.
    assert!(report.sectors_erased.is_empty());
    assert_eq!(report.sectors_written, vec![1]);

    // Independent check: read sector 1's CRC. Since we didn't
    // actually erase / write anything, the stub is still in its
    // default erased state and sector 1's CRC is CRC of 128 KB of
    // 0xFF, not the image's CRC.
    let erased_crc = crc32(&vec![0xFFu8; BL_SECTOR_SIZE as usize]);
    let image_sector_crc = crc32(&image.data[..BL_SECTOR_SIZE as usize]);
    assert_ne!(
        erased_crc, image_sector_crc,
        "test sanity: erased and image sector must differ"
    );

    let resp = h
        .session
        .send_command(&cmd_flash_read_crc(BL_APP_BASE, BL_SECTOR_SIZE))
        .await
        .unwrap();
    match resp {
        Response::Ack { opcode, payload } => {
            assert_eq!(opcode, CommandOpcode::FlashReadCrc.as_byte());
            let crc = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
            assert_eq!(
                crc, erased_crc,
                "dry-run must not modify flash — expected erased CRC"
            );
        }
        other => panic!("expected ACK, got {other:?}"),
    }

    tear_down(h).await;
}

// ---- Per-sector CRC mismatch after write ----

#[tokio::test]
async fn flash_manager_sector_crc_mismatch_surfaces_as_flash_error() {
    // Stub is configured to lie about sector 1's CRC after any
    // write. The manager's post-write verify reads that lie,
    // compares against the host-computed CRC, and refuses to
    // continue.
    let h = setup(|stub| {
        // Any value not equal to the legitimate sector CRC will
        // do — `0xDEADBEEF` is unmistakably not crc32(image).
        stub.set_flash_crc_override(1, Some(0xDEAD_BEEF));
    })
    .await;
    h.session.send_command(&cmd_connect_self()).await.unwrap();

    let image = make_image(1);
    let config = FlashConfig::default();
    let err = FlashManager::new(&h.session, &image, config)
        .run(None)
        .await
        .expect_err("sector verify must fail");

    match err {
        FlashError::SectorCrcMismatch {
            sector,
            expected,
            got,
        } => {
            assert_eq!(sector, 1);
            assert_eq!(expected, crc32(&image.data[..BL_SECTOR_SIZE as usize]));
            assert_eq!(got, 0xDEAD_BEEF);
        }
        other => panic!("expected SectorCrcMismatch, got {other:?}"),
    }

    // And the exit hint maps to a generic flash error (exit 1) —
    // same bucket as a programming fault on hardware.
    tear_down(h).await;
}

#[tokio::test]
async fn sector_crc_mismatch_exit_hint_is_flash_error() {
    // Sanity check the hint mapping independently of the end-to-end
    // flow — FlashManager surfaces the error, cli::flash (feat/17)
    // will route it through exit_err with this hint.
    let err = FlashError::SectorCrcMismatch {
        sector: 1,
        expected: 0xAAAA_AAAA,
        got: 0xBBBB_BBBB,
    };
    assert_eq!(err.exit_code_hint(), ExitCodeHint::FlashError);
}

// ---- Timeout against a non-listening node ----

#[tokio::test]
async fn flash_manager_timeout_exit_hint_is_device_not_found() {
    // Session targets node 0x5; stub answers on 0x3. Every command
    // times out. The first hop is the plan phase's FLASH_READ_CRC,
    // which surfaces as FlashError::Session(CommandTimeout).
    let h = setup_with_target(0x5, |_| {}).await;

    // Don't even bother connecting — a CONNECT to 0x5 would also
    // time out. Jump straight to FlashManager.
    let image = make_image(1);
    let err = FlashManager::new(&h.session, &image, FlashConfig::default())
        .run(None)
        .await
        .expect_err("no reachable device → must fail");

    // Exit hint maps to DeviceNotFound (exit 4) per REQUIREMENTS.md.
    assert_eq!(err.exit_code_hint(), ExitCodeHint::DeviceNotFound);

    tear_down(h).await;
}

// ---- Planning event is emitted per sector ----

#[tokio::test]
async fn flash_manager_emits_planning_events_via_progress_sink() {
    use tokio::sync::mpsc;

    let h = setup(|_| {}).await;
    h.session.send_command(&cmd_connect_self()).await.unwrap();

    let image = make_image(2);
    let (tx, mut rx) = mpsc::unbounded_channel();
    FlashManager::new(&h.session, &image, FlashConfig::default())
        .run(Some(tx))
        .await
        .expect("flash succeeds");

    // Drain the event stream and count the PlanningSector events.
    // Both sectors should be flagged Write on a fresh stub.
    let mut planning = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let can_flasher::flash::FlashEvent::PlanningSector { sector, role } = event {
            planning.push((sector, role));
        }
    }
    assert_eq!(
        planning,
        vec![(1, SectorRole::Write), (2, SectorRole::Write)]
    );

    tear_down(h).await;
}

// ---- Partial-sector image (regression test for fix/8) ----

/// A real-world firmware rarely fills a flash sector exactly — most
/// Cortex-M apps fit in 50-100 KB, well under the 128 KB sector.
/// The engine has to CRC the sector's image-bytes plus `0xFF`
/// padding out to the sector boundary, matching what the device
/// sees after erase + partial write.
///
/// Pre-fix/8, `expected_sector_crc` called `sector_slice` which
/// `unreachable!()`-panicked when the requested slice extended past
/// `image.data.len()`. This test reproduces that path with a 50 KB
/// image — well short of a single sector's 128 KB.
#[tokio::test]
async fn flash_manager_handles_partial_sector_image() {
    let h = setup(|_| {}).await;
    h.session.send_command(&cmd_connect_self()).await.unwrap();

    // 50 KB image — lands partway into sector 1. The engine must
    // pad the remaining ~78 KB with `0xFF` when computing the
    // sector's expected CRC.
    let image = make_image_exact(50 * 1024);
    let manager = FlashManager::new(&h.session, &image, FlashConfig::default());

    let report = manager
        .run(None)
        .await
        .expect("partial-sector flash succeeds");

    // Only sector 1 touched; it was fully rewritten (stub started
    // erased so diff saw a mismatch regardless of padding).
    assert_eq!(report.sectors_erased, vec![1]);
    assert_eq!(report.sectors_written, vec![1]);
    assert!(report.sectors_skipped.is_empty());
    assert_eq!(report.size, 50 * 1024);

    tear_down(h).await;
}
