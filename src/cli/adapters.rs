//! `adapters` subcommand — enumerate detectable CAN adapters on the
//! current machine.
//!
//! Collects the per-backend `detect()` outputs (SLCAN USB enumeration
//! via `serialport`, SocketCAN sysfs walk on Linux, PCAN channel
//! probing via `libloading` on Windows/macOS) and prints them either
//! as a human-readable summary or as a stable JSON object for
//! downstream tooling.
//!
//! JSON shape (always all four top-level keys present, even on
//! platforms where a backend isn't supported — the key just holds an
//! empty array, which keeps consumer code stable across OSes):
//!
//! ```json
//! {
//!   "slcan": [
//!     { "channel": "/dev/ttyACM0", "description": "CANable (…)",
//!       "vid": "0x1d50", "pid": "0x606f" }
//!   ],
//!   "socketcan": [
//!     { "interface": "can0" }
//!   ],
//!   "pcan": [
//!     { "channel": "PCAN_USBBUS1", "channel_byte": "0x51" }
//!   ],
//!   "vector": [
//!     { "channel": "0", "name": "VN1610 1 Channel 1",
//!       "transceiver": "CAN - TJA1041" }
//!   ]
//! }
//! ```

use std::io::Write;

use anyhow::{Context, Result};
use serde::Serialize;
use tracing::debug;

use super::GlobalFlags;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use crate::transport::pcan;
use crate::transport::slcan;
#[cfg(target_os = "linux")]
use crate::transport::socketcan;
#[cfg(target_os = "windows")]
use crate::transport::vector;

/// Structured report. Serialised as-is for `--json` mode; the human
/// formatter iterates over the same fields.
#[derive(Debug, Default, Serialize)]
pub struct AdapterReport {
    pub slcan: Vec<SlcanEntry>,
    pub socketcan: Vec<SocketCanEntry>,
    pub pcan: Vec<PcanEntry>,
    pub vector: Vec<VectorEntry>,
}

#[derive(Debug, Serialize)]
pub struct SlcanEntry {
    pub channel: String,
    pub description: String,
    /// `"0x1d50"` on hit, `None` for non-USB ports (which we filter
    /// out today, but the field is optional so a future SLCAN-over-
    /// serial-cable case renders cleanly).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SocketCanEntry {
    pub interface: String,
}

#[derive(Debug, Serialize)]
pub struct PcanEntry {
    pub channel: String,
    /// `"0x51"`..`"0x60"`, the numeric constant the PCAN-Basic API
    /// takes.
    pub channel_byte: String,
}

#[derive(Debug, Serialize)]
pub struct VectorEntry {
    /// Decimal XL channel index — pass as `--channel N`.
    pub channel: String,
    /// Human-readable channel name from the XL Driver Library
    /// (e.g. `"VN1610 1 Channel 1"`).
    pub name: String,
    /// Transceiver name (e.g. `"CAN - TJA1041"`).
    pub transceiver: String,
}

/// Collect adapters from every backend available on this platform.
/// Non-supported backends yield empty vectors.
pub fn collect_report() -> AdapterReport {
    let mut report = AdapterReport::default();

    for info in slcan::detect() {
        let (vid, pid) = match info.vid_pid {
            Some((vid, pid)) => (Some(format!("0x{vid:04x}")), Some(format!("0x{pid:04x}"))),
            None => (None, None),
        };
        report.slcan.push(SlcanEntry {
            channel: info.channel,
            description: info.description,
            vid,
            pid,
        });
    }

    #[cfg(target_os = "linux")]
    {
        for info in socketcan::detect() {
            report.socketcan.push(SocketCanEntry {
                interface: info.interface,
            });
        }
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        for info in pcan::detect() {
            report.pcan.push(PcanEntry {
                channel: info.channel_name,
                channel_byte: format!("0x{:02X}", info.channel_byte),
            });
        }
    }

    #[cfg(target_os = "windows")]
    {
        for info in vector::detect() {
            report.vector.push(VectorEntry {
                channel: info.channel_index.to_string(),
                name: info.name,
                transceiver: info.transceiver_name,
            });
        }
    }

    report
}

pub async fn run(global: &GlobalFlags) -> Result<()> {
    debug!("adapters: starting detection");
    let report = collect_report();

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if global.json {
        serde_json::to_writer_pretty(&mut out, &report)
            .context("serialising adapter report as JSON")?;
        writeln!(out).ok();
    } else {
        write_human(&mut out, &report).context("writing adapter report")?;
    }
    Ok(())
}

// ---- Human-readable formatting ----
//
// Kept hand-rolled (no `tabled` dep) because the three sections have
// different column sets and mixed-section tables would be noisier
// than the labelled lists below. Format is stable enough to line up
// cleanly in a terminal without pulling in a layout engine.

fn write_human<W: Write>(out: &mut W, report: &AdapterReport) -> std::io::Result<()> {
    write_slcan_section(out, &report.slcan)?;
    writeln!(out)?;
    write_pcan_section(out, &report.pcan)?;
    writeln!(out)?;
    write_vector_section(out, &report.vector)?;
    writeln!(out)?;
    write_socketcan_section(out, &report.socketcan)?;
    Ok(())
}

fn write_slcan_section<W: Write>(out: &mut W, entries: &[SlcanEntry]) -> std::io::Result<()> {
    writeln!(out, "SLCAN serial ports:")?;
    if entries.is_empty() {
        writeln!(out, "  (none detected)")?;
        return Ok(());
    }
    // Pad the channel column so descriptions line up.
    let channel_width = entries.iter().map(|e| e.channel.len()).max().unwrap_or(0);
    for e in entries {
        writeln!(
            out,
            "  {:width$}   {}",
            e.channel,
            e.description,
            width = channel_width
        )?;
    }
    Ok(())
}

fn write_pcan_section<W: Write>(out: &mut W, entries: &[PcanEntry]) -> std::io::Result<()> {
    writeln!(out, "PCAN devices:")?;
    if !supports_pcan_basic() {
        writeln!(out, "  (PCAN-Basic only supported on Windows / macOS — on Linux PCAN adapters appear under SocketCAN)")?;
        return Ok(());
    }
    if entries.is_empty() {
        writeln!(out, "  (none detected — PCAN-Basic library may be missing)")?;
        return Ok(());
    }
    let channel_width = entries.iter().map(|e| e.channel.len()).max().unwrap_or(0);
    for e in entries {
        writeln!(
            out,
            "  {:width$}   ({})",
            e.channel,
            e.channel_byte,
            width = channel_width
        )?;
    }
    Ok(())
}

fn write_socketcan_section<W: Write>(
    out: &mut W,
    entries: &[SocketCanEntry],
) -> std::io::Result<()> {
    writeln!(out, "SocketCAN interfaces:")?;
    if !supports_socketcan() {
        writeln!(out, "  (SocketCAN is Linux-only)")?;
        return Ok(());
    }
    if entries.is_empty() {
        writeln!(out, "  (none detected — try `ip link show type can`)")?;
        return Ok(());
    }
    for e in entries {
        writeln!(out, "  {}", e.interface)?;
    }
    Ok(())
}

fn write_vector_section<W: Write>(out: &mut W, entries: &[VectorEntry]) -> std::io::Result<()> {
    writeln!(out, "Vector XL devices:")?;
    if !supports_vector() {
        writeln!(
            out,
            "  (Vector XL Driver Library is currently Windows-only — Linux support planned)"
        )?;
        return Ok(());
    }
    if entries.is_empty() {
        writeln!(
            out,
            "  (none detected — XL Driver Library may be missing or no hardware connected)"
        )?;
        return Ok(());
    }
    let channel_width = entries.iter().map(|e| e.channel.len()).max().unwrap_or(0);
    let name_width = entries.iter().map(|e| e.name.len()).max().unwrap_or(0);
    for e in entries {
        writeln!(
            out,
            "  {:cw$}   {:nw$}   {}",
            e.channel,
            e.name,
            e.transceiver,
            cw = channel_width,
            nw = name_width,
        )?;
    }
    Ok(())
}

const fn supports_pcan_basic() -> bool {
    cfg!(any(target_os = "windows", target_os = "macos"))
}

const fn supports_socketcan() -> bool {
    cfg!(target_os = "linux")
}

const fn supports_vector() -> bool {
    cfg!(target_os = "windows")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> AdapterReport {
        AdapterReport {
            slcan: vec![SlcanEntry {
                channel: "/dev/ttyACM0".into(),
                description: "CANable 2.0 (/dev/ttyACM0, USB 1d50:606f)".into(),
                vid: Some("0x1d50".into()),
                pid: Some("0x606f".into()),
            }],
            socketcan: vec![SocketCanEntry {
                interface: "can0".into(),
            }],
            pcan: vec![PcanEntry {
                channel: "PCAN_USBBUS1".into(),
                channel_byte: "0x51".into(),
            }],
            vector: vec![VectorEntry {
                channel: "0".into(),
                name: "VN1610 1 Channel 1".into(),
                transceiver: "CAN - TJA1041".into(),
            }],
        }
    }

    #[test]
    fn json_shape_has_all_four_sections() {
        let json = serde_json::to_string(&sample_report()).unwrap();
        assert!(json.contains("\"slcan\":"));
        assert!(json.contains("\"socketcan\":"));
        assert!(json.contains("\"pcan\":"));
        assert!(json.contains("\"vector\":"));
    }

    #[test]
    fn json_slcan_entry_carries_vid_pid_lowercase() {
        let json = serde_json::to_string(&sample_report()).unwrap();
        assert!(json.contains("\"vid\":\"0x1d50\""));
        assert!(json.contains("\"pid\":\"0x606f\""));
    }

    #[test]
    fn json_omits_missing_vid_pid() {
        let report = AdapterReport {
            slcan: vec![SlcanEntry {
                channel: "COM3".into(),
                description: "generic".into(),
                vid: None,
                pid: None,
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("\"vid\":"));
        assert!(!json.contains("\"pid\":"));
    }

    #[test]
    fn empty_report_serialises_with_empty_arrays() {
        let report = AdapterReport::default();
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"slcan\":[]"));
        assert!(json.contains("\"socketcan\":[]"));
        assert!(json.contains("\"pcan\":[]"));
        assert!(json.contains("\"vector\":[]"));
    }

    // ---- Human formatting ----

    fn human_string(report: &AdapterReport) -> String {
        let mut buf = Vec::new();
        write_human(&mut buf, report).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn human_output_has_four_section_headers() {
        let s = human_string(&sample_report());
        assert!(s.contains("SLCAN serial ports:"));
        assert!(s.contains("PCAN devices:"));
        assert!(s.contains("Vector XL devices:"));
        assert!(s.contains("SocketCAN interfaces:"));
    }

    #[test]
    fn human_output_lists_slcan_entries() {
        let s = human_string(&sample_report());
        assert!(s.contains("/dev/ttyACM0"));
        assert!(s.contains("CANable 2.0"));
    }

    #[test]
    fn human_output_for_empty_slcan_says_none_detected() {
        let report = AdapterReport {
            slcan: vec![],
            socketcan: vec![SocketCanEntry {
                interface: "can0".into(),
            }],
            pcan: vec![],
            vector: vec![],
        };
        let s = human_string(&report);
        assert!(s.contains("SLCAN serial ports:\n  (none detected)"));
    }

    #[test]
    fn human_output_pads_channel_column() {
        // Two SLCAN entries with different channel lengths — the
        // formatter should pad to the longer one.
        let report = AdapterReport {
            slcan: vec![
                SlcanEntry {
                    channel: "COM3".into(),
                    description: "short".into(),
                    vid: None,
                    pid: None,
                },
                SlcanEntry {
                    channel: "/dev/cu.usbmodem14201".into(),
                    description: "long".into(),
                    vid: None,
                    pid: None,
                },
            ],
            ..Default::default()
        };
        let s = human_string(&report);
        // Both rows should align — find the line-starting index of
        // each description by locating the substring.
        let com3_line = s
            .lines()
            .find(|l| l.contains("COM3"))
            .expect("COM3 line present");
        let long_line = s
            .lines()
            .find(|l| l.contains("/dev/cu.usbmodem14201"))
            .expect("long line present");
        let com3_desc_off = com3_line.find("short").unwrap();
        let long_desc_off = long_line.find("long").unwrap();
        assert_eq!(
            com3_desc_off, long_desc_off,
            "description columns should align"
        );
    }

    #[test]
    fn human_output_surfaces_platform_limits() {
        let s = human_string(&AdapterReport::default());
        if cfg!(target_os = "linux") {
            assert!(s.contains("PCAN-Basic only supported on Windows / macOS"));
            assert!(s.contains("Vector XL Driver Library is currently Windows-only"));
        } else if cfg!(target_os = "windows") {
            assert!(s.contains("SocketCAN is Linux-only"));
        } else {
            // macOS: both SocketCAN and Vector are unavailable.
            assert!(s.contains("SocketCAN is Linux-only"));
            assert!(s.contains("Vector XL Driver Library is currently Windows-only"));
        }
    }
}
