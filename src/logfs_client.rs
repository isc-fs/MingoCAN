//! Shared LOGFS pull orchestration: OPEN → READ loop → CRC → CLOSE, with
//! two robustness properties the bench found necessary (#506, IFS08_HIL#94):
//!
//! - **Transport retry.** A single ranged READ is re-sent on a timeout or
//!   transport error. Reads are ranged and stateless firmware-side, so
//!   re-requesting the same window is safe. A multi-MB pull is thousands
//!   of round trips; without this one dropped frame discards the file.
//!
//! - **Session recovery.** On a busy shared bus the node's diag session
//!   can drop mid-transfer (the host stalls long enough for the firmware's
//!   idle timeout, or a keepalive READ is lost), after which every command
//!   NACKs `BAD_SESSION`. Rather than abort the whole pull, we transparently
//!   re-CONNECT and re-OPEN — the node released our handle when the session
//!   died — and resume the ranged read from the last acked offset.
//!
//! Both `can-flasher logs pull` and the Studio Data logs view call [`pull_file`] so
//! this logic lives and is tested in exactly one place.

use std::time::Duration;

use tokio::time::sleep;

use crate::firmware::crc32;
use crate::protocol::commands::{cmd_logfs_close, cmd_logfs_crc, cmd_logfs_open, cmd_logfs_read};
use crate::protocol::logfs::{self, OpenedFile, ReadOutcome, MAX_READ_LEN};
use crate::protocol::opcodes::NackCode;
use crate::protocol::Response;
use crate::session::{Session, SessionError};

/// Re-send an idempotent LOGFS command this many times before failing.
const RETRY_ATTEMPTS: u32 = 3;

/// Linear backoff base — attempt N waits `N * this`.
const RETRY_BACKOFF_MS: u64 = 60;

/// How many times a pull may re-establish a dropped session **without the
/// transfer advancing** before giving up. The counter resets on every byte
/// of progress, so a pull that keeps moving may reconnect as many times as
/// it needs — a large file naturally drops the session more often than a
/// small one. Only a genuinely stuck pull (this many reconnects in a row,
/// no new bytes) fails.
///
/// A total cap was wrong: it failed a 525 KB transfer that was recovering
/// and advancing (0 → 26% → 41%) simply because it needed more than five
/// reconnects across its length, while a 293 KB file fit inside five and
/// completed (IFS08_HIL#94).
const MAX_STALLED_RESYNCS: u32 = 5;

/// Outcome of a completed pull.
pub struct PullResult {
    pub data: Vec<u8>,
    /// `true` when the node's CRC matched the received bytes. Always
    /// `true` when `verify` was requested (a mismatch returns an error
    /// instead); `false` when verification was skipped.
    pub crc_verified: bool,
}

/// Why a pull stopped short.
#[derive(Debug)]
pub enum PullError {
    /// The operator cancelled — not a failure. Carries no message so the
    /// caller can render it neutrally.
    Cancelled,
    /// Anything else, with an operator-readable explanation.
    Failed(String),
}

impl std::fmt::Display for PullError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => write!(f, "cancelled by operator"),
            Self::Failed(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for PullError {}

fn failed(msg: impl Into<String>) -> PullError {
    PullError::Failed(msg.into())
}

/// One send attempt's classified result.
enum Sent {
    Ack(Vec<u8>),
    /// The node lost our session (`NACK BAD_SESSION`) — recoverable by
    /// re-connecting.
    SessionLost,
    /// A different NACK, or a non-recoverable error, already described.
    Failed(String),
}

/// Send one idempotent LOGFS command with transport-level retries, and
/// classify the reply. `BAD_SESSION` is surfaced distinctly so the caller
/// can resync; every other NACK is terminal.
async fn send_retrying(session: &Session, payload: Vec<u8>, what: &str) -> Sent {
    let expected_opcode = payload.first().copied();
    let mut attempt = 1u32;
    loop {
        match session.send_app_command(&payload).await {
            Ok(Response::Ack { opcode, payload }) => {
                return match expected_opcode {
                    Some(want) if opcode != want => Sent::Failed(format!(
                        "reply to {what} echoes opcode 0x{opcode:02X}, expected 0x{want:02X} \
                         — replies are out of step with requests"
                    )),
                    _ => Sent::Ack(payload),
                };
            }
            Ok(Response::Nack {
                code: NackCode::BadSession,
                ..
            }) => {
                return Sent::SessionLost;
            }
            Ok(Response::Nack {
                rejected_opcode,
                code,
            }) => {
                return Sent::Failed(format!(
                    "device NACK'd {what} (opcode 0x{rejected_opcode:02X}): {code}"
                ));
            }
            Ok(other) => {
                return Sent::Failed(format!("unexpected reply to {what}: {}", other.kind_str()));
            }
            // Timeout / transport error — retry the same (idempotent) window.
            Err(e @ SessionError::CommandTimeout { .. }) | Err(e @ SessionError::Transport(_)) => {
                if attempt >= RETRY_ATTEMPTS {
                    return Sent::Failed(format!("{what}: {e}"));
                }
                sleep(Duration::from_millis(RETRY_BACKOFF_MS * u64::from(attempt))).await;
                attempt += 1;
            }
            Err(e) => return Sent::Failed(format!("{what}: {e}")),
        }
    }
}

/// OPEN a file by index, returning the parsed reply (handle + size + crc).
async fn open_file(session: &Session, index: u16) -> Result<OpenedFile, PullError> {
    match send_retrying(session, cmd_logfs_open(index), "LOGFS_OPEN").await {
        Sent::Ack(body) => {
            logfs::parse_open(&body).map_err(|e| failed(format!("parse LOGFS_OPEN: {e}")))
        }
        Sent::SessionLost => Err(failed(
            "LOGFS_OPEN rejected with BAD_SESSION — the diag session is not open",
        )),
        Sent::Failed(msg) => Err(failed(msg)),
    }
}

/// Re-establish a dropped session and re-open `index`, returning the fresh
/// handle. The node released the old handle when the session died, so this
/// is the only way to keep pulling.
async fn resync(session: &Session, index: u16) -> Result<OpenedFile, PullError> {
    session
        .app_connect()
        .await
        .map_err(|e| failed(format!("re-CONNECT after session drop: {e}")))?;
    open_file(session, index).await
}

/// Pull one log file to memory.
///
/// `on_progress(received, total)` is called after every chunk (`total` is
/// `0` if the node didn't report a size). `is_cancelled()` is polled at
/// each read boundary; returning `true` aborts with [`PullError::Cancelled`]
/// after closing the handle. `verify` compares the node's CRC against the
/// received bytes.
///
/// The session must already be open ([`Session::app_connect`]); this only
/// re-connects to recover from a mid-pull drop.
pub async fn pull_file<P, C>(
    session: &Session,
    index: u16,
    verify: bool,
    on_progress: P,
    is_cancelled: C,
) -> Result<PullResult, PullError>
where
    P: Fn(u32, u32),
    C: Fn() -> bool,
{
    let mut open = open_file(session, index).await?;
    let mut data: Vec<u8> = Vec::with_capacity(open.size as usize);
    let mut offset = 0u32;
    // Consecutive reconnects since the last byte of progress. Reset to 0
    // whenever the transfer advances, so only a stuck pull trips the cap.
    let mut stalled_resyncs = 0u32;

    loop {
        if is_cancelled() {
            let _ = close(session, open.handle).await;
            return Err(PullError::Cancelled);
        }

        match send_retrying(
            session,
            cmd_logfs_read(open.handle, offset, MAX_READ_LEN),
            "LOGFS_READ",
        )
        .await
        {
            Sent::Ack(body) => {
                let out: ReadOutcome = logfs::parse_read(MAX_READ_LEN, &body);
                if !out.data.is_empty() {
                    // Forward progress — the reconnect budget is about being
                    // *stuck*, not about the total count, so clear it.
                    stalled_resyncs = 0;
                }
                data.extend_from_slice(&out.data);
                offset = offset.saturating_add(out.data.len() as u32);
                on_progress(offset, open.size);
                if out.eof {
                    break;
                }
                if out.data.is_empty() {
                    return Err(failed(format!(
                        "LOGFS_READ returned no data before EOF at offset {offset}"
                    )));
                }
            }
            Sent::SessionLost => {
                if stalled_resyncs >= MAX_STALLED_RESYNCS {
                    return Err(failed(format!(
                        "session dropped mid-pull and did not hold after {MAX_STALLED_RESYNCS} \
                         reconnect(s) with no progress (stuck at {offset}/{} B) — the bus is \
                         too busy to finish",
                        open.size
                    )));
                }
                stalled_resyncs += 1;
                // Re-open resets the handle; the loop retries the SAME
                // offset with it, so no bytes are re-fetched or skipped.
                open = resync(session, index).await?;
            }
            Sent::Failed(msg) => return Err(failed(msg)),
        }
    }

    if open.size > 0 && data.len() as u32 != open.size {
        let _ = close(session, open.handle).await;
        return Err(failed(format!(
            "size mismatch: OPEN said {} B, transfer produced {} B",
            open.size,
            data.len()
        )));
    }

    let crc_verified = if verify {
        // OPEN usually carries the sealed crc32; only fall back to an
        // explicit LOGFS_CRC when the node deferred it.
        let want = if open.crc_deferred() {
            match send_retrying(session, cmd_logfs_crc(open.handle), "LOGFS_CRC").await {
                Sent::Ack(body) => {
                    logfs::parse_crc(&body).map_err(|e| failed(format!("parse LOGFS_CRC: {e}")))?
                }
                Sent::SessionLost => {
                    return Err(failed(
                        "session dropped before CRC verification; retry the pull",
                    ))
                }
                Sent::Failed(msg) => return Err(failed(msg)),
            }
        } else {
            open.crc32
        };
        let got = crc32(&data);
        if want != got {
            let _ = close(session, open.handle).await;
            return Err(failed(format!(
                "CRC mismatch: node says 0x{want:08X}, received bytes are 0x{got:08X}"
            )));
        }
        true
    } else {
        false
    };

    let _ = close(session, open.handle).await;
    Ok(PullResult { data, crc_verified })
}

/// Best-effort CLOSE — a failure here doesn't undo a completed transfer,
/// and the node reclaims the handle on session end regardless.
async fn close(session: &Session, handle: u16) -> Result<(), PullError> {
    match send_retrying(session, cmd_logfs_close(handle), "LOGFS_CLOSE").await {
        Sent::Ack(_) => Ok(()),
        Sent::SessionLost => Ok(()), // session gone → handle already freed
        Sent::Failed(msg) => Err(failed(msg)),
    }
}
