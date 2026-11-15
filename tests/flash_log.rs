//! Flash log (#651), end to end through the real binary on the virtual
//! bus: a write command appends exactly one line — on failure too — and
//! read-only commands, declined prompts and the virtual bus without
//! `--log` append nothing.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use can_flasher::audit;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_can-flasher")
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mingocan-flashlog-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Same 2-sector image as `flash_subcommand.rs`.
fn write_firmware(path: &Path) {
    let buf: Vec<u8> = (0..2 * 128 * 1024)
        .map(|i| ((i & 0xFF) ^ 0xA5) as u8)
        .collect();
    std::fs::write(path, buf).unwrap();
}

fn run(args: &[&str], extra: &[&Path]) -> Output {
    let mut cmd = Command::new(bin());
    cmd.args(args);
    for p in extra {
        cmd.arg(p);
    }
    cmd.output().expect("spawn can-flasher")
}

#[test]
fn a_successful_flash_appends_one_complete_line() {
    let dir = temp_dir("ok");
    let log = dir.join("flash-log.jsonl");
    let fw = dir.join("app.bin");
    write_firmware(&fw);

    let out = run(
        &[
            "--interface",
            "virtual",
            "--node-id",
            "0x3",
            "--operator",
            "pit-crew",
            "--log",
            log.to_str().unwrap(),
            "flash",
            "--yes",
            "--address",
            "0x08020000",
        ],
        &[&fw],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let entries = audit::read(&log).unwrap();
    assert_eq!(entries.len(), 1, "one line per command");
    let e = &entries[0];
    assert_eq!(e.action, "flash");
    assert_eq!(e.tool, "cli");
    assert!(e.ok && e.error.is_none() && e.exit_code.is_none());
    assert_eq!(e.operator.as_deref(), Some("pit-crew"));
    assert_eq!(e.node_id, Some(0x3));
    assert_eq!(e.role.as_deref(), Some("udv"));
    assert_eq!(e.interface.as_deref(), Some("virtual"));
    assert_eq!(e.size_bytes, Some(2 * 128 * 1024));
    assert!(e.crc32.as_deref().unwrap().starts_with("0x"));
    assert_eq!(e.verified, Some(true));
    assert!(e.artifact.as_deref().unwrap().ends_with("app.bin"));
    assert_eq!(e.details["sectorsWritten"], 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_flash_is_logged_with_its_exit_code() {
    let dir = temp_dir("fail");
    let log = dir.join("flash-log.jsonl");
    let missing = dir.join("nope.elf");

    let out = run(
        &[
            "--interface",
            "virtual",
            "--node-id",
            "0x2",
            "--log",
            log.to_str().unwrap(),
            "flash",
            "--yes",
        ],
        &[&missing],
    );
    assert_eq!(out.status.code(), Some(8));

    let entries = audit::read(&log).unwrap();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert!(!e.ok);
    assert_eq!(e.exit_code, Some(8));
    assert!(e.error.as_deref().unwrap().contains("nope.elf"));
    assert_eq!(e.role.as_deref(), Some("ams"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_only_commands_and_declined_prompts_log_nothing() {
    let dir = temp_dir("readonly");
    let log = dir.join("flash-log.jsonl");
    let log_arg = log.to_str().unwrap();

    // Read-only.
    let out = run(
        &["--interface", "virtual", "--log", log_arg, "discover"],
        &[],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A declined confirmation: stdin is not a TTY, so the prompt
    // auto-declines and nothing is written to the board.
    let out = run(
        &[
            "--interface",
            "virtual",
            "--log",
            log_arg,
            "diagnose",
            "clear-dtc",
        ],
        &[],
    );
    assert!(!out.status.success());

    assert!(!log.exists(), "nothing should have been logged");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clear_dtc_is_logged() {
    let dir = temp_dir("cleardtc");
    let log = dir.join("flash-log.jsonl");
    let out = run(
        &[
            "--interface",
            "virtual",
            "--log",
            log.to_str().unwrap(),
            "diagnose",
            "clear-dtc",
            "--yes",
        ],
        &[],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let entries = audit::read(&log).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].action, "clear-dtc");
    assert_eq!(entries[0].node_id, Some(0x3));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Without `--log`, the virtual bus stays out of the operator's history.
/// Points the per-user data dir at a temp HOME (macOS / Linux resolve it
/// from the environment; Windows uses the shell API, so skip there).
#[cfg(unix)]
#[test]
fn the_virtual_bus_is_not_logged_by_default() {
    let home = temp_dir("home");
    let dir = temp_dir("default");
    let fw = dir.join("app.bin");
    write_firmware(&fw);

    let out = Command::new(bin())
        .env("HOME", &home)
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .args([
            "--interface",
            "virtual",
            "--node-id",
            "0x3",
            "flash",
            "--yes",
            "--address",
            "0x08020000",
        ])
        .arg(&fw)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let found = walk(&home)
        .into_iter()
        .any(|p| p.ends_with(audit::FILE_NAME));
    assert!(!found, "virtual-bus flash must not write the default log");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}
