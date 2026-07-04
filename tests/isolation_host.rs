//! End-to-end test of the out-of-process CAN backend bridge.
//!
//! Spawns the REAL `can-flasher` binary as the hidden `__can-host`
//! helper against the in-process `virtual` backend (no hardware), and
//! verifies the stdio protocol: the helper opens the backend, emits the
//! `READY` frame on a clean stdout (proving logging didn't leak into the
//! protocol channel), and exits promptly when the parent closes stdin.
//!
//! The wire protocol + the parent-side `IsolatedBackend` are unit-tested
//! in `src/transport/isolation.rs`; this covers the process boundary.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Framed `READY`: `[u32-LE len = 1][tag = 0x03]`.
const READY_FRAME: [u8; 5] = [1, 0, 0, 0, 0x03];

#[test]
fn can_host_helper_opens_virtual_backend_and_reports_ready() {
    let exe = env!("CARGO_BIN_EXE_can-flasher");
    let mut child = Command::new(exe)
        .args(["--interface", "virtual", "__can-host"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null()) // logging goes here; keep it out of the test
        .spawn()
        .expect("spawn __can-host helper");

    let mut stdout = child.stdout.take().expect("piped stdout");

    // The first bytes on stdout must be exactly the READY frame — if
    // logging (or anything else) leaked onto stdout, this mismatches.
    let mut buf = [0u8; 5];
    stdout
        .read_exact(&mut buf)
        .expect("read READY frame from helper stdout");
    assert_eq!(
        buf, READY_FRAME,
        "helper's first stdout bytes must be the READY frame (stdout must carry only the protocol)"
    );

    // Closing stdin signals shutdown; the helper must exit promptly.
    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                assert!(
                    status.success(),
                    "helper should exit cleanly on stdin EOF, got {status:?}"
                );
                break;
            }
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            None => {
                let _ = child.kill();
                panic!("helper did not exit within 5s of stdin close — teardown wedged");
            }
        }
    }
}
