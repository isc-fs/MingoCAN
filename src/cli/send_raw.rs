//! `send-raw` subcommand — transmit one raw CAN frame bypassing the
//! bootloader protocol entirely.
//!
//! The rest of this CLI speaks the isc-fs BL protocol: ISO-TP
//! segmentation, node-ID encoding, ACK/NACK reassembly, session
//! keepalives. That's the right default for every *bootloader* action
//! — but there's one routine workflow it can't serve: getting a
//! running application back into bootloader mode so the next flash
//! can happen. The BL's command channel only works when the BL
//! itself is running; once we've jumped to the application, any
//! "please reboot into BL" signal has to travel on a channel the
//! application owns, on an ID the app-side firmware listens for.
//!
//! `send-raw` is the **generic primitive** for that: one frame, one
//! 11-bit ID, 0..=8 payload bytes. It knows nothing about ISO-TP or
//! the BL protocol's ID encoding — the caller picks both. That's
//! deliberate: the "escape to BL" frame is only one of many
//! app-defined conventions operators may need (health pings,
//! board-specific probes, bench-test harnesses, calibration stimuli,
//! …). The specific ID/payload convention lives in the app
//! firmware + REQUIREMENTS.md, not here.
//!
//! CAN arbitration gives lower IDs higher bus priority. The ID
//! ranges the BL protocol claims — and the app-control range that
//! mirrors its layout — live in REQUIREMENTS.md and are locked down
//! by the BL firmware's RX filter; see the app-side handler for the
//! opcode payload layout.
//!
//! ### Example
//! ```shell
//! cf --interface slcan --channel /dev/cu.usbmodem1201 \
//!    send-raw 0x010 01
//! ```
//! (sends one classic-CAN frame with ID `0x010` and 1 byte of
//! payload, then listens 100 ms for replies. The exact ID to use
//! for the reboot-to-BL escape is a protocol question — resolved in
//! the matching fix branch that updates BL, app, and docs together.)
//!
//! ### Listening for replies
//! By default `send-raw` listens for **100 ms** after transmitting
//! and prints every frame that arrives in that window. That's enough
//! to catch a prompt ACK from the peer without stalling the CLI.
//! `--listen-ms 0` skips the listen phase; `--listen-ms <N>` for a
//! larger N lets you catch slower replies or watch the bus briefly
//! after a stimulus.

use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;
use tracing::debug;

use super::GlobalFlags;
use crate::protocol::CanFrame;
use crate::transport::open_backend;

/// Parse an 11-bit CAN ID from `0x…` hex or plain decimal. Rejects
/// values that don't fit in 11 bits (`0x000..=0x7FF`), which is all
/// classic-CAN standard IDs. Extended (29-bit) IDs aren't supported
/// — the SLCAN transport rejects them and none of our workflows
/// need them today.
fn parse_can_id(raw: &str) -> Result<u16, String> {
    let trimmed = raw.trim();
    let (body, radix) = if let Some(rest) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        (rest, 16)
    } else {
        (trimmed, 10)
    };
    let n = u16::from_str_radix(body, radix).map_err(|e| format!("invalid CAN id '{raw}': {e}"))?;
    if n > 0x7FF {
        return Err(format!(
            "CAN id 0x{n:X} does not fit in 11 bits (max 0x7FF)"
        ));
    }
    Ok(n)
}

/// Parse one hex byte — accepts `01`, `0x01`, `0X01`, case-insensitive.
/// Used for every payload byte; the loop rejects payloads longer than
/// 8 bytes separately so the per-byte error message stays focused.
fn parse_hex_byte(raw: &str) -> Result<u8, String> {
    let trimmed = raw.trim();
    let body = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u8::from_str_radix(body, 16).map_err(|e| format!("invalid hex byte '{raw}': {e}"))
}

#[derive(Debug, Args)]
pub struct SendRawArgs {
    /// 11-bit CAN ID to transmit (hex `0x031` or decimal `49`).
    #[arg(value_parser = parse_can_id)]
    pub id: u16,

    /// Payload bytes as hex, one per argument: `01 02 0A` or
    /// `0x01 0x02 0x0A`. Max 8 bytes; empty payload is allowed
    /// (zero-byte classic-CAN frame).
    #[arg(value_parser = parse_hex_byte, num_args = 0..=8)]
    pub data: Vec<u8>,

    /// Listen for inbound frames for this many ms after transmitting,
    /// printing each one. `0` skips the listen phase. Default 100 ms
    /// catches a prompt ACK from the peer without stalling the CLI.
    #[arg(long = "listen-ms", default_value_t = 100)]
    pub listen_ms: u64,
}

pub async fn run(args: SendRawArgs, global: &GlobalFlags) -> Result<()> {
    // `data.len() <= 8` is already enforced by clap's `num_args`, but
    // a second guard here keeps the contract explicit in case the
    // parser config ever drifts.
    if args.data.len() > CanFrame::MAX_LEN {
        anyhow::bail!(
            "payload must be at most {} bytes; got {}",
            CanFrame::MAX_LEN,
            args.data.len()
        );
    }

    debug!(
        id = format!("0x{:03X}", args.id),
        len = args.data.len(),
        "send-raw: opening backend"
    );

    let backend = open_backend(global.interface, global.channel.as_deref(), global.bitrate)
        .context("opening CAN backend")?;
    backend
        .set_bitrate(global.bitrate)
        .await
        .context("setting bitrate")?;

    let frame =
        CanFrame::new(args.id, &args.data).map_err(|e| anyhow::anyhow!("building frame: {e}"))?;
    backend
        .send(frame)
        .await
        .context("sending frame over CAN")?;

    if !global.json {
        println!(
            "sent: id=0x{:03X} len={} data=[{}]",
            args.id,
            args.data.len(),
            hex_bytes(&args.data),
        );
    }

    // Listen phase — print every frame that arrives in the window.
    // We deliberately don't filter by ID: the caller is doing raw
    // diagnostics, and surfacing the whole bus activity is usually
    // what they want ("did the peer reply? did anyone else chime
    // in?").
    if args.listen_ms > 0 {
        let deadline = Instant::now() + Duration::from_millis(args.listen_ms);
        let mut received: Vec<CanFrame> = Vec::new();
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            // Poll in tight-ish slices so `remaining` counts down
            // cleanly if a burst of frames arrives.
            let slice = remaining.min(Duration::from_millis(25));
            match backend.recv(slice).await {
                Ok(frame) => received.push(frame),
                Err(crate::transport::TransportError::Timeout(_)) => continue,
                Err(err) => {
                    debug!(?err, "send-raw: recv error during listen phase");
                    break;
                }
            }
        }
        if global.json {
            print_json_replies(&received, &args)?;
        } else {
            print_human_replies(&received);
        }
    } else if global.json {
        // `--json --listen-ms 0`: still emit a stable shape so scripts
        // don't have to branch.
        print_json_replies(&[], &args)?;
    }

    Ok(())
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{b:02X}"));
    }
    s
}

fn print_human_replies(frames: &[CanFrame]) {
    if frames.is_empty() {
        println!("listened: (no replies)");
        return;
    }
    println!("listened: {} frame(s)", frames.len());
    for f in frames {
        println!(
            "  id=0x{:03X} len={} data=[{}]",
            f.id,
            f.len,
            hex_bytes(f.payload())
        );
    }
}

fn print_json_replies(frames: &[CanFrame], args: &SendRawArgs) -> Result<()> {
    let replies: Vec<_> = frames
        .iter()
        .map(|f| {
            serde_json::json!({
                "id":   format!("0x{:03X}", f.id),
                "len":  f.len,
                "data": hex_bytes(f.payload()),
            })
        })
        .collect();
    let report = serde_json::json!({
        "sent": {
            "id":   format!("0x{:03X}", args.id),
            "len":  args.data.len(),
            "data": hex_bytes(&args.data),
        },
        "listened_ms": args.listen_ms,
        "replies":     replies,
    });
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer_pretty(&mut out, &report).context("serialising send-raw report")?;
    writeln!(out).ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_can_id_accepts_hex() {
        assert_eq!(parse_can_id("0x31").unwrap(), 0x31);
        assert_eq!(parse_can_id("0X7FF").unwrap(), 0x7FF);
        assert_eq!(parse_can_id("0x000").unwrap(), 0);
    }

    #[test]
    fn parse_can_id_accepts_decimal() {
        assert_eq!(parse_can_id("49").unwrap(), 49);
        assert_eq!(parse_can_id("0").unwrap(), 0);
    }

    #[test]
    fn parse_can_id_rejects_extended() {
        // 0x800 is the smallest extended ID — we're 11-bit only.
        assert!(parse_can_id("0x800").is_err());
        assert!(parse_can_id("0xFFFF").is_err());
    }

    #[test]
    fn parse_can_id_rejects_junk() {
        assert!(parse_can_id("").is_err());
        assert!(parse_can_id("0xZZZ").is_err());
        assert!(parse_can_id("not-a-number").is_err());
    }

    #[test]
    fn parse_hex_byte_round_trips() {
        assert_eq!(parse_hex_byte("01").unwrap(), 0x01);
        assert_eq!(parse_hex_byte("0x0A").unwrap(), 0x0A);
        assert_eq!(parse_hex_byte("FF").unwrap(), 0xFF);
        assert_eq!(parse_hex_byte("ff").unwrap(), 0xFF);
    }

    #[test]
    fn parse_hex_byte_rejects_overflow() {
        // u8 max is 0xFF — anything higher is invalid.
        assert!(parse_hex_byte("0x100").is_err());
        assert!(parse_hex_byte("256").is_err()); // decimal not supported
    }

    #[test]
    fn parse_hex_byte_rejects_junk() {
        assert!(parse_hex_byte("").is_err());
        assert!(parse_hex_byte("zz").is_err());
    }

    #[test]
    fn hex_bytes_empty_renders_empty() {
        assert_eq!(hex_bytes(&[]), "");
    }

    #[test]
    fn hex_bytes_is_uppercase_space_separated() {
        assert_eq!(hex_bytes(&[0x01, 0x0A, 0xFF]), "01 0A FF");
    }
}
