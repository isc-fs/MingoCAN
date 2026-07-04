//! Integration test for the reboot-to-bootloader poll loop in
//! [`Session::connect_entering_bootloader`].
//!
//! The [`StubDevice`] is configured to model a *running application*
//! that only answers `CMD_CONNECT` after it has received the app-level
//! reboot-to-BL trigger ([`REBOOT_TO_BL_ID`]/[`REBOOT_TO_BL_PAYLOAD`]).
//! This exercises the host's `Auto` path: a first CONNECT probe times
//! out, the host sends the trigger, and — crucially — keeps re-sending +
//! re-probing until the "bootloader" comes up (or the window elapses),
//! rather than the old single-shot "sleep once, retry once".

use std::time::Duration;

use tokio::sync::oneshot;

use can_flasher::app_control::BootloaderEntry;
use can_flasher::protocol::commands::{PROTOCOL_VERSION_MAJOR, PROTOCOL_VERSION_MINOR};
use can_flasher::session::{Session, SessionConfig, SessionError};
use can_flasher::transport::{CanBackend, StubDevice, VirtualBus};

const STUB_NODE: u8 = 0x1; // ECU
const WINDOW: Duration = Duration::from_secs(4);

/// Build a session + stub that requires `reboots_needed` reboot triggers
/// before it answers CONNECT. Caller drives the session, then drops
/// `cancel` + awaits `handle` to tear the stub down.
fn setup(reboots_needed: u32) -> (Session, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let bus = VirtualBus::new();
    let host = bus.host_backend();
    let device: Box<dyn CanBackend> = Box::new(bus.device_backend());
    drop(bus);

    let mut stub = StubDevice::new(device, STUB_NODE);
    stub.set_reboots_needed(reboots_needed);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let handle = tokio::spawn(async move {
        let _ = stub.run(cancel_rx).await;
    });

    let config = SessionConfig {
        target_node: STUB_NODE,
        keepalive_interval: Duration::from_millis(250),
        // Deliberately huge (like the flash erase floor) to prove the
        // reboot poll loop uses its own short per-attempt timeout, not
        // this — otherwise the test would take tens of seconds.
        command_timeout: Duration::from_secs(30),
        host_major: PROTOCOL_VERSION_MAJOR,
        host_minor: PROTOCOL_VERSION_MINOR,
    };
    let session = Session::attach(Box::new(host), config);
    (session, cancel_tx, handle)
}

async fn teardown(
    session: Session,
    cancel: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
) {
    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn auto_reboots_a_running_app_into_the_bootloader() {
    // One trigger needed = a healthy running app that resets on the
    // first reboot frame.
    let (session, cancel, handle) = setup(1);
    let (major, minor) = session
        .connect_entering_bootloader(BootloaderEntry::Auto, WINDOW)
        .await
        .expect("auto entry should reboot the app and connect");
    assert_eq!(
        (major, minor),
        (PROTOCOL_VERSION_MAJOR, PROTOCOL_VERSION_MINOR)
    );
    assert!(session.is_connected());
    teardown(session, cancel, handle).await;
}

#[tokio::test]
async fn auto_survives_a_dropped_first_trigger() {
    // Two triggers needed = the first reboot frame is "lost"; the poll
    // loop must re-send and succeed on a later round. This is exactly
    // the case the old sleep-once/retry-once design failed.
    let (session, cancel, handle) = setup(2);
    let res = session
        .connect_entering_bootloader(BootloaderEntry::Auto, WINDOW)
        .await;
    assert!(
        res.is_ok(),
        "poll loop should re-send the trigger and connect, got {res:?}"
    );
    assert!(session.is_connected());
    teardown(session, cancel, handle).await;
}

#[tokio::test]
async fn auto_gives_up_with_timeout_when_board_never_enters_bl() {
    // A board that never enters the bootloader (unreachable count) must
    // still fail cleanly with a CommandTimeout once the window elapses —
    // not hang forever.
    let (session, cancel, handle) = setup(u32::MAX);
    let short = Duration::from_millis(1_500);
    let err = session
        .connect_entering_bootloader(BootloaderEntry::Auto, short)
        .await
        .expect_err("should time out when the BL never answers");
    assert!(
        matches!(err, SessionError::CommandTimeout { .. }),
        "expected CommandTimeout, got {err:?}"
    );
    assert!(!session.is_connected());
    teardown(session, cancel, handle).await;
}
