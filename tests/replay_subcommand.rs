//! Integration tests for the `replay` subcommand.
//!
//! `replay record` is a thin wrapper over `CanBackend::recv` and a
//! `BufWriter<File>`; its parser + formatter are already exhaustively
//! covered by the unit tests inside `cli/replay.rs`. This file pins
//! the *shell* of the subcommand by driving the real binary through
//! `CARGO_BIN_EXE_can-flasher`, proving that:
//!
//! - `replay run <file>` parses a canonical candump log and prints a
//!   line per frame plus a trailing `— N frame(s) total` summary.
//! - `replay run --json <file>` emits a well-formed JSON array.
//! - `replay run` rejects a malformed line with a non-zero exit code
//!   and surfaces the offending line number in the error.
//! - `replay record --duration-ms` exits cleanly on its own (no
//!   hang-on-Ctrl-C behaviour in this path) and writes the file
//!   header a downstream `replay run` can read back without error.
//!
//! The tests intentionally do NOT speak to any real adapter — the
//! `--interface virtual` path runs a `StubLoopback` in-process, so
//! the record-mode test is deterministic regardless of host
//! environment.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Path to the binary under test — Cargo injects `CARGO_BIN_EXE_*`
/// automatically for integration test targets.
fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_can-flasher")
}

/// Freshly-named file in the system temp dir. Uses pid + nanos so
/// tests running in parallel don't collide, and the file is cleaned
/// up at the end of each test.
fn temp_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    p.push(format!(
        "can-flasher-replay-{tag}-{}-{stamp}.candump",
        std::process::id()
    ));
    p
}

// ---- replay run: pretty-printed output ----

#[test]
fn replay_run_prints_one_line_per_frame() {
    let path = temp_path("run-plain");
    fs::write(
        &path,
        "# can-flasher replay record — iface=vcan0 bitrate=500000\n\
         (1609459200.123456) vcan0 123#AABBCC\n\
         (1609459200.234567) vcan0 456#0102030405060708\n\
         \n\
         (1609459201.000000) vcan0 003#\n",
    )
    .unwrap();

    let out = Command::new(bin())
        .args(["replay", "run"])
        .arg(&path)
        .output()
        .expect("spawn can-flasher");
    let _ = fs::remove_file(&path);

    assert!(
        out.status.success(),
        "replay run failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();

    // Header comment + blank are skipped; 3 frames survive.
    assert!(stdout.contains("0x123"), "stdout:\n{stdout}");
    assert!(stdout.contains("0x456"), "stdout:\n{stdout}");
    assert!(stdout.contains("0x003"), "stdout:\n{stdout}");
    assert!(
        stdout.contains("AA BB CC"),
        "expected space-separated hex bytes; stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("3 frame(s) total"),
        "expected summary line; stdout:\n{stdout}"
    );
}

// ---- replay run: JSON output ----

#[test]
fn replay_run_emits_well_formed_json_with_json_flag() {
    let path = temp_path("run-json");
    fs::write(
        &path,
        "(42.000001) vcan0 123#DEADBEEF\n(42.000002) vcan0 7FF#00\n",
    )
    .unwrap();

    let out = Command::new(bin())
        .args(["--json", "replay", "run"])
        .arg(&path)
        .output()
        .expect("spawn can-flasher");
    let _ = fs::remove_file(&path);

    assert!(
        out.status.success(),
        "replay run --json failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");
    let arr = parsed.as_array().expect("top-level must be array");
    assert_eq!(arr.len(), 2);

    // First frame: id=0x123, data=DEADBEEF
    assert_eq!(arr[0]["id"].as_str(), Some("0x123"));
    assert_eq!(arr[0]["len"].as_u64(), Some(4));
    assert_eq!(arr[0]["data"].as_str(), Some("DEADBEEF"));
    assert_eq!(arr[0]["interface"].as_str(), Some("vcan0"));

    // Second frame: id=0x7FF (max 11-bit), single byte 0x00
    assert_eq!(arr[1]["id"].as_str(), Some("0x7FF"));
    assert_eq!(arr[1]["len"].as_u64(), Some(1));
    assert_eq!(arr[1]["data"].as_str(), Some("00"));
}

// ---- replay run: malformed line surfaces as a descriptive error ----

#[test]
fn replay_run_rejects_malformed_line_with_line_number() {
    let path = temp_path("run-bad");
    // Line 2 is garbage — should surface with `parsing line 2`.
    fs::write(
        &path,
        "(1.000000) vcan0 123#AABB\n\
         not-a-valid-candump-line\n",
    )
    .unwrap();

    let out = Command::new(bin())
        .args(["replay", "run"])
        .arg(&path)
        .output()
        .expect("spawn can-flasher");
    let _ = fs::remove_file(&path);

    assert!(!out.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("parsing line 2"),
        "stderr should reference line 2:\n{stderr}"
    );
}

// ---- replay record: duration bound + header is readable by run ----

#[test]
fn replay_record_writes_header_and_duration_bound_exits() {
    let path = temp_path("record-virtual");

    // --duration-ms 250: short enough to keep the test snappy, long
    // enough that tokio's signal wiring isn't racing us. Interface is
    // `virtual` so the test is fully in-process.
    let out = Command::new(bin())
        .args([
            "--interface",
            "virtual",
            "replay",
            "record",
            "--duration-ms",
            "250",
            "--out",
        ])
        .arg(&path)
        .output()
        .expect("spawn can-flasher");

    assert!(
        out.status.success(),
        "replay record failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let contents = fs::read_to_string(&path).expect("output file must exist");
    assert!(
        contents.starts_with("# can-flasher replay record"),
        "header comment missing:\n{contents}"
    );

    // Now feed the very file we just produced into `replay run`.
    // Even if the bus was silent the resulting file parses cleanly
    // and prints the "no frames" marker.
    let out2 = Command::new(bin())
        .args(["replay", "run"])
        .arg(&path)
        .output()
        .expect("spawn can-flasher");
    let _ = fs::remove_file(&path);

    assert!(
        out2.status.success(),
        "replay run on recorded file failed: stderr={}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let stdout = String::from_utf8(out2.stdout).unwrap();
    // Either we caught a frame (unlikely on an idle virtual bus) or
    // we printed the "no frames" marker. Both are acceptable outputs
    // — what we assert is that one of them fired.
    assert!(
        stdout.contains("no frames") || stdout.contains("frame(s) total"),
        "expected frame summary marker; stdout:\n{stdout}"
    );
}
