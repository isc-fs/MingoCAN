//! Data-logs (LOGFS) commands — scan and pull the microSD car-data logs
//! off a node over CAN (#506, firmware spec IFS08-CE-AMS#406).
//!
//! Rides the existing CONNECT session + ISO-TP via the shared
//! `can_flasher` protocol layer; nothing here re-implements the wire
//! format. Read-only — the card has no delete.
//!
//! Because nothing can be removed from the card, this laptop keeps a
//! **download ledger** (`logs-ledger.jsonl` in the app-data dir): one
//! JSON line per verified download, hide, unhide or forget. [`logs_scan`]
//! joins it with the card listing so the UI can fold away what is already
//! safely on disk. A file only counts as downloaded while the saved copy
//! still exists at the listed size — the ledger can never hide a log that
//! isn't really here.
//!
//! `logs_pull` streams progress to the frontend over
//! [`EVENT_NAME`] so the UI can show a bar + ETA on what is, at classic
//! CAN speeds, a minutes-long transfer.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use can_flasher::firmware::crc32;
use can_flasher::log_decode;
use can_flasher::logfs_client;
use can_flasher::protocol::commands::{cmd_logfs_close, cmd_logfs_list, cmd_logfs_open};
use can_flasher::protocol::logfs;
use can_flasher::protocol::Response;
use can_flasher::session::{Session, SessionConfig, SessionError};
use can_flasher::transport::open_backend;

use crate::flash::parse_interface;

/// Progress events for an in-flight `logs_pull`.
pub const EVENT_NAME: &str = "logs://progress";

/// Marker the frontend matches to render a cancel as a neutral outcome
/// rather than a failure.
pub const CANCELLED_MSG: &str = "cancelled by operator";

/// Marker the frontend matches to re-scan instead of showing an error:
/// the index it asked for no longer holds the file it listed (the AMS
/// rebooted and sealed a new file, or the card was swapped).
pub const CARD_CHANGED_MSG: &str = "the card changed since it was listed";

/// Ledger file name, inside the app-data dir. Per laptop, never synced.
const LEDGER_FILE: &str = "logs-ledger.jsonl";

/// Serialises ledger appends: a hide can land while a pull is finishing.
static LEDGER_LOCK: Mutex<()> = Mutex::new(());

/// How many already-downloaded files [`logs_scan`] re-OPENs to make sure
/// the card is still the one they came from. A reformatted card restarts
/// at LOG0000 and full files are all ~4 MiB, so name + size alone would
/// fold brand-new logs away as "downloaded".
const SPOT_CHECKS: usize = 3;

/// Set by [`logs_cancel`], polled between reads by [`logs_pull`]. A pull
/// can run 3-7 minutes at classic-CAN speeds, so aborting has to be
/// possible without tearing down the app.
static CANCEL_PULL: AtomicBool = AtomicBool::new(false);

/// Held for the duration of any LOGFS operation. There is one CAN
/// adapter, and a pull holds it for minutes — so a second command
/// (another pull, or a List click on a stale window) must be told no
/// rather than race for the device and fail with an opaque
/// adapter-in-use error from the driver.
static LOGS_BUSY: AtomicBool = AtomicBool::new(false);

/// Floor for the LOGFS command timeout, regardless of the operator's
/// adapter setting. The app default is sized for bootloader commands that
/// answer out of RAM; a LOGFS round trip additionally waits on a FatFs
/// read from a microSD card behind a shared lock, so it can legitimately
/// take over a second — and a spurious timeout mid-pull throws away
/// minutes of transfer. A higher setting is still honoured.
const LOGFS_TIMEOUT_FLOOR_MS: u32 = 2_000;

/// How many times to re-send an idempotent LOGFS command before failing.
const LOGFS_RETRY_ATTEMPTS: u32 = 3;

/// Linear backoff base — attempt N waits `N * this`.
const LOGFS_RETRY_BACKOFF_MS: u64 = 60;

/// RAII holder for [`LOGS_BUSY`], so the flag clears on every exit path
/// including the `?` ones.
struct BusyGuard;

impl BusyGuard {
    fn acquire() -> Result<Self, String> {
        if LOGS_BUSY.swap(true, Ordering::SeqCst) {
            return Err("another log transfer is already running on this \
                        adapter — wait for it to finish or cancel it"
                .to_string());
        }
        Ok(Self)
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        LOGS_BUSY.store(false, Ordering::SeqCst);
    }
}

/// A failed round trip, plus whether re-sending it could plausibly help.
struct AckError {
    message: String,
    retryable: bool,
}

impl AckError {
    fn fatal(message: String) -> Self {
        Self {
            message,
            retryable: false,
        }
    }
}

// Lets every existing `?` in a `Result<_, String>` function keep working.
impl From<AckError> for String {
    fn from(e: AckError) -> Self {
        e.message
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsRequest {
    pub interface: String,
    pub channel: Option<String>,
    pub bitrate: u32,
    pub node_id: Option<u8>,
    pub timeout_ms: u32,
}

/// Emitted repeatedly during a pull.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullProgress {
    pub index: u16,
    pub name: String,
    pub received: u32,
    pub total: u32,
}

/// Returned when a pull completes.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullResult {
    pub path: String,
    pub bytes: u32,
    /// `true` when the node's CRC matched the bytes we received.
    pub crc_verified: bool,
    /// The file is saved, but recording it in the ledger failed — it will
    /// show as new on the next scan. `None` when all is well.
    pub ledger_error: Option<String>,
    /// For a binary log (`IMUnnnn.BIN`, `CELnnnn.BIN`): the CSV decoded
    /// beside it (#613). `None` for a file that isn't one, or if decoding
    /// failed — see `decode_error`.
    pub decoded: Option<DecodedCsv>,
    /// Why a binary log couldn't be decoded. The pulled file is still saved.
    pub decode_error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecodedCsv {
    pub path: String,
    pub records: usize,
    /// Bytes of a partial last record that were dropped (power cut mid-write).
    pub torn_bytes: usize,
}

/// Identifies one card file across scans: LOGFS indices are reused after
/// a reformat, so the name and size travel with it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileKey {
    pub node: u8,
    pub name: String,
    pub size: u32,
}

/// One line of the download ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum LedgerEvent {
    /// Written only after the CRC-gated bytes were renamed into place.
    Pulled {
        #[serde(flatten)]
        key: FileKey,
        crc32: u32,
        path: String,
        at: u64,
    },
    Hide {
        #[serde(flatten)]
        key: FileKey,
        at: u64,
    },
    Unhide {
        #[serde(flatten)]
        key: FileKey,
        at: u64,
    },
    /// Drop the download records for a file (its copy is gone for good).
    Forget {
        #[serde(flatten)]
        key: FileKey,
        at: u64,
    },
}

/// What the ledger knows about one file once every event is replayed.
#[derive(Debug, Default, Clone, PartialEq)]
struct KeyState {
    /// `(crc32, path, at)` for each download still on record, oldest first.
    pulled: Vec<(u32, String, u64)>,
    hidden: bool,
}

/// Where a card file stands on this laptop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FileStatus {
    /// Not downloaded here (or its download can't be confirmed).
    New,
    /// A verified copy exists on disk at the listed size.
    Downloaded,
    /// Downloaded before, but no copy is on disk any more.
    Missing,
    /// Dismissed by hand. Never implies it was downloaded.
    Hidden,
}

/// One card file plus its status on this laptop.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScannedFile {
    pub index: u16,
    pub name: String,
    pub size: u32,
    pub status: FileStatus,
    /// The copy on disk (Downloaded) or where it used to be (Missing).
    pub path: Option<String>,
    /// Unix seconds of that download.
    pub pulled_at: Option<u64>,
    /// For a downloaded binary log: its decoded CSV, if it sits beside the
    /// copy (what "Show" should open).
    pub csv_path: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    /// In card (directory) order; the UI sorts.
    pub files: Vec<ScannedFile>,
    /// A re-OPENed file's CRC disagreed with the one recorded at download:
    /// the card was reformatted or swapped, so the ledger is ignored and
    /// everything is reported as new.
    pub card_changed: bool,
    /// The ledger couldn't be read; everything is reported as new.
    pub ledger_error: Option<String>,
}

fn open_session(request: &LogsRequest) -> Result<Session, String> {
    // Never guess the target board (FMEA #271 G2, same rule as flash).
    // The old default of 0x3 is uDV — a real board, and not the log
    // source — which also made the bootloader probe answer and mislead
    // the operator into reflashing the wrong ECU.
    let target_node = request.node_id.ok_or_else(|| {
        "no node id selected: pick the target board (the microSD log \
         service is AMS-only today) before listing or pulling logs"
            .to_string()
    })?;
    let interface = parse_interface(&request.interface)?;
    let backend = open_backend(interface, request.channel.as_deref(), request.bitrate)
        .map_err(|e| format!("open backend: {e}"))?;
    Ok(Session::attach(
        backend,
        SessionConfig {
            // Caller-supplied — nothing hardcodes the AMS address, so the
            // pending 0x01 -> 0x02 move (IFS08-CE-AMS#403) is a settings change.
            target_node,
            keepalive_interval: Duration::from_millis(5_000),
            command_timeout: Duration::from_millis(u64::from(
                request.timeout_ms.max(LOGFS_TIMEOUT_FLOOR_MS),
            )),
            ..SessionConfig::default()
        },
    ))
}

/// Send one LOGFS command, unwrapping the ACK body (opcode already
/// stripped by the response parser).
async fn ack_body(session: &Session, payload: Vec<u8>, what: &str) -> Result<Vec<u8>, AckError> {
    // Remember what we asked for: the ACK echoes the opcode back, and
    // checking the echo catches a reply belonging to a *different*
    // command (a stale one that landed late, or a dispatcher that ran the
    // wrong handler). Unchecked, those bytes get parsed as this command's
    // body and turn into silent nonsense.
    let expected_opcode = payload.first().copied();

    // LOGFS rides APP_CTRL (0x06), not CMD (0x00) — see IFS08-CE-AMS#406.
    // The bootloader silently drops APP_CTRL, so a timeout is ambiguous:
    // probe with a command the BL answers to tell "in bootloader" apart
    // from "dead node / wrong id".
    let reply = match session.send_app_command(&payload).await {
        Err(SessionError::CommandTimeout { .. }) if session.probe_bootloader().await => {
            return Err(AckError::fatal(format!(
                "no reply to {what}: the node is alive but running the bootloader, \
                 where the log service isn't available — boot the application firmware"
            )))
        }
        // Only transport-level failures are worth another go. A NACK is a
        // considered answer and a bootloader diagnosis is a persistent
        // state; re-sending either hides the real message.
        Err(e) => {
            let retryable = matches!(
                e,
                SessionError::CommandTimeout { .. } | SessionError::Transport(_)
            );
            return Err(AckError {
                message: format!("send {what}: {e}"),
                retryable,
            });
        }
        Ok(reply) => reply,
    };
    match reply {
        Response::Ack { opcode, payload } => match expected_opcode {
            Some(want) if opcode != want => Err(AckError::fatal(format!(
                "reply to {what} echoes opcode 0x{opcode:02X}, expected 0x{want:02X} \
                 — replies are out of step with requests"
            ))),
            _ => Ok(payload),
        },
        Response::Nack {
            rejected_opcode,
            code,
        } => Err(AckError::fatal(format!(
            "device NACK'd {what} (opcode 0x{rejected_opcode:02X}): {code}"
        ))),
        other => Err(AckError::fatal(format!(
            "unexpected reply to {what}: {}",
            other.kind_str()
        ))),
    }
}

/// [`ack_body`] with retries, for **idempotent** opcodes only.
///
/// LOGFS reads are ranged and stateless firmware-side, so re-requesting
/// the same window is safe; LIST is a pure read of a cursor page. A
/// multi-MB pull is thousands of round trips over several minutes, and
/// without this one blip on a shared bus discards the whole transfer.
/// Deliberately *not* used for OPEN (allocates a handle) or CLOSE (frees
/// one).
async fn ack_body_retrying(
    session: &Session,
    payload: Vec<u8>,
    what: &str,
) -> Result<Vec<u8>, AckError> {
    let mut attempt = 1u32;
    loop {
        match ack_body(session, payload.clone(), what).await {
            Ok(body) => return Ok(body),
            Err(e) if attempt < LOGFS_RETRY_ATTEMPTS && e.retryable => {
                tokio::time::sleep(Duration::from_millis(
                    LOGFS_RETRY_BACKOFF_MS * u64::from(attempt),
                ))
                .await;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Walk `LOGFS_LIST` to completion, following the cursor.
async fn list_all(session: &Session) -> Result<Vec<logfs::LogEntry>, String> {
    let mut all = Vec::new();
    let mut cursor = 0u16;
    loop {
        let body = ack_body_retrying(session, cmd_logfs_list(cursor), "LOGFS_LIST").await?;
        let page = logfs::parse_list(&body).map_err(|e| format!("parse LOGFS_LIST: {e}"))?;
        let is_last = page.is_last();
        let next = page.next_cursor;
        all.extend(page.entries);
        if is_last {
            break;
        }
        if next == cursor {
            return Err(format!("LOGFS_LIST cursor stuck at {cursor}"));
        }
        cursor = next;
    }
    Ok(all)
}

/// Where downloads go when the operator hasn't picked a folder:
/// `<Documents>/MingoCAN Logs`. Documents rather than Downloads, which
/// cleanup tools like to sweep.
#[tauri::command]
pub fn logs_default_root(app: AppHandle) -> Result<String, String> {
    let docs = app
        .path()
        .document_dir()
        .map_err(|e| format!("no Documents folder: {e}"))?;
    Ok(docs.join("MingoCAN Logs").display().to_string())
}

fn ledger_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no app-data folder: {e}"))?;
    Ok(dir.join(LEDGER_FILE))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Parse the ledger text. A line that doesn't parse (a torn write, a
/// newer event kind) is skipped rather than failing the whole scan: losing
/// one record can only make a file show as new, never hide one.
fn parse_ledger(text: &str) -> Vec<LedgerEvent> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Read the whole ledger. A missing file is an empty ledger.
fn read_ledger(app: &AppHandle) -> Result<Vec<LedgerEvent>, String> {
    let path = ledger_path(app)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(parse_ledger(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

fn append_ledger(app: &AppHandle, events: &[LedgerEvent]) -> Result<(), String> {
    let path = ledger_path(app)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let mut text = String::new();
    for ev in events {
        let line = serde_json::to_string(ev).map_err(|e| format!("encode ledger event: {e}"))?;
        text.push_str(&line);
        text.push('\n');
    }
    let _lock = LEDGER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    // One write per batch, so a crash can tear at most the last line —
    // which `parse_ledger` then skips.
    f.write_all(text.as_bytes())
        .and_then(|_| f.sync_all())
        .map_err(|e| format!("write {}: {e}", path.display()))
}

/// Replay the ledger into per-file state.
fn fold_ledger(events: &[LedgerEvent]) -> HashMap<FileKey, KeyState> {
    let mut map: HashMap<FileKey, KeyState> = HashMap::new();
    for ev in events {
        match ev {
            LedgerEvent::Pulled {
                key,
                crc32,
                path,
                at,
            } => {
                map.entry(key.clone())
                    .or_default()
                    .pulled
                    .push((*crc32, path.clone(), *at));
            }
            LedgerEvent::Hide { key, .. } => map.entry(key.clone()).or_default().hidden = true,
            LedgerEvent::Unhide { key, .. } => map.entry(key.clone()).or_default().hidden = false,
            LedgerEvent::Forget { key, .. } => map.entry(key.clone()).or_default().pulled.clear(),
        }
    }
    map
}

/// Decide one file's status. `on_disk(path, size)` says whether a copy
/// exists there at exactly that size — the only proof that counts.
fn classify(
    key: &FileKey,
    state: Option<&KeyState>,
    on_disk: &dyn Fn(&str, u32) -> bool,
) -> (FileStatus, Option<String>, Option<u64>) {
    let Some(state) = state else {
        return (FileStatus::New, None, None);
    };
    // The newest copy that is still there wins; otherwise point at the
    // newest place it used to be.
    let present = state
        .pulled
        .iter()
        .rev()
        .find(|(_, path, _)| on_disk(path, key.size));
    let (status, record) = match (present, state.pulled.last()) {
        (Some(rec), _) => (FileStatus::Downloaded, Some(rec)),
        (None, Some(rec)) => (FileStatus::Missing, Some(rec)),
        (None, None) => (FileStatus::New, None),
    };
    // A hide is the operator's call and applies to any of the above.
    let status = if state.hidden {
        FileStatus::Hidden
    } else {
        status
    };
    (
        status,
        record.map(|(_, path, _)| path.clone()),
        record.map(|(_, _, at)| *at),
    )
}

/// `<stem>.csv` next to a binary log, if it exists — the decoded copy.
fn csv_beside(path: &Path) -> Option<String> {
    let is_bin = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("bin"));
    if !is_bin {
        return None;
    }
    let csv = path.with_extension("csv");
    csv.is_file().then(|| csv.display().to_string())
}

fn file_on_disk(path: &str, size: u32) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.len() == u64::from(size))
        .unwrap_or(false)
}

/// Pick which downloaded files to re-OPEN: the lowest, highest and middle
/// index, so both an old and a recent part of the card are covered.
fn pick_spot_checks(mut indices: Vec<u16>) -> Vec<u16> {
    indices.sort_unstable();
    indices.dedup();
    if indices.len() <= SPOT_CHECKS {
        return indices;
    }
    let mut picked = vec![
        indices[0],
        indices[indices.len() / 2],
        indices[indices.len() - 1],
    ];
    picked.dedup();
    picked
}

/// OPEN + CLOSE one file to read its sealed CRC. `Ok(None)` when the node
/// has no sealed CRC for it (a file from before CRC sidecars existed).
async fn sealed_crc(session: &Session, index: u16) -> Result<Option<u32>, String> {
    let body = ack_body(session, cmd_logfs_open(index), "LOGFS_OPEN").await?;
    let opened = logfs::parse_open(&body).map_err(|e| format!("parse LOGFS_OPEN: {e}"))?;
    let _ = ack_body(session, cmd_logfs_close(opened.handle), "LOGFS_CLOSE").await;
    Ok((!opened.crc_deferred()).then_some(opened.crc32))
}

/// List the card and say, for each file, whether this laptop already has
/// it. Also re-OPENs up to [`SPOT_CHECKS`] downloaded files in the same
/// session, so a swapped or reformatted card is caught *before* anything
/// is folded away.
#[tauri::command]
pub async fn logs_scan(app: AppHandle, request: LogsRequest) -> Result<ScanResult, String> {
    let _busy = BusyGuard::acquire()?;
    // `open_session` refuses a missing node id, so this default is never used.
    let node = request.node_id.unwrap_or(0);
    let (events, ledger_error) = match read_ledger(&app) {
        Ok(ev) => (ev, None),
        Err(e) => (Vec::new(), Some(e)),
    };
    let ledger = fold_ledger(&events);

    let session = open_session(&request)?;
    session
        .app_connect()
        .await
        .map_err(|e| format!("app CONNECT before LOGFS_LIST: {e}"))?;
    let listed = list_all(&session).await;
    let entries = match listed {
        Ok(entries) => entries,
        Err(e) => {
            let _ = session.app_disconnect().await;
            return Err(e);
        }
    };

    let key_of = |e: &logfs::LogEntry| FileKey {
        node,
        name: e.name.clone(),
        size: e.size,
    };
    let candidates: Vec<u16> = entries
        .iter()
        .filter(|e| ledger.get(&key_of(e)).is_some_and(|s| !s.pulled.is_empty()))
        .map(|e| e.index)
        .collect();
    let mut card_changed = false;
    for index in pick_spot_checks(candidates) {
        let Some(entry) = entries.iter().find(|e| e.index == index) else {
            continue;
        };
        match sealed_crc(&session, index).await {
            Ok(Some(crc)) => {
                let known = &ledger[&key_of(entry)].pulled;
                if !known.iter().any(|(c, _, _)| *c == crc) {
                    card_changed = true;
                    break;
                }
            }
            // No sealed CRC: nothing to compare, name + size will have to do.
            Ok(None) => {}
            // A check that can't run proves nothing either way; the listing
            // itself is still good.
            Err(e) => {
                tracing::warn!("LOGFS spot check of index {index} failed: {e}");
                break;
            }
        }
    }
    let _ = session.app_disconnect().await;

    let empty = HashMap::new();
    let ledger = if card_changed { &empty } else { &ledger };
    let files = entries
        .into_iter()
        .map(|e| {
            let key = key_of(&e);
            let (status, path, pulled_at) = classify(&key, ledger.get(&key), &file_on_disk);
            let csv_path = match (status, &path) {
                (FileStatus::Downloaded, Some(p)) => csv_beside(Path::new(p)),
                _ => None,
            };
            ScannedFile {
                index: e.index,
                name: e.name,
                size: e.size,
                status,
                path,
                pulled_at,
                csv_path,
            }
        })
        .collect();
    Ok(ScanResult {
        files,
        card_changed,
        ledger_error,
    })
}

/// What [`logs_mark`] records.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MarkAction {
    Hide,
    Unhide,
    Forget,
}

/// Hide, unhide or forget files in this laptop's ledger. Never touches the
/// card or any file on disk.
#[tauri::command]
pub fn logs_mark(app: AppHandle, action: MarkAction, files: Vec<FileKey>) -> Result<(), String> {
    let at = now_secs();
    let events: Vec<LedgerEvent> = files
        .into_iter()
        .map(|key| match action {
            MarkAction::Hide => LedgerEvent::Hide { key, at },
            MarkAction::Unhide => LedgerEvent::Unhide { key, at },
            MarkAction::Forget => LedgerEvent::Forget { key, at },
        })
        .collect();
    if events.is_empty() {
        return Ok(());
    }
    append_ledger(&app, &events)
}

/// Make sure `dir` exists and takes writes, before minutes are spent on
/// the bus: an unplugged USB disk or a read-only folder fails here in
/// milliseconds instead of after the transfer.
fn preflight_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    let probe = dir.join(".mingocan-write-test");
    std::fs::write(&probe, b"ok").map_err(|e| format!("can't write to {}: {e}", dir.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

#[tauri::command]
pub async fn logs_pull(
    app: AppHandle,
    request: LogsRequest,
    index: u16,
    expect_name: String,
    expect_size: u32,
    dest_dir: String,
) -> Result<PullResult, String> {
    let _busy = BusyGuard::acquire()?;
    CANCEL_PULL.store(false, Ordering::Relaxed);
    let dir = PathBuf::from(&dest_dir);
    preflight_dir(&dir)?;
    let session = open_session(&request)?;
    session
        .app_connect()
        .await
        .map_err(|e| format!("app CONNECT before LOGFS pull: {e}"))?;

    let expected = FileKey {
        node: request.node_id.unwrap_or(0),
        name: expect_name,
        size: expect_size,
    };
    let result = pull_inner(&app, &session, index, &expected, &dir).await;
    let _ = session.app_disconnect().await;
    result
}

async fn pull_inner(
    app: &AppHandle,
    session: &Session,
    index: u16,
    expected: &FileKey,
    dir: &Path,
) -> Result<PullResult, String> {
    // Re-list so the index still means the file the operator clicked:
    // indices are only stable while the card is untouched.
    let entries = list_all(session).await?;
    let entry = entries
        .into_iter()
        .find(|e| e.index == index)
        .ok_or_else(|| format!("{CARD_CHANGED_MSG}: no log with index {index} any more"))?;
    if entry.name != expected.name || entry.size != expected.size {
        return Err(format!(
            "{CARD_CHANGED_MSG}: index {index} is now {} ({} B), not {} ({} B)",
            entry.name, entry.size, expected.name, expected.size
        ));
    }

    // Orchestration (OPEN → READ with transport retry + mid-pull session
    // recovery → CRC → CLOSE) lives in the shared client, so the CLI and
    // this view harden identically. We supply progress + cancel.
    let name = entry.name.clone();
    let on_progress = |received: u32, total: u32| {
        let _ = app.emit(
            EVENT_NAME,
            PullProgress {
                index,
                name: name.clone(),
                received,
                total,
            },
        );
    };
    let is_cancelled = || CANCEL_PULL.load(Ordering::Relaxed);

    let pulled = logfs_client::pull_file(session, index, true, on_progress, is_cancelled)
        .await
        .map_err(|e| match e {
            // Surface a cancel with the exact marker the frontend matches
            // so it renders as a neutral outcome, not a red error.
            logfs_client::PullError::Cancelled => CANCELLED_MSG.to_string(),
            logfs_client::PullError::Failed(msg) => format!("{}: {msg}", entry.name),
        })?;

    let path = log_decode::save_file(dir, &entry.name, &pulled.data)
        .map_err(|e| format!("write {} into {}: {e}", entry.name, dir.display()))?;

    // Only now, with the bytes CRC-checked and renamed into place, does
    // the file count as downloaded. Recorded *before* decoding, so a crash
    // mid-decode can't leave a saved file looking new (and the next pull
    // making a `_2` copy).
    let ledger_error = append_ledger(
        app,
        &[LedgerEvent::Pulled {
            key: expected.clone(),
            crc32: crc32(&pulled.data),
            path: path.display().to_string(),
            at: now_secs(),
        }],
    )
    .err();

    // A binary log also gets its CSV beside it (#613). CPU-bound for a few
    // MiB, so off the async executor; it can't fail the pull.
    let bytes = pulled.data.len() as u32;
    let crc_verified = pulled.crc_verified;
    let decode_path = path.clone();
    let data = pulled.data;
    let decoded =
        tokio::task::spawn_blocking(move || log_decode::decode_beside(&decode_path, &data)).await;
    let (decoded, decode_error) = match decoded {
        Ok(None) => (None, None),
        Ok(Some(Ok(d))) => (
            Some(DecodedCsv {
                path: d.path.display().to_string(),
                records: d.records,
                torn_bytes: d.torn_bytes,
            }),
            None,
        ),
        Ok(Some(Err(e))) => {
            tracing::warn!("{}: not decoded: {e}", path.display());
            (None, Some(e))
        }
        Err(e) => (None, Some(format!("decoder stopped: {e}"))),
    };

    Ok(PullResult {
        path: path.display().to_string(),
        bytes,
        crc_verified,
        ledger_error,
        decoded,
        decode_error,
    })
}

/// Show a file (selected) or a folder in the OS file manager.
#[tauri::command]
pub fn logs_reveal(path: String) -> Result<(), String> {
    let p = Path::new(&path);
    if !p.exists() {
        return Err(format!("{path} doesn't exist any more"));
    }
    let mut cmd;
    #[cfg(target_os = "macos")]
    {
        cmd = std::process::Command::new("open");
        if p.is_file() {
            cmd.arg("-R");
        }
        cmd.arg(p);
    }
    #[cfg(target_os = "windows")]
    {
        cmd = std::process::Command::new("explorer");
        if p.is_file() {
            cmd.arg(format!("/select,{}", p.display()));
        } else {
            cmd.arg(p);
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // xdg-open can't select a file; open the folder holding it.
        cmd = std::process::Command::new("xdg-open");
        cmd.arg(if p.is_file() {
            p.parent().unwrap_or(p)
        } else {
            p
        });
    }
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("open file manager: {e}"))
}

/// Ask an in-flight [`logs_pull`] to stop. Cooperative: the transfer
/// aborts at the next read boundary and the file is not written.
#[tauri::command]
pub fn logs_cancel() {
    CANCEL_PULL.store(true, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str, size: u32) -> FileKey {
        FileKey {
            node: 0x02,
            name: name.to_string(),
            size,
        }
    }

    fn pulled(name: &str, size: u32, crc: u32, path: &str, at: u64) -> LedgerEvent {
        LedgerEvent::Pulled {
            key: key(name, size),
            crc32: crc,
            path: path.to_string(),
            at,
        }
    }

    /// The ledger is a file on disk that outlives app versions: pin the
    /// exact line shape (flat, camelCase, `kind` tag).
    #[test]
    fn ledger_lines_have_a_stable_shape() {
        let line =
            serde_json::to_value(pulled("LOG0042.CSV", 100, 7, "/x/LOG0042.CSV", 9)).unwrap();
        assert_eq!(
            line,
            serde_json::json!({
                "kind": "pulled", "node": 2, "name": "LOG0042.CSV", "size": 100,
                "crc32": 7, "path": "/x/LOG0042.CSV", "at": 9
            })
        );
        let hide = serde_json::to_value(LedgerEvent::Hide {
            key: key("IMU0001.CSV", 5),
            at: 1,
        })
        .unwrap();
        assert_eq!(hide["kind"], "hide");
        assert_eq!(hide["name"], "IMU0001.CSV");
    }

    #[test]
    fn a_torn_or_unknown_line_is_skipped_not_fatal() {
        let good = serde_json::to_string(&pulled("LOG0001.CSV", 10, 1, "/a", 1)).unwrap();
        let text = format!("{good}\n{{\"kind\":\"pulled\",\"node\":2,\"na\n{{\"kind\":\"fromTheFuture\"}}\n\n{good}\n");
        assert_eq!(parse_ledger(&text).len(), 2);
    }

    #[test]
    fn hide_unhide_and_forget_replay_in_order() {
        let k = key("LOG0001.CSV", 10);
        let events = vec![
            pulled("LOG0001.CSV", 10, 1, "/a", 1),
            LedgerEvent::Hide {
                key: k.clone(),
                at: 2,
            },
            LedgerEvent::Unhide {
                key: k.clone(),
                at: 3,
            },
            LedgerEvent::Forget {
                key: k.clone(),
                at: 4,
            },
            pulled("LOG0001.CSV", 10, 1, "/b", 5),
        ];
        let state = &fold_ledger(&events)[&k];
        assert!(!state.hidden);
        assert_eq!(state.pulled, vec![(1, "/b".to_string(), 5)]);
    }

    #[test]
    fn downloaded_only_while_the_copy_is_on_disk_at_the_listed_size() {
        let k = key("LOG0001.CSV", 10);
        let state = fold_ledger(&[
            pulled("LOG0001.CSV", 10, 1, "/old", 1),
            pulled("LOG0001.CSV", 10, 1, "/new", 2),
        ]);
        let st = state.get(&k);

        // Newest present copy wins.
        let (s, path, at) = classify(&k, st, &|_, _| true);
        assert_eq!(
            (s, path.as_deref(), at),
            (FileStatus::Downloaded, Some("/new"), Some(2))
        );

        // Only the older copy survives: still downloaded, pointing at it.
        let (s, path, _) = classify(&k, st, &|p, _| p == "/old");
        assert_eq!((s, path.as_deref()), (FileStatus::Downloaded, Some("/old")));

        // Gone (or truncated, which `on_disk` reports the same way): Missing.
        let (s, path, _) = classify(&k, st, &|_, _| false);
        assert_eq!((s, path.as_deref()), (FileStatus::Missing, Some("/new")));

        // Never downloaded: New.
        assert_eq!(classify(&k, None, &|_, _| true).0, FileStatus::New);
    }

    #[test]
    fn a_hide_wins_but_never_claims_a_download() {
        let k = key("IMU0003.CSV", 10);
        let state = fold_ledger(&[LedgerEvent::Hide {
            key: k.clone(),
            at: 1,
        }]);
        let (s, path, _) = classify(&k, state.get(&k), &|_, _| true);
        assert_eq!((s, path), (FileStatus::Hidden, None));
    }

    #[test]
    fn same_name_different_size_is_a_different_file() {
        let state = fold_ledger(&[pulled("LOG0001.CSV", 10, 1, "/a", 1)]);
        let other = key("LOG0001.CSV", 11);
        assert_eq!(
            classify(&other, state.get(&other), &|_, _| true).0,
            FileStatus::New
        );
    }

    #[test]
    fn file_on_disk_checks_the_exact_size() {
        let dir = std::env::temp_dir().join(format!("mingocan-logs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("LOG0001.CSV");
        std::fs::write(&f, b"0123456789").unwrap();
        let p = f.to_str().unwrap();
        assert!(file_on_disk(p, 10));
        assert!(!file_on_disk(p, 11));
        assert!(!file_on_disk(dir.to_str().unwrap(), 0));
        assert!(!file_on_disk(dir.join("nope").to_str().unwrap(), 10));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn csv_beside_finds_only_a_decoded_binary_log() {
        let dir = std::env::temp_dir().join(format!("mingocan-logs-csv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("CEL0003.BIN");
        assert_eq!(csv_beside(&bin), None);
        std::fs::write(dir.join("CEL0003.csv"), b"x").unwrap();
        assert_eq!(
            csv_beside(&bin),
            Some(dir.join("CEL0003.csv").display().to_string())
        );
        assert_eq!(csv_beside(&dir.join("LOG0003.CSV")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn spot_checks_cover_both_ends_and_the_middle() {
        assert_eq!(pick_spot_checks(vec![]), Vec::<u16>::new());
        assert_eq!(pick_spot_checks(vec![5, 1]), vec![1, 5]);
        assert_eq!(
            pick_spot_checks(vec![9, 1, 4, 7, 2, 0x8003]),
            vec![1, 7, 0x8003]
        );
    }

    #[test]
    fn status_and_scan_result_serialise_camel_case() {
        let r = ScanResult {
            files: vec![ScannedFile {
                index: 0x8001,
                name: "IMU0001.CSV".into(),
                size: 1,
                status: FileStatus::Missing,
                path: Some("/a".into()),
                pulled_at: Some(3),
                csv_path: None,
            }],
            card_changed: false,
            ledger_error: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["cardChanged"], false);
        assert_eq!(v["files"][0]["status"], "missing");
        assert_eq!(v["files"][0]["pulledAt"], 3);
        let a: MarkAction = serde_json::from_str("\"unhide\"").unwrap();
        assert!(matches!(a, MarkAction::Unhide));
    }
}
