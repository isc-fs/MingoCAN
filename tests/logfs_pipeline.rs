//! End-to-end integration test for the LOGFS log-extraction pipeline
//! (#506), driven against [`StubDevice`]'s application mode.
//!
//! Why this exists: a cross-repo analysis found **five** byte-level
//! divergences between the merged host and AMS PR #440, none of which
//! any host test could catch — `StubDevice` modelled only the
//! bootloader, which drops APP_CTRL, so no test could reach a single
//! LOGFS opcode. Every one of those five is the kind of bug a loopback
//! test surfaces in seconds and a bench session surfaces in hours.
//!
//! Two groups below:
//!  - the happy path, pinned against what the host currently speaks
//!    ([`LogfsWire::HOST`]);
//!  - the known contract mismatches, pinned against what firmware
//!    currently sends ([`LogfsWire::FIRMWARE_PR440`]). Those assert the
//!    host *fails* — they encode the open questions on #506 as
//!    executable fact, so whoever settles the table gets a red test
//!    telling them exactly what to update.

use std::time::Duration;

use tokio::sync::oneshot;

use can_flasher::firmware::crc32;
use can_flasher::protocol::commands::{
    cmd_logfs_close, cmd_logfs_crc, cmd_logfs_list, cmd_logfs_open, cmd_logfs_read,
};
use can_flasher::protocol::logfs::{self, MAX_READ_LEN};
use can_flasher::protocol::opcodes::NackCode;
use can_flasher::protocol::Response;
use can_flasher::session::{Session, SessionConfig};
use can_flasher::transport::{CanBackend, LogfsWire, StubDevice, StubLogFile, VirtualBus};

const STUB_NODE: u8 = 0x2; // AMS — LOGFS is AMS-only today

/// Three files: one short, one spanning several 512 B reads, one whose
/// length is an exact multiple of the read size (so EOF can only be
/// signalled by a following zero-length read).
fn synthetic_card() -> Vec<StubLogFile> {
    vec![
        StubLogFile::new("LOG0001.CSV", b"ts,v,i\n1,3.7,0.2\n".to_vec(), 111),
        StubLogFile::new("LOG0002.CSV", (0..1300u32).map(|i| i as u8).collect(), 222),
        StubLogFile::new("LOG0003.CSV", vec![0xAB; 1024], 333),
    ]
}

async fn spawn(wire: LogfsWire) -> (Session, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let bus = VirtualBus::new();
    let host = bus.host_backend();
    let device: Box<dyn CanBackend> = Box::new(bus.device_backend());
    drop(bus);

    let stub = StubDevice::new(device, STUB_NODE).with_logfs(synthetic_card(), wire);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let handle = tokio::spawn(async move {
        let _ = stub.run(cancel_rx).await;
    });

    let session = Session::attach(
        Box::new(host),
        SessionConfig {
            target_node: STUB_NODE,
            keepalive_interval: Duration::from_millis(5_000),
            command_timeout: Duration::from_millis(300),
            ..SessionConfig::default()
        },
    );
    (session, cancel_tx, handle)
}

async fn ack(session: &Session, payload: Vec<u8>) -> Vec<u8> {
    match session.send_app_command(&payload).await.expect("app command") {
        Response::Ack { payload, .. } => payload,
        other => panic!("expected ACK, got {other:?}"),
    }
}

/// Walk LIST to completion exactly as the CLI/Studio do.
async fn list_all(session: &Session) -> Vec<logfs::LogEntry> {
    let mut all = Vec::new();
    let mut cursor = 0u16;
    loop {
        let body = ack(session, cmd_logfs_list(cursor)).await;
        let page = logfs::parse_list(&body).expect("parse list page");
        let last = page.is_last();
        let next = page.next_cursor;
        all.extend(page.entries);
        if last {
            break;
        }
        assert_ne!(next, cursor, "cursor must advance");
        cursor = next;
    }
    all
}

// ---- happy path (host-shaped wire) -------------------------------

#[tokio::test]
async fn lists_every_file_across_pages() {
    let (session, cancel, handle) = spawn(LogfsWire::HOST).await;

    // entries_per_page = 2 with 3 files, so this only passes if cursor
    // pagination actually works.
    let entries = list_all(&session).await;
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].name, "LOG0001.CSV");
    assert_eq!(entries[2].name, "LOG0003.CSV");
    assert_eq!(entries[1].size, 1300);
    assert_eq!(entries[0].mtime, 111);

    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn pulls_a_multi_read_file_and_crc_matches() {
    let (session, cancel, handle) = spawn(LogfsWire::HOST).await;
    let entries = list_all(&session).await;
    let target = entries.iter().find(|e| e.name == "LOG0002.CSV").unwrap();

    let body = ack(&session, cmd_logfs_open(target.index)).await;
    let open = logfs::parse_open(&body).expect("parse open");
    assert_eq!(open.size, 1300);
    assert!(!open.crc_deferred(), "host wire carries a sealed crc32");

    // Ranged reads until the short read signals EOF.
    let mut data = Vec::new();
    let mut offset = 0u32;
    let mut round_trips = 0;
    loop {
        let body = ack(
            &session,
            cmd_logfs_read(open.handle, offset, MAX_READ_LEN),
        )
        .await;
        let out = logfs::parse_read(MAX_READ_LEN, &body);
        data.extend_from_slice(&out.data);
        offset += out.data.len() as u32;
        round_trips += 1;
        if out.eof {
            break;
        }
    }
    assert_eq!(round_trips, 3, "1300 B over 512 B reads = 3 round trips");
    assert_eq!(data.len(), 1300);
    assert_eq!(data, (0..1300u32).map(|i| i as u8).collect::<Vec<_>>());

    // OPEN's crc32 and an explicit LOGFS_CRC must agree with the bytes.
    let body = ack(&session, cmd_logfs_crc(open.handle)).await;
    let reported = logfs::parse_crc(&body).expect("parse crc");
    assert_eq!(reported, crc32(&data));
    assert_eq!(open.crc32, reported);

    ack(&session, cmd_logfs_close(open.handle)).await;

    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn exact_multiple_of_read_size_still_terminates() {
    // LOG0003 is exactly 1024 B = 2 full reads, so EOF can only come
    // from a third, zero-length read. A naive "short read" loop that
    // never issues it would hang forever.
    let (session, cancel, handle) = spawn(LogfsWire::HOST).await;
    let entries = list_all(&session).await;
    let target = entries.iter().find(|e| e.name == "LOG0003.CSV").unwrap();
    let open = logfs::parse_open(&ack(&session, cmd_logfs_open(target.index)).await).unwrap();

    let mut data = Vec::new();
    let mut offset = 0u32;
    for _ in 0..8 {
        let body = ack(&session, cmd_logfs_read(open.handle, offset, MAX_READ_LEN)).await;
        let out = logfs::parse_read(MAX_READ_LEN, &body);
        data.extend_from_slice(&out.data);
        offset += out.data.len() as u32;
        if out.eof {
            break;
        }
    }
    assert_eq!(data.len(), 1024);
    assert!(data.iter().all(|&b| b == 0xAB));

    let _ = cancel.send(());
    let _ = handle.await;
}

// ---- error paths --------------------------------------------------

#[tokio::test]
async fn unknown_index_nacks_file_not_found() {
    let (session, cancel, handle) = spawn(LogfsWire::HOST).await;
    match session
        .send_app_command(&cmd_logfs_open(999))
        .await
        .expect("reply")
    {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::FileNotFound),
        other => panic!("expected NACK, got {other:?}"),
    }
    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn stale_handle_nacks_bad_handle() {
    let (session, cancel, handle) = spawn(LogfsWire::HOST).await;
    let entries = list_all(&session).await;
    let open = logfs::parse_open(&ack(&session, cmd_logfs_open(entries[0].index)).await).unwrap();
    ack(&session, cmd_logfs_close(open.handle)).await;

    // Reading after CLOSE must be rejected, and specifically as
    // BAD_HANDLE (0x11) — not BUSY (0x08), which is the collision that
    // started this whole thread.
    match session
        .send_app_command(&cmd_logfs_read(open.handle, 0, MAX_READ_LEN))
        .await
        .expect("reply")
    {
        Response::Nack { code, .. } => {
            assert_eq!(code, NackCode::BadHandle);
            assert_eq!(code.as_byte(), 0x11);
        }
        other => panic!("expected NACK, got {other:?}"),
    }
    let _ = cancel.send(());
    let _ = handle.await;
}

// ---- known contract mismatches (#506) -----------------------------
//
// These pin what AMS PR #440 currently sends. They assert the host
// FAILS, which is the honest state today. When #506 settles, whichever
// side moves, these become the checklist.

#[tokio::test]
async fn mismatch_list_end_sentinel_0xffff_breaks_pagination() {
    let (session, cancel, handle) = spawn(LogfsWire::FIRMWARE_PR440).await;

    // Firmware never emits next_cursor == 0, so the host's is_last()
    // never fires; it follows the 0xFFFF sentinel as if it were a real
    // cursor and the node NACKs it. Result: `cf logs list` fails 100%
    // of the time, even on an empty card.
    // Page 1 of 2 advances normally (next = 2); the sentinel only shows
    // up on the FINAL page, which is where the host's loop breaks.
    let first = logfs::parse_list(&ack(&session, cmd_logfs_list(0)).await).unwrap();
    assert_eq!(first.next_cursor, 2, "mid-walk cursor advances as usual");

    let body = ack(&session, cmd_logfs_list(first.next_cursor)).await;
    let page = logfs::parse_list(&body).expect("page parses fine");
    assert_eq!(page.next_cursor, 0xFFFF, "final page carries the sentinel");
    assert!(
        !page.is_last(),
        "#506 item 3: host treats only 0 as terminal, so 0xFFFF looks \
         like another page"
    );

    let follow = session.send_app_command(&cmd_logfs_list(0xFFFF)).await;
    assert!(
        matches!(follow, Ok(Response::Nack { .. })),
        "following the sentinel gets NACK'd — the visible symptom"
    );

    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn mismatch_open_reply_missing_crc_and_wider_handle() {
    let (session, cancel, handle) = spawn(LogfsWire::FIRMWARE_PR440).await;

    // 6 B [handle:u16][size:u32] vs the host's 9 B expectation. It's
    // long enough not to error, so the host silently mis-parses:
    // #506 items 4 and 5.
    let body = ack(&session, cmd_logfs_open(0)).await;
    assert_eq!(body.len(), 6, "firmware sends 6 B, host expects 9");
    assert!(
        logfs::parse_open(&body).is_err(),
        "#506 items 4+5: host requires 9 B (u8 handle + size + crc32)"
    );

    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn mismatch_app_ctrl_only_dispatcher_drops_cmd_connect() {
    let (session, cancel, handle) = spawn(LogfsWire::FIRMWARE_PR440).await;

    // PR #440's dispatcher answers only APP_CTRL. The host still sends
    // CONNECT as Cmd, so it times out before a single LOGFS opcode goes
    // out — #506 item 1, the first thing anyone would hit on a bench.
    assert!(
        session.connect().await.is_err(),
        "#506 item 1: Cmd-typed CONNECT gets no reply from an \
         APP_CTRL-only dispatcher"
    );

    // APP_CTRL traffic is answered, proving it's the transport and not
    // a dead stub.
    let body = ack(&session, cmd_logfs_list(0)).await;
    assert!(!body.is_empty());

    let _ = cancel.send(());
    let _ = handle.await;
}
