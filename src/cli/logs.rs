//! `cf logs` — list and pull the microSD data logs off a node over CAN.
//!
//! Implements the host side of the LOGFS service (IFS08-CE-AMS#406 /
//! #506) on top of the existing CONNECT session + ISO-TP transport.
//! Read-only: there is deliberately no `delete` subcommand in v1.
//!
//! Flow mirrors the spec's recommended sequence:
//! `CONNECT → LIST (paginate) → per file: OPEN → READ… → CRC → CLOSE →
//! DISCONNECT`. Reads are ranged and stateless per request, so a dropped
//! transfer just re-requests that range.
//!
//! Note the node id comes from `--node-id`; nothing here hardcodes the
//! AMS address, so the pending `0x01 → 0x02` move is a flag change.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use tracing::debug;

use super::GlobalFlags;
use crate::firmware::crc32;
use crate::protocol::commands::{
    cmd_logfs_close, cmd_logfs_crc, cmd_logfs_list, cmd_logfs_open, cmd_logfs_read,
};
use crate::protocol::logfs::{self, LogEntry, MAX_READ_LEN};
use crate::protocol::responses::Response;
use crate::session::{Session, SessionConfig};
use crate::transport::open_backend;

#[derive(Debug, Args)]
pub struct LogsArgs {
    #[command(subcommand)]
    pub command: LogsCommand,
}

#[derive(Debug, Subcommand)]
pub enum LogsCommand {
    /// List the log files on the node's microSD card
    List,

    /// Download log file(s) to a local directory
    Pull(PullArgs),
}

#[derive(Debug, Args)]
pub struct PullArgs {
    /// Log index to pull (from `logs list`). Omit to pull every file.
    #[arg(long)]
    pub index: Option<u16>,

    /// Directory to write the downloaded log(s) into
    #[arg(short, long, default_value = ".")]
    pub out: PathBuf,

    /// Skip the LOGFS_CRC verification step (not recommended)
    #[arg(long)]
    pub no_verify: bool,
}

pub async fn run(global: &GlobalFlags, args: &LogsArgs) -> Result<()> {
    match &args.command {
        LogsCommand::List => run_list(global).await,
        LogsCommand::Pull(p) => run_pull(global, p).await,
    }
}

fn open_session(global: &GlobalFlags) -> Result<Session> {
    let backend = open_backend(global.interface, global.channel.as_deref(), global.bitrate)
        .context("opening CAN backend for logs")?;
    let config = SessionConfig {
        target_node: global.node_id.unwrap_or(0x3),
        keepalive_interval: Duration::from_millis(5_000),
        command_timeout: Duration::from_millis(u64::from(global.timeout_ms)),
        ..SessionConfig::default()
    };
    Ok(Session::attach(backend, config))
}

/// Send one LOGFS command and unwrap the ACK body (opcode already
/// stripped by the response parser).
async fn ack_body(session: &Session, payload: Vec<u8>, what: &str) -> Result<Vec<u8>> {
    match session
        .send_command(&payload)
        .await
        .with_context(|| format!("sending {what}"))?
    {
        Response::Ack { payload, .. } => Ok(payload),
        Response::Nack {
            rejected_opcode,
            code,
        } => bail!("device NACK'd {what} (opcode 0x{rejected_opcode:02X}): {code}"),
        other => bail!("unexpected reply to {what}: {}", other.kind_str()),
    }
}

/// Walk `LOGFS_LIST` to completion, following the cursor.
async fn list_all(session: &Session) -> Result<Vec<LogEntry>> {
    let mut all = Vec::new();
    let mut cursor = 0u16;
    loop {
        let body = ack_body(session, cmd_logfs_list(cursor), "LOGFS_LIST").await?;
        let page = logfs::parse_list(&body).context("parsing LOGFS_LIST response")?;
        debug!(
            cursor,
            next = page.next_cursor,
            entries = page.entries.len(),
            "logfs list page"
        );
        let is_last = page.is_last();
        let next = page.next_cursor;
        all.extend(page.entries);
        if is_last {
            break;
        }
        if next == cursor {
            bail!("LOGFS_LIST cursor did not advance (stuck at {cursor}) — aborting");
        }
        cursor = next;
    }
    Ok(all)
}

async fn run_list(global: &GlobalFlags) -> Result<()> {
    let session = open_session(global)?;
    session.connect().await.context("CONNECT before LOGFS_LIST")?;
    let entries = list_all(&session).await;
    let _ = session.disconnect().await;
    let entries = entries?;

    if global.json {
        // `mtime` is monotonic/boot-relative, so it is emitted raw — it
        // is NOT a unix timestamp and must not be formatted as a date.
        let items: Vec<String> = entries
            .iter()
            .map(|e| {
                format!(
                    r#"{{"index":{},"name":"{}","size":{},"mtimeMonotonic":{}}}"#,
                    e.index, e.name, e.size, e.mtime
                )
            })
            .collect();
        println!("[{}]", items.join(","));
        return Ok(());
    }

    if entries.is_empty() {
        println!("no log files on the card");
        return Ok(());
    }
    println!("{:>5}  {:<12} {:>10}  {:>12}", "INDEX", "NAME", "SIZE", "MTIME(mono)");
    for e in &entries {
        println!(
            "{:>5}  {:<12} {:>10}  {:>12}",
            e.index, e.name, e.size, e.mtime
        );
    }
    println!(
        "\n{} file(s). mtime is boot-relative (no RTC) — ordering only, not a date.",
        entries.len()
    );
    Ok(())
}

/// Pull one file: OPEN → READ until EOF → CRC → CLOSE. Returns the bytes.
async fn pull_one(session: &Session, entry: &LogEntry, verify: bool) -> Result<Vec<u8>> {
    let body = ack_body(session, cmd_logfs_open(entry.index), "LOGFS_OPEN").await?;
    let open = logfs::parse_open(&body).context("parsing LOGFS_OPEN response")?;
    debug!(
        handle = open.handle,
        size = open.size,
        crc_deferred = open.crc_deferred(),
        "opened log"
    );

    let mut data: Vec<u8> = Vec::with_capacity(open.size as usize);
    let mut offset = 0u32;
    loop {
        let body = ack_body(
            session,
            cmd_logfs_read(open.handle, offset, MAX_READ_LEN),
            "LOGFS_READ",
        )
        .await?;
        let out = logfs::parse_read(MAX_READ_LEN, &body);
        data.extend_from_slice(&out.data);
        offset = offset.saturating_add(out.data.len() as u32);

        // Progress on one rewritten line; size may be 0 if unknown.
        if open.size > 0 {
            let pct = (u64::from(offset) * 100 / u64::from(open.size)).min(100);
            print!("\r  {} … {pct:>3}% ({offset}/{} B)", entry.name, open.size);
            let _ = std::io::stdout().flush();
        }

        if out.eof {
            break;
        }
        if out.data.is_empty() {
            bail!("LOGFS_READ returned no data before EOF at offset {offset}");
        }
    }
    println!();

    if open.size > 0 && data.len() as u32 != open.size {
        bail!(
            "size mismatch for {}: OPEN said {} B, transfer produced {} B",
            entry.name,
            open.size,
            data.len()
        );
    }

    if verify {
        let body = ack_body(session, cmd_logfs_crc(open.handle), "LOGFS_CRC").await?;
        let want = logfs::parse_crc(&body).context("parsing LOGFS_CRC response")?;
        let got = crc32(&data);
        if want != got {
            bail!(
                "CRC mismatch for {}: node says 0x{want:08X}, received bytes are 0x{got:08X}",
                entry.name
            );
        }
        debug!(crc = format!("0x{want:08X}"), "crc verified");
    }

    let _ = ack_body(session, cmd_logfs_close(open.handle), "LOGFS_CLOSE").await?;
    Ok(data)
}

async fn run_pull(global: &GlobalFlags, args: &PullArgs) -> Result<()> {
    let session = open_session(global)?;
    session.connect().await.context("CONNECT before LOGFS pull")?;

    let result = async {
        let entries = list_all(&session).await?;
        let selected: Vec<&LogEntry> = match args.index {
            Some(i) => {
                let found = entries.iter().find(|e| e.index == i);
                match found {
                    Some(e) => vec![e],
                    None => bail!("no log with index {i} on the card (try `cf logs list`)"),
                }
            }
            None => entries.iter().collect(),
        };
        if selected.is_empty() {
            println!("no log files on the card");
            return Ok(());
        }

        std::fs::create_dir_all(&args.out)
            .with_context(|| format!("creating output dir {}", args.out.display()))?;

        for e in selected {
            let data = pull_one(&session, e, !args.no_verify).await?;
            let path = unique_path(&args.out, &e.name);
            std::fs::write(&path, &data)
                .with_context(|| format!("writing {}", path.display()))?;
            println!("  saved {} ({} B)", path.display(), data.len());
        }
        Ok(())
    }
    .await;

    let _ = session.disconnect().await;
    result
}

/// Don't clobber an existing download — `LOG0001.CSV` → `LOG0001.CSV.1`.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let base = dir.join(name);
    if !base.exists() {
        return base;
    }
    for n in 1..1000 {
        let candidate = dir.join(format!("{name}.{n}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_path_avoids_clobbering() {
        let dir = std::env::temp_dir().join(format!("cf-logs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "LOG0001.CSV");
        assert!(first.ends_with("LOG0001.CSV"));
        std::fs::write(&first, b"x").unwrap();
        let second = unique_path(&dir, "LOG0001.CSV");
        assert!(second.ends_with("LOG0001.CSV.1"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
