//! Pull → save → decode, end to end against [`StubDevice`] (#613).
//!
//! The same path `can-flasher logs pull` and the Studio Data logs view take:
//! [`logfs_client::pull_file`] over the virtual bus, then
//! [`log_decode::save_pulled`]. A binary log must land as both the pulled
//! `.BIN` and a decoded `.csv`; a broken one must still be saved; a plain
//! `LOGnnnn.CSV` must come through untouched.

use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::oneshot;

use can_flasher::log_decode;
use can_flasher::logfs_client;
use can_flasher::session::{Session, SessionConfig};
use can_flasher::transport::{CanBackend, LogfsWire, StubDevice, StubLogFile, VirtualBus};

const STUB_NODE: u8 = 0x2;

/// Output of the AMS reference decoder for this file — see the fixtures.
const CEL_BIN: &[u8] = include_bytes!("fixtures/log_decode/CEL0003.BIN");
const CEL_CSV: &str = include_str!("fixtures/log_decode/CEL0003.csv");

/// A binary log whose schema (1 B) disagrees with its header (9 B).
fn corrupt_bin() -> Vec<u8> {
    let mut h = vec![0u8; log_decode::HEADER_LEN];
    h[..8].copy_from_slice(log_decode::MAGIC);
    h[10..12].copy_from_slice(&9u16.to_le_bytes());
    let schema = b"x u8 1 1 -\n";
    h[64..64 + schema.len()].copy_from_slice(schema);
    h.extend([1, 2, 3]);
    h
}

async fn spawn(
    files: Vec<StubLogFile>,
) -> (Session, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let bus = VirtualBus::new();
    let host = bus.host_backend();
    let device: Box<dyn CanBackend> = Box::new(bus.device_backend());
    drop(bus);

    let stub = StubDevice::new(device, STUB_NODE).with_logfs(files, LogfsWire::SETTLED);
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
    (session, cancel_tx, handle)
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mingocan-pull-decode-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test]
async fn pulled_binary_logs_land_with_their_csv_and_csv_logs_are_untouched() {
    let log = b"tick_ms,soc\n1,99\n".to_vec();
    let bad = corrupt_bin();
    let (session, cancel, handle) = spawn(vec![
        StubLogFile::new("LOG0001.CSV", log.clone(), 1),
        StubLogFile::new("CEL0001.BIN", CEL_BIN.to_vec(), 1),
        StubLogFile::new("CEL0002.BIN", bad.clone(), 2),
    ])
    .await;
    let dir = temp_dir("stub");

    // The stub serves files by their position in the list.
    let pull = |index: u16| logfs_client::pull_file(&session, index, true, |_, _| {}, || false);

    // CEL0001.BIN → CEL0001.BIN (as pulled) + CEL0001.csv (decoded).
    let cel = pull(1).await.expect("pull CEL0001.BIN");
    assert!(cel.crc_verified);
    let saved = log_decode::save_pulled(&dir, "CEL0001.BIN", &cel.data).unwrap();
    assert_eq!(std::fs::read(dir.join("CEL0001.BIN")).unwrap(), CEL_BIN);
    let csv = saved.decoded.expect("a binary log").expect("decodes");
    assert_eq!(csv.path, dir.join("CEL0001.csv"));
    assert_eq!(std::fs::read_to_string(&csv.path).unwrap(), CEL_CSV);

    // A corrupted schema: the pull and the save still succeed, the .BIN is
    // kept byte-exact, and no CSV is written.
    let broken = pull(2).await.expect("pull CEL0002.BIN");
    let saved = log_decode::save_pulled(&dir, "CEL0002.BIN", &broken.data)
        .expect("save never fails on decode");
    assert_eq!(std::fs::read(&saved.path).unwrap(), bad);
    assert!(saved.decoded.expect("a binary log").is_err());
    assert!(!dir.join("CEL0002.csv").exists());

    // LOG0001.CSV is saved exactly as pulled, with nothing beside it.
    let plain = pull(0).await.expect("pull LOG0001.CSV");
    let saved = log_decode::save_pulled(&dir, "LOG0001.CSV", &plain.data).unwrap();
    assert!(saved.decoded.is_none());
    assert_eq!(std::fs::read(&saved.path).unwrap(), log);

    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["CEL0001.BIN", "CEL0001.csv", "CEL0002.BIN", "LOG0001.CSV"]
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = cancel.send(());
    let _ = handle.await;
}
