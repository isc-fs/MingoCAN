//! End-to-end integration test for the LOGFS log-extraction pipeline
//! (#506), driven against [`StubDevice`]'s application mode.
//!
//! Why this exists: a cross-repo analysis found **five** byte-level
//! divergences between the host and AMS PR #440, none of which any host
//! test could catch — `StubDevice` modelled only the bootloader, which
//! drops APP_CTRL, so no test could reach a single LOGFS opcode.
//!
//! The contract is settled now (IFS08-CE-AMS#452; firmware on `dev`), so
//! [`LogfsWire::SETTLED`] *is* the wire and these tests are conformance
//! rather than a divergence record: they pin the exact bytes both sides
//! agreed to, so either side drifting shows up here before it shows up
//! on a bench. The five items each have a named test below.
//!
//! Still bench-unproven on both sides — a loopback stub proves the
//! framing, not the microSD card, the FatFs lock, or the CAN wiring.

use std::time::Duration;

use tokio::sync::oneshot;

use can_flasher::firmware::crc32;
use can_flasher::protocol::commands::{
    cmd_logfs_close, cmd_logfs_crc, cmd_logfs_finalize, cmd_logfs_list, cmd_logfs_open,
    cmd_logfs_read,
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
    // LOGFS is session-gated: without this every opcode NACKs BAD_SESSION.
    let version = session.app_connect().await.expect("app CONNECT");
    assert_eq!((version.major, version.minor), (1, 0));
    (session, cancel_tx, handle)
}

async fn ack(session: &Session, payload: Vec<u8>) -> Vec<u8> {
    match session
        .send_app_command(&payload)
        .await
        .expect("app command")
    {
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
    let (session, cancel, handle) = spawn(LogfsWire::SETTLED).await;

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
    let (session, cancel, handle) = spawn(LogfsWire::SETTLED).await;
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
        let body = ack(&session, cmd_logfs_read(open.handle, offset, MAX_READ_LEN)).await;
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
    let (session, cancel, handle) = spawn(LogfsWire::SETTLED).await;
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
    let (session, cancel, handle) = spawn(LogfsWire::SETTLED).await;
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
    let (session, cancel, handle) = spawn(LogfsWire::SETTLED).await;
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

// ---- conformance: the five settled contract items -----------------

#[tokio::test]
async fn item1_connect_rides_app_ctrl() {
    // The firmware dispatcher drops every non-APP_CTRL frame in silence
    // (`if (!req.is_app_ctrl()) return 0;`), so a bootloader-style
    // CONNECT never gets an answer — it times out rather than failing
    // loudly. `spawn` already asserted app_connect() succeeds; this pins
    // that the Cmd-typed one does NOT, so nobody "simplifies" the two
    // back into one call.
    let (session, cancel, handle) = spawn(LogfsWire::SETTLED).await;
    assert!(
        session.connect().await.is_err(),
        "bootloader CONNECT (Cmd) must not be answered by the app dispatcher"
    );
    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn item2_connect_ack_carries_the_app_protocol_version() {
    // Asserted inside spawn() for every test; restated here so the
    // requirement is greppable by name.
    let (session, cancel, handle) = spawn(LogfsWire::SETTLED).await;
    let v = session
        .app_connect()
        .await
        .expect("re-CONNECT is idempotent");
    assert_eq!((v.major, v.minor), (1, 0));
    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn item3_list_terminates_on_the_0xffff_sentinel() {
    let (session, cancel, handle) = spawn(LogfsWire::SETTLED).await;

    // Walking to the end must terminate on 0xFFFF and never emit it as a
    // cursor. list_all() would hang or NACK if either half were wrong.
    let entries = list_all(&session).await;
    assert_eq!(entries.len(), 3);

    // And 0 must NOT be read as terminal — it's the first cursor.
    let first = logfs::parse_list(&ack(&session, cmd_logfs_list(0)).await).unwrap();
    assert!(!first.is_last(), "cursor 0 is the start, not the end");
    assert_ne!(first.next_cursor, 0);

    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn item4_and_5_open_reply_is_ten_bytes_with_u16_handle_and_crc() {
    let (session, cancel, handle) = spawn(LogfsWire::SETTLED).await;
    let entries = list_all(&session).await;

    let body = ack(&session, cmd_logfs_open(entries[1].index)).await;
    assert_eq!(body.len(), 10, "[handle:u16][size:u32][crc32:u32]");
    let open = logfs::parse_open(&body).expect("parse open");
    assert_ne!(open.handle, 0, "0 is the firmware's 'nothing open' marker");
    assert!(!open.crc_deferred(), "sealed crc32 arrives with OPEN");
    assert_eq!(open.size, 1300);

    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn a_node_still_speaking_the_pre_452_wire_is_rejected_not_misread() {
    // The old reply is 6 B where we now need 10. It is *shorter*, so the
    // failure is clean — but the point of the test is that we surface it
    // as an error instead of reading a handle, size and CRC that are all
    // shifted by a byte.
    let (session, cancel, handle) = spawn(LogfsWire::LEGACY_PRE_452).await;
    let body = ack(&session, cmd_logfs_open(0)).await;
    assert!(
        logfs::parse_open(&body).is_err(),
        "a legacy node must fail loudly, not decode to nonsense"
    );
    let _ = cancel.send(());
    let _ = handle.await;
}

// ---- FINALIZE (0x27) ----------------------------------------------

#[tokio::test]
async fn finalize_seals_the_active_log_and_makes_it_listable() {
    // The logger only closes a file on shutdown, so without FINALIZE the
    // run you just did is the one file you cannot pull.
    let bus = VirtualBus::new();
    let host = bus.host_backend();
    let device: Box<dyn CanBackend> = Box::new(bus.device_backend());
    drop(bus);

    let stub = StubDevice::new(device, STUB_NODE)
        .with_logfs(synthetic_card(), LogfsWire::SETTLED)
        .with_active_log(StubLogFile::new(
            "LOG0004.CSV",
            b"still,being,written\n".to_vec(),
            444,
        ));
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
    session.app_connect().await.expect("app CONNECT");

    assert_eq!(
        list_all(&session).await.len(),
        3,
        "active log is not listable"
    );

    let sealed = logfs::parse_finalize(&ack(&session, cmd_logfs_finalize()).await).unwrap();
    assert_eq!(sealed, 3, "sealed log takes the next index");

    let after = list_all(&session).await;
    assert_eq!(after.len(), 4);
    assert_eq!(after[3].name, "LOG0004.CSV");

    // Nothing left to seal — FILE_NOT_FOUND, not a silent success.
    match session
        .send_app_command(&cmd_logfs_finalize())
        .await
        .expect("reply")
    {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::FileNotFound),
        other => panic!("expected NACK, got {other:?}"),
    }

    let _ = cancel_tx.send(());
    let _ = handle.await;
}

// ---- session gating -----------------------------------------------

#[tokio::test]
async fn logfs_before_connect_is_refused() {
    // "anything other than CONNECT without a live session is refused, so
    // a stray or replayed frame cannot stream the card to whoever is on
    // the bus" — diag_dispatch.hpp.
    let bus = VirtualBus::new();
    let host = bus.host_backend();
    let device: Box<dyn CanBackend> = Box::new(bus.device_backend());
    drop(bus);
    let stub = StubDevice::new(device, STUB_NODE).with_logfs(synthetic_card(), LogfsWire::SETTLED);
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

    match session
        .send_app_command(&cmd_logfs_list(0))
        .await
        .expect("reply")
    {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::BadSession),
        other => panic!("expected BAD_SESSION NACK, got {other:?}"),
    }

    let _ = cancel_tx.send(());
    let _ = handle.await;
}
