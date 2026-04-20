//! Integration tests for the `flash` subcommand, spawned as the
//! real binary via `CARGO_BIN_EXE_can-flasher`.
//!
//! These tests pin the CLI contract: arg parsing, exit codes, JSON
//! output shape, and the happy-path end-to-end flow on `--interface
//! virtual` (which spins up a `StubLoopback` in-process).
//!
//! Engine-internal behaviour (sector rewrites, idempotent diff,
//! CRC-mismatch recovery, timeouts) is already covered by
//! `tests/flash_manager.rs` — this file focuses on the user-facing
//! CLI surface.
//!
//! Scenarios:
//!
//! - End-to-end flash of a 2-sector .bin on the virtual interface
//!   returns exit 0 with a "Flashed" summary.
//! - `--json` produces a well-formed JSON report with the
//!   REQUIREMENTS.md § 8.3 schema (bootloader, firmware, sector
//!   lists, duration_ms).
//! - `--dry-run` exits 0, names the sectors that would have been
//!   written, and doesn't emit a jump line.
//! - A sector-0 binary exits 3 (ProtectionViolation) without
//!   sending any frames.
//! - A raw .bin without `--address` exits 8 (InputFileError).
//! - A non-existent file exits 8 with an I/O-shaped error.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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
        "can-flasher-flash-{tag}-{}-{stamp}.bin",
        std::process::id()
    ));
    p
}

/// Build a deterministic 2-sector (256 KB) firmware image. Byte
/// pattern is `(i & 0xFF) ^ 0xA5` so a stray zero-buffer bug
/// surfaces as a CRC mismatch on readback rather than a silent pass.
fn write_two_sector_bin(path: &std::path::Path) {
    let size = 2 * 128 * 1024;
    let mut buf = Vec::with_capacity(size);
    for i in 0..size {
        buf.push(((i & 0xFF) ^ 0xA5) as u8);
    }
    fs::write(path, buf).unwrap();
}

// ---- End-to-end flash on --interface virtual ----

#[test]
fn flash_cli_writes_and_jumps_on_virtual_interface() {
    let path = temp_bin_path("e2e");
    write_two_sector_bin(&path);

    let out = Command::new(bin())
        .args(["--interface", "virtual", "flash", "--address", "0x08020000"])
        .arg(&path)
        .output()
        .expect("spawn can-flasher");
    let _ = fs::remove_file(&path);

    assert!(
        out.status.success(),
        "flash failed: code={} stderr={}",
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr),
    );

    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("Flashed"), "stdout:\n{stdout}");
    assert!(
        stdout.contains("sectors: erased=[1, 2], written=[1, 2]"),
        "stdout should list erased + written sectors 1 and 2:\n{stdout}"
    );
    assert!(
        stdout.contains("jumped to app at 0x08020000"),
        "expected post-flash jump line:\n{stdout}"
    );
}

// ---- JSON output shape ----

#[test]
fn flash_cli_json_output_matches_requirements_schema() {
    let path = temp_bin_path("json");
    write_two_sector_bin(&path);

    let out = Command::new(bin())
        .args([
            "--interface",
            "virtual",
            "--json",
            "flash",
            "--address",
            "0x08020000",
            "--no-jump",
        ])
        .arg(&path)
        .output()
        .expect("spawn can-flasher");
    let _ = fs::remove_file(&path);

    assert!(
        out.status.success(),
        "flash --json failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).unwrap();
    // `--json` emits one JSON-per-line event stream followed by a
    // single pretty-printed report object at the end. Pull out the
    // final JSON object — the reader is the easiest way.
    let (first_brace, _) = stdout
        .match_indices('{')
        .next_back()
        .expect("stdout must contain at least one JSON object");

    // Find the matching pretty-printed report (the one after the
    // line-events). It's the longest final object; scan from the
    // last `{` that starts on a line boundary.
    let report_start = stdout[..first_brace + 1]
        .rfind("\n{")
        .map(|i| i + 1)
        .unwrap_or(first_brace);
    let report_slice = &stdout[report_start..];

    let parsed: serde_json::Value =
        serde_json::from_str(report_slice.trim()).expect("final report must be valid JSON");

    // REQUIREMENTS.md § 8.3 schema keys.
    for key in [
        "status",
        "firmware",
        "bootloader",
        "sectors_erased",
        "sectors_written",
        "sectors_skipped",
        "duration_ms",
    ] {
        assert!(
            parsed.get(key).is_some(),
            "report missing '{key}' key:\n{parsed}"
        );
    }

    assert_eq!(parsed["status"].as_str(), Some("ok"));
    assert_eq!(parsed["firmware"]["size_bytes"].as_u64(), Some(262_144));
    assert_eq!(
        parsed["sectors_written"].as_array().map(|a| a.len()),
        Some(2),
        "expected 2 sectors written"
    );
    assert_eq!(
        parsed["sectors_erased"].as_array().map(|a| a.len()),
        Some(2),
        "expected 2 sectors erased"
    );
    assert_eq!(
        parsed["bootloader"]["wrp_protected"].as_bool(),
        Some(false),
        "fresh virtual stub has no WRP latched"
    );
}

// ---- Dry-run doesn't erase / write / jump ----

#[test]
fn flash_cli_dry_run_lists_plan_without_jumping() {
    let path = temp_bin_path("dryrun");
    write_two_sector_bin(&path);

    let out = Command::new(bin())
        .args([
            "--interface",
            "virtual",
            "flash",
            "--address",
            "0x08020000",
            "--dry-run",
        ])
        .arg(&path)
        .output()
        .expect("spawn can-flasher");
    let _ = fs::remove_file(&path);

    assert!(
        out.status.success(),
        "flash --dry-run failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("Dry-ran"), "stdout:\n{stdout}");
    assert!(
        stdout.contains("erased=[]"),
        "dry-run must not erase:\n{stdout}"
    );
    assert!(
        stdout.contains("written=[1, 2]"),
        "dry-run plan should still list the sectors:\n{stdout}"
    );
    assert!(
        !stdout.contains("jumped to app"),
        "dry-run must not emit a jump line:\n{stdout}"
    );
}

// ---- Exit-code contract ----

#[test]
fn flash_cli_exits_3_for_binary_at_sector_0() {
    let path = temp_bin_path("sector0");
    write_two_sector_bin(&path);

    let out = Command::new(bin())
        .args(["--interface", "virtual", "flash", "--address", "0x08000000"])
        .arg(&path)
        .output()
        .expect("spawn can-flasher");
    let _ = fs::remove_file(&path);

    assert_eq!(
        out.status.code().unwrap_or(-1),
        3,
        "expected exit 3 (ProtectionViolation); stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("bootloader sector"),
        "stderr should name the violation:\n{stderr}"
    );
}

#[test]
fn flash_cli_exits_8_for_bin_without_address() {
    let path = temp_bin_path("noaddr");
    write_two_sector_bin(&path);

    let out = Command::new(bin())
        .args(["--interface", "virtual", "flash"])
        .arg(&path)
        .output()
        .expect("spawn can-flasher");
    let _ = fs::remove_file(&path);

    assert_eq!(
        out.status.code().unwrap_or(-1),
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

#[test]
fn flash_cli_exits_8_for_missing_firmware_file() {
    // File does not exist — loader surfaces `LoaderError::Io` which
    // classifies as InputFileError (exit 8).
    let out = Command::new(bin())
        .args([
            "--interface",
            "virtual",
            "flash",
            "--address",
            "0x08020000",
            "/does/not/exist/anywhere-on-this-host.bin",
        ])
        .output()
        .expect("spawn can-flasher");

    assert_eq!(
        out.status.code().unwrap_or(-1),
        8,
        "expected exit 8 (InputFileError); stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("could not load firmware"),
        "stderr should explain the load failure:\n{stderr}"
    );
}

// ---- --require-wrp gates on OB_READ ----

#[test]
fn flash_cli_require_wrp_exits_7_when_not_protected() {
    // Virtual stub starts with wrp_sector_mask=0x0. --require-wrp
    // should abort with exit 7 (WrpNotApplied) before any frame
    // enters the flash pipeline.
    let path = temp_bin_path("wrp");
    write_two_sector_bin(&path);

    let out = Command::new(bin())
        .args([
            "--interface",
            "virtual",
            "flash",
            "--address",
            "0x08020000",
            "--require-wrp",
            "--no-jump",
        ])
        .arg(&path)
        .output()
        .expect("spawn can-flasher");
    let _ = fs::remove_file(&path);

    assert_eq!(
        out.status.code().unwrap_or(-1),
        7,
        "expected exit 7 (WrpNotApplied); stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8(out.stdout).unwrap() + &String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("WRP"),
        "output should mention WRP:\n{stderr}"
    );
}

// ---- --no-diff + --no-verify-after parse cleanly ----

#[test]
fn flash_cli_negative_flags_parse_and_succeed() {
    let path = temp_bin_path("neg-flags");
    write_two_sector_bin(&path);

    let out = Command::new(bin())
        .args([
            "--interface",
            "virtual",
            "flash",
            "--address",
            "0x08020000",
            "--no-diff",
            "--no-verify-after",
            "--no-jump",
        ])
        .arg(&path)
        .output()
        .expect("spawn can-flasher");
    let _ = fs::remove_file(&path);

    assert!(
        out.status.success(),
        "negative flags failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    // --no-diff still rewrites every sector; --no-jump means no
    // post-flash jump line. Same "erased + written" shape as the
    // default path.
    assert!(stdout.contains("written=[1, 2]"), "stdout:\n{stdout}");
    assert!(
        !stdout.contains("jumped to app"),
        "--no-jump must suppress the jump line:\n{stdout}"
    );
}
