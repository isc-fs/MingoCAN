//! `replay` subcommand — record live CAN traffic to a file and read
//! it back later.
//!
//! Two sub-actions today:
//!
//! - **`replay record --out <file>`** — passive monitor. Opens the
//!   configured adapter and writes every received frame to `<file>`
//!   in Linux `candump -l` format. Runs until Ctrl-C or
//!   `--duration-ms` elapses. No frames are sent on the bus from
//!   this subcommand; pair it with a separate `flash` / `discover`
//!   / etc. invocation to capture traffic from a live session.
//! - **`replay run <file>`** — reads a candump log and prints each
//!   frame either as a human-readable line or as a `--json`
//!   object. This is a log-analysis / diff helper; **full fidelity
//!   replay** (feeding the frames into the virtual bus and
//!   re-driving the host) is a later feat branch if a concrete
//!   test scenario asks for it.
//!
//! ## candump format
//!
//! Each line looks like:
//!
//! ```text
//! (1609459200.123456) can0 123#AABBCCDD
//! ```
//!
//! - `(seconds.microseconds)` — wall clock at the moment of
//!   capture. Not monotonic; adequate for human inspection and
//!   relative-delta playback.
//! - `interface` — the adapter channel the frame arrived on
//!   (e.g. `can0`, or `virtual` for the in-process stub).
//! - `id#data` — hex ID followed by `#`, then concatenated hex
//!   bytes (two chars per byte, no separator). 11-bit IDs print as
//!   3 uppercase hex chars; extended 29-bit IDs would print as 8
//!   chars uppercase — the bootloader only uses 11-bit so the
//!   parser rejects anything else for v1.
//!
//! Compatible with Linux `canplayer` and `cantools` — log files
//! produced here can be replayed against a `vcan` interface
//! externally if the need arises.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde::Serialize;
use tokio::time::Instant;
use tracing::{debug, warn};

use super::GlobalFlags;
use crate::protocol::CanFrame;
use crate::transport::open_backend;
use crate::transport::TransportError;

#[derive(Debug, Args)]
pub struct ReplayArgs {
    #[command(subcommand)]
    pub action: ReplayAction,
}

#[derive(Debug, Subcommand)]
pub enum ReplayAction {
    /// Record live CAN traffic to a file (candump `-l` format)
    Record {
        /// Output file. Existing contents will be truncated.
        #[arg(long = "out", value_name = "FILE")]
        out: PathBuf,

        /// Stop recording after this many milliseconds. If omitted
        /// the recorder runs until Ctrl-C.
        #[arg(long = "duration-ms")]
        duration_ms: Option<u64>,
    },

    /// Read a recorded session file and print each frame
    Run {
        /// Recorded candump-format file
        #[arg(value_name = "FILE")]
        file: PathBuf,
    },
}

pub async fn run(args: ReplayArgs, global: &GlobalFlags) -> Result<()> {
    match args.action {
        ReplayAction::Record { out, duration_ms } => run_record(out, duration_ms, global).await,
        ReplayAction::Run { file } => run_run(file, global).await,
    }
}

// ---- record ----

async fn run_record(out: PathBuf, duration_ms: Option<u64>, global: &GlobalFlags) -> Result<()> {
    let backend = open_backend(global.interface, global.channel.as_deref(), global.bitrate)
        .context("opening CAN backend for replay record")?;

    let file =
        File::create(&out).with_context(|| format!("creating output file '{}'", out.display()))?;
    let mut writer = BufWriter::new(file);

    // Log a brief header comment so humans can tell the file apart
    // from a vanilla candump dump. Lines starting with `#` aren't
    // part of the candump spec but are widely ignored by reader
    // implementations.
    let iface_label = interface_label(global.channel.as_deref(), global.interface);
    writeln!(
        writer,
        "# can-flasher replay record — iface={} bitrate={}",
        iface_label, global.bitrate
    )
    .context("writing candump header")?;

    let interface_for_line = iface_label.clone();
    eprintln!(
        "Recording to {} on interface '{interface_for_line}'. Press Ctrl-C to stop.",
        out.display()
    );

    let deadline = duration_ms.map(|ms| Instant::now() + Duration::from_millis(ms));
    // Short recv timeout so the loop wakes up often enough to notice
    // cancel / deadline signals.
    let slice = Duration::from_millis(100);

    // Graceful shutdown: either Ctrl-C or deadline expiry.
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    tokio::pin!(shutdown);

    let mut frame_count: u64 = 0;
    loop {
        // Check deadline before each recv so --duration-ms is
        // respected even if the bus is completely idle.
        if let Some(d) = deadline {
            if Instant::now() >= d {
                break;
            }
        }

        tokio::select! {
            _ = &mut shutdown => {
                eprintln!("\nstopping record…");
                break;
            }
            result = backend.recv(slice) => match result {
                Ok(frame) => {
                    let line = format_candump_line(&frame, &interface_for_line, SystemTime::now());
                    writeln!(writer, "{line}").context("writing candump frame")?;
                    frame_count += 1;
                }
                Err(TransportError::Timeout(_)) => continue,
                Err(TransportError::Disconnected) => {
                    warn!("recorder: backend disconnected, stopping");
                    break;
                }
                Err(other) => return Err(anyhow::anyhow!(other)),
            }
        }
    }

    writer.flush().context("flushing candump writer")?;
    eprintln!("Wrote {frame_count} frame(s) to {}.", out.display());
    Ok(())
}

fn interface_label(channel: Option<&str>, iface: super::InterfaceType) -> String {
    // Interface label convention: prefer the user's `--channel` value
    // when available (e.g. `can0`, `/dev/ttyACM0`, `PCAN_USBBUS1`);
    // fall back to the adapter kind otherwise (`virtual`, `slcan`, …).
    // Strip characters that would break the candump format
    // (space, hash) — turn `/dev/ttyACM0` into `dev_ttyACM0`.
    let raw = channel
        .map(str::to_string)
        .unwrap_or_else(|| format!("{iface:?}").to_ascii_lowercase());
    sanitize_interface(&raw)
}

fn sanitize_interface(raw: &str) -> String {
    raw.chars()
        .map(|c| match c {
            '/' | ' ' | '#' | '(' | ')' => '_',
            other => other,
        })
        .skip_while(|c| *c == '_')
        .collect()
}

// ---- run (read + pretty-print) ----

#[derive(Debug, Serialize)]
struct FrameJson {
    timestamp: f64,
    interface: String,
    id: String,
    len: u8,
    data: String,
}

async fn run_run(file: PathBuf, global: &GlobalFlags) -> Result<()> {
    debug!(path = %file.display(), "replay run: reading");
    let handle =
        File::open(&file).with_context(|| format!("opening candump file '{}'", file.display()))?;
    let reader = BufReader::new(handle);

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let mut frame_count: u64 = 0;
    let mut json_rows: Vec<FrameJson> = Vec::new();

    for (lineno, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("reading line {}", lineno + 1))?;
        let trimmed = line.trim_end();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            // Blank line or header comment — skip.
            continue;
        }
        let parsed = parse_candump_line(trimmed)
            .with_context(|| format!("parsing line {}: '{trimmed}'", lineno + 1))?;

        frame_count += 1;

        if global.json {
            json_rows.push(FrameJson {
                timestamp: parsed.timestamp_secs,
                interface: parsed.interface.clone(),
                id: format!("0x{:03X}", parsed.id),
                len: parsed.len,
                data: hex_encode(&parsed.data[..parsed.len as usize]),
            });
        } else {
            writeln!(
                out,
                "({:.6}) {} 0x{:03X}  {} byte(s)  {}",
                parsed.timestamp_secs,
                parsed.interface,
                parsed.id,
                parsed.len,
                hex_encode_spaced(&parsed.data[..parsed.len as usize]),
            )
            .context("writing output")?;
        }
    }

    if global.json {
        serde_json::to_writer_pretty(&mut out, &json_rows).context("serialising frames as JSON")?;
        writeln!(out).ok();
    } else if frame_count == 0 {
        writeln!(out, "(no frames in {})", file.display())?;
    } else {
        writeln!(out, "— {frame_count} frame(s) total")?;
    }

    Ok(())
}

// ---- candump line format ----

/// One parsed candump record. Used internally by the `run` path.
#[derive(Debug, Clone, PartialEq)]
struct ParsedFrame {
    timestamp_secs: f64,
    interface: String,
    id: u16,
    len: u8,
    data: [u8; 8],
}

/// Encode a `CanFrame` plus metadata into a candump line:
/// `(seconds.microseconds) iface ID#DATA`.
fn format_candump_line(frame: &CanFrame, interface: &str, ts: SystemTime) -> String {
    let (secs, micros) = system_time_to_secs_micros(ts);
    let data_hex = hex_encode(&frame.data[..frame.len as usize]);
    // 11-bit ID: 3 hex chars, zero-padded. Mask to 11 bits just in
    // case the caller handed us a raw 16-bit value with extra bits
    // set (shouldn't happen — FrameId::encode already masks — but
    // belt-and-braces for a format-level helper).
    format!(
        "({secs}.{micros:06}) {interface} {:03X}#{data_hex}",
        frame.id & 0x7FF
    )
}

/// Parse one candump line into a [`ParsedFrame`]. Lenient about
/// whitespace inside but strict about shape — the format is tightly
/// specified and invalid input surfaces as a descriptive error.
fn parse_candump_line(line: &str) -> Result<ParsedFrame> {
    let mut parts = line.split_whitespace();

    // Timestamp `(s.µs)`.
    let ts_token = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("line is empty"))?;
    let ts = parse_timestamp(ts_token)?;

    // Interface name.
    let interface = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing interface after timestamp"))?
        .to_string();

    // ID#data token.
    let iddata = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing `id#data` after interface"))?;
    let (id_part, data_part) = iddata
        .split_once('#')
        .ok_or_else(|| anyhow::anyhow!("`id#data` separator missing"))?;

    // Classic CAN 11-bit ID: 3 hex digits max. Reject extended IDs —
    // the bootloader doesn't emit them, and silently truncating
    // would hide real corruption.
    if id_part.len() > 3 {
        bail!(
            "ID '{id_part}' has more than 3 hex chars (11-bit only); extended CAN frames not supported"
        );
    }
    let id =
        u16::from_str_radix(id_part, 16).with_context(|| format!("parsing hex ID '{id_part}'"))?;
    if id > 0x7FF {
        bail!("ID 0x{id:X} does not fit in 11 bits");
    }

    // Data bytes.
    if data_part.len() % 2 != 0 {
        bail!("data has odd number of hex digits: '{data_part}'");
    }
    let len_bytes = data_part.len() / 2;
    if len_bytes > 8 {
        bail!("data length {len_bytes} exceeds classic CAN max of 8 bytes: '{data_part}'");
    }
    let mut data = [0u8; 8];
    for (i, chunk) in data_part.as_bytes().chunks(2).enumerate() {
        let pair = std::str::from_utf8(chunk)
            .with_context(|| format!("non-ASCII in data pair at offset {}", i * 2))?;
        data[i] = u8::from_str_radix(pair, 16)
            .with_context(|| format!("parsing hex data pair '{pair}' at offset {}", i * 2))?;
    }

    Ok(ParsedFrame {
        timestamp_secs: ts,
        interface,
        id,
        len: len_bytes as u8,
        data,
    })
}

fn parse_timestamp(token: &str) -> Result<f64> {
    // Shape: `(sss.µµµµµµ)` — parentheses + decimal number.
    let inner = token
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .ok_or_else(|| anyhow::anyhow!("timestamp '{token}' is not wrapped in parentheses"))?;
    inner
        .parse::<f64>()
        .with_context(|| format!("parsing timestamp '{token}'"))
}

fn system_time_to_secs_micros(t: SystemTime) -> (u64, u32) {
    let elapsed = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    (elapsed.as_secs(), elapsed.subsec_micros())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn hex_encode_spaced(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

// `replay run` needs `Path` — quiet clippy about the unused import
// on some configurations.
#[allow(dead_code)]
fn _silence_path_warning(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- candump format round-trip ----

    #[test]
    fn format_line_handles_11_bit_id_and_short_payload() {
        let frame = CanFrame::new(0x123, &[0xAA, 0xBB, 0xCC]).unwrap();
        let ts = UNIX_EPOCH + Duration::new(1_609_459_200, 123_456_000);
        let line = format_candump_line(&frame, "vcan0", ts);
        assert_eq!(line, "(1609459200.123456) vcan0 123#AABBCC");
    }

    #[test]
    fn format_line_handles_empty_payload() {
        let frame = CanFrame::new(0x003, &[]).unwrap();
        let ts = UNIX_EPOCH + Duration::new(0, 0);
        let line = format_candump_line(&frame, "virtual", ts);
        assert_eq!(line, "(0.000000) virtual 003#");
    }

    #[test]
    fn format_line_pads_id_to_three_hex_chars() {
        let frame = CanFrame::new(0x003, &[]).unwrap();
        let ts = UNIX_EPOCH;
        let line = format_candump_line(&frame, "can0", ts);
        assert!(
            line.contains(" 003#"),
            "id should be 3-hex-char padded: {line}"
        );
    }

    #[test]
    fn parse_line_round_trips() {
        let original = "(1609459200.123456) vcan0 123#AABBCC";
        let parsed = parse_candump_line(original).unwrap();
        assert_eq!(parsed.timestamp_secs, 1_609_459_200.123456);
        assert_eq!(parsed.interface, "vcan0");
        assert_eq!(parsed.id, 0x123);
        assert_eq!(parsed.len, 3);
        assert_eq!(&parsed.data[..3], &[0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn parse_line_accepts_empty_payload() {
        let parsed = parse_candump_line("(0.000000) v 003#").unwrap();
        assert_eq!(parsed.len, 0);
        assert_eq!(parsed.id, 0x003);
    }

    #[test]
    fn parse_line_rejects_extended_id() {
        let err = parse_candump_line("(1.000000) c 12345678#AA").unwrap_err();
        assert!(err.to_string().contains("11-bit only"));
    }

    #[test]
    fn parse_line_rejects_missing_hash() {
        let err = parse_candump_line("(1.000000) c 123AABB").unwrap_err();
        assert!(err.to_string().contains("separator missing"));
    }

    #[test]
    fn parse_line_rejects_odd_hex() {
        let err = parse_candump_line("(1.000000) c 123#AAB").unwrap_err();
        assert!(err.to_string().contains("odd number of hex digits"));
    }

    #[test]
    fn parse_line_rejects_oversize_payload() {
        // 9 bytes (18 hex chars) — classic CAN caps at 8.
        let err = parse_candump_line("(1.000000) c 123#AABBCCDDEEFF0011223344").unwrap_err();
        assert!(err.to_string().contains("exceeds classic CAN max"));
    }

    #[test]
    fn sanitize_interface_replaces_slashes_and_spaces() {
        assert_eq!(sanitize_interface("/dev/ttyACM0"), "dev_ttyACM0");
        assert_eq!(sanitize_interface("PCAN_USBBUS1"), "PCAN_USBBUS1");
        assert_eq!(sanitize_interface("can0"), "can0");
    }

    #[test]
    fn hex_encode_is_uppercase_and_packed() {
        assert_eq!(hex_encode(&[0xDE, 0xAD, 0xBE, 0xEF]), "DEADBEEF");
        assert_eq!(hex_encode(&[]), "");
    }

    // ---- Round-trip: encode → parse ----

    #[test]
    fn round_trip_through_parse_preserves_fields() {
        let frame =
            CanFrame::new(0x7FF, &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]).unwrap();
        let ts = UNIX_EPOCH + Duration::new(42, 999_999_000);
        let line = format_candump_line(&frame, "vcan0", ts);
        let parsed = parse_candump_line(&line).unwrap();
        assert_eq!(parsed.id, frame.id);
        assert_eq!(parsed.len, frame.len);
        assert_eq!(parsed.data, frame.data);
        assert_eq!(parsed.interface, "vcan0");
        // Microsecond precision: 999999 µs → 0.999999 s
        assert!((parsed.timestamp_secs - 42.999999).abs() < 1e-9);
    }
}
