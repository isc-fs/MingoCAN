//! Integration test for the reboot-to-bootloader poll loop in
//! [`Session::connect_entering_bootloader`].
//!
//! The [`StubDevice`] is configured to model a *running application*
//! that only answers `CMD_CONNECT` after it has received the app-level
//! reboot-to-BL trigger (`REBOOT_TO_BL_ID` + the node's per-node payload).
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

// Comfortably longer than one BL_REBOOT_RETRIGGER_INTERVAL (2.5s) so the
// "dropped first trigger" case, which only re-sends on the second
// trigger, has room. Passing tests return as soon as CONNECT succeeds,
// so the larger cap doesn't slow them.
const WINDOW: Duration = Duration::from_secs(8);

/// Build a session + stub that requires `reboots_needed` reboot triggers
/// before it answers CONNECT. Caller drives the session, then drops
/// `cancel` + awaits `handle` to tear the stub down.
fn setup(reboots_needed: u32) -> (Session, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    setup_node(STUB_NODE, reboots_needed)
}

/// [`setup`] for an arbitrary node id (stub and session both target it).
fn setup_node(
    node: u8,
    reboots_needed: u32,
) -> (Session, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let bus = VirtualBus::new();
    let host = bus.host_backend();
    let device: Box<dyn CanBackend> = Box::new(bus.device_backend());
    drop(bus);

    let mut stub = StubDevice::new(device, node);
    stub.set_reboots_needed(reboots_needed);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let handle = tokio::spawn(async move {
        let _ = stub.run(cancel_rx).await;
    });

    let config = SessionConfig {
        target_node: node,
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

#[tokio::test]
async fn auto_reboots_every_known_node_with_its_own_payload() {
    // ECU 0x1, AMS 0x2, uDV 0x3: the stub only counts a trigger carrying
    // ITS node's payload, so connecting proves the host sent the right one.
    for node in [0x1_u8, 0x2, 0x3] {
        let (session, cancel, handle) = setup_node(node, 1);
        let res = session
            .connect_entering_bootloader(BootloaderEntry::Auto, WINDOW)
            .await;
        assert!(
            res.is_ok(),
            "node 0x{node:X}: expected connect, got {res:?}"
        );
        teardown(session, cancel, handle).await;
    }
}

#[tokio::test]
async fn unknown_node_refuses_to_send_a_trigger() {
    // Node 0x4 has no known payload. Both the auto fallback and an
    // explicit `always` must refuse with NoRebootTrigger — never fall
    // back to another board's payload (the old uDV → AMS bug).
    for entry in [BootloaderEntry::Auto, BootloaderEntry::Always] {
        let (session, cancel, handle) = setup_node(0x4, 1);
        let err = session
            .connect_entering_bootloader(entry, WINDOW)
            .await
            .expect_err("unknown node must not get a reboot trigger");
        assert!(
            matches!(err, SessionError::NoRebootTrigger { node: 0x4 }),
            "{entry:?}: expected NoRebootTrigger, got {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("node 0x4"), "{msg}");
        assert!(msg.contains("--enter-bootloader never"), "{msg}");
        assert!(msg.contains("0x3 uDV (B0 07 AD 13)"), "{msg}");
        assert!(!session.is_connected());
        teardown(session, cancel, handle).await;
    }
}

#[tokio::test]
async fn unknown_node_already_in_bootloader_still_connects() {
    // The refusal only applies when a trigger is needed: an unknown node
    // already sitting in its bootloader answers the auto fast path, and
    // `never` doesn't touch the trigger at all.
    for entry in [BootloaderEntry::Auto, BootloaderEntry::Never] {
        let (session, cancel, handle) = setup_node(0x4, 0);
        let res = session.connect_entering_bootloader(entry, WINDOW).await;
        assert!(res.is_ok(), "{entry:?}: expected connect, got {res:?}");
        teardown(session, cancel, handle).await;
    }
}
