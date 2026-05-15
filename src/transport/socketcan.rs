//! Native SocketCAN backend (Linux only).
//!
//! Uses the kernel's `AF_CAN` stack via the [`socketcan`] crate's
//! tokio integration. The whole module is compiled out on Windows
//! and macOS via `#![cfg(target_os = "linux")]`; callers select this
//! backend through `--interface socketcan` (direct) or
//! `--interface pcan` on Linux (the CLI routes PCAN through SocketCAN
//! on Linux since the `peak_usb` kernel module exposes PCAN adapters
//! as SocketCAN interfaces).
//!
//! ## Channel string
//!
//! Interface names as shown by `ip link show`: `can0`, `can1`, `vcan0`,
//! etc. Nothing fancy — the same names the `ip` / `candump` toolchain
//! uses.
//!
//! ## Bitrate policy
//!
//! SocketCAN interfaces are configured outside the process (usually
//! `sudo ip link set can0 up type can bitrate 500000`), so the
//! host-supplied `--bitrate` flag is advisory only. We log the value
//! at `debug` level and otherwise leave the kernel alone — changing
//! bitrate from userspace requires `CAP_NET_ADMIN` which the flasher
//! deliberately doesn't take per REQUIREMENTS.md § Non-functional
//! requirements ("No root on Linux").
//!
//! ## Frame filtering
//!
//! SocketCAN delivers data, remote, and error frames through the same
//! socket. The bootloader protocol only uses 11-bit **data** frames,
//! so this backend silently drops remote/error frames and keeps
//! reading. A bus error that produces a stream of error frames would
//! manifest as a `Timeout` at the caller — consistent with how the
//! SLCAN backend would behave under the same bus conditions.

#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use async_trait::async_trait;
use socketcan::tokio::CanSocket;
use socketcan::{EmbeddedFrame, Id, StandardId};
use tracing::{debug, trace};

use crate::protocol::CanFrame;

use super::{CanBackend, Result, TransportError};

// ---- Backend ----

/// Wraps a Linux `AF_CAN` socket. Cheap to construct; the
/// [`CanSocket`] value internally holds a file descriptor registered
/// with the tokio reactor.
pub struct SocketCanBackend {
    socket: CanSocket,
    description: String,
    interface: String,
}

impl SocketCanBackend {
    /// Bind to `interface` (e.g. `can0`, `vcan0`). The interface must
    /// already be up — the flasher does not attempt `ip link` changes
    /// and will not request elevated privileges.
    pub fn open(interface: &str) -> Result<Self> {
        if interface.is_empty() {
            return Err(TransportError::InvalidChannel {
                channel: String::new(),
                reason: "SocketCAN interface name must not be empty — try `can0` or `vcan0`".into(),
            });
        }

        let socket = CanSocket::open(interface).map_err(|err| {
            // Most "no such device" errors surface as io::ErrorKind::NotFound;
            // include the interface name so the user doesn't have to
            // guess what failed.
            TransportError::InvalidChannel {
                channel: interface.to_string(),
                reason: format!(
                    "could not open SocketCAN interface (`ip link show {interface}` up?): \
                     {err}"
                ),
            }
        })?;

        let description = format!("SocketCAN (iface {interface})");
        Ok(Self {
            socket,
            description,
            interface: interface.to_string(),
        })
    }
}

#[async_trait]
impl CanBackend for SocketCanBackend {
    async fn send(&self, frame: CanFrame) -> Result<()> {
        // Per-frame trace symmetric with the SLCAN / PCAN / Vector
        // backends. Useful for diagnosing ISO-TP framing bugs against
        // a stricter bootloader (e.g. issue #178 — bootloader v1.2.0
        // strictness around PCI nibbles + zero-payload CFs).
        trace!(
            id = format!("0x{:03X}", frame.id),
            len = frame.len,
            data = format!("{:02X?}", frame.payload()),
            "socketcan tx",
        );
        let sc_frame = our_frame_to_socketcan(&frame)?;
        self.socket
            .write_frame(sc_frame)
            .await
            .map_err(TransportError::Io)?;
        Ok(())
    }

    async fn recv(&self, timeout: Duration) -> Result<CanFrame> {
        // Skip non-data frames inline so the caller doesn't see
        // remote/error frames it doesn't know how to handle. The
        // deadline bounds the total wait across any number of
        // skipped frames.
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(TransportError::Timeout(timeout));
            }
            let next = tokio::time::timeout(remaining, self.socket.read_frame()).await;
            match next {
                Ok(Ok(sc_frame)) => {
                    if let Some(data_frame) = extract_data_frame(&sc_frame) {
                        return socketcan_data_to_our(data_frame);
                    }
                    // Remote/error frame — skip and keep reading
                    // within the remaining deadline.
                    continue;
                }
                Ok(Err(err)) => return Err(TransportError::Io(err)),
                Err(_elapsed) => return Err(TransportError::Timeout(timeout)),
            }
        }
    }

    async fn set_bitrate(&self, nominal_bps: u32) -> Result<()> {
        debug!(
            interface = %self.interface,
            nominal_bps,
            "SocketCAN: --bitrate is advisory on Linux; configure via `ip link set … bitrate`"
        );
        Ok(())
    }

    fn description(&self) -> String {
        self.description.clone()
    }
}

// ---- Detection ----

/// Enumerate SocketCAN interfaces by walking `/sys/class/net/*/type`
/// and picking entries with `type == 280` (ARPHRD_CAN). Best-effort:
/// non-Linux builds never reach this module, and a sysfs read failure
/// just returns an empty list.
pub fn detect() -> Vec<SocketCanAdapterInfo> {
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let type_path = entry.path().join("type");
        let Ok(raw) = std::fs::read_to_string(&type_path) else {
            continue;
        };
        // ARPHRD_CAN = 280 per <linux/if_arp.h>.
        if raw.trim() == "280" {
            out.push(SocketCanAdapterInfo { interface: name });
        }
    }
    out
}

/// Enumerated SocketCAN interface, surfaced by the `adapters`
/// subcommand.
#[derive(Debug, Clone)]
pub struct SocketCanAdapterInfo {
    pub interface: String,
}

// ---- Frame conversion ----

fn our_frame_to_socketcan(frame: &CanFrame) -> Result<socketcan::CanFrame> {
    if frame.id > 0x7FF {
        return Err(TransportError::Other(format!(
            "SocketCAN: extended (29-bit) ID 0x{:X} not supported by v1 flasher",
            frame.id
        )));
    }
    let id = StandardId::new(frame.id).ok_or_else(|| {
        TransportError::Other(format!(
            "SocketCAN: could not build standard ID from 0x{:X}",
            frame.id
        ))
    })?;
    let payload = frame.payload();
    socketcan::CanFrame::new(id, payload).ok_or_else(|| {
        TransportError::Other(format!(
            "SocketCAN: could not build data frame ({} byte payload)",
            payload.len()
        ))
    })
}

fn extract_data_frame(frame: &socketcan::CanFrame) -> Option<&socketcan::CanDataFrame> {
    match frame {
        socketcan::CanFrame::Data(data) => Some(data),
        socketcan::CanFrame::Remote(_) | socketcan::CanFrame::Error(_) => None,
    }
}

fn socketcan_data_to_our(frame: &socketcan::CanDataFrame) -> Result<CanFrame> {
    let id = match frame.id() {
        Id::Standard(std) => std.as_raw(),
        Id::Extended(_) => {
            // The bootloader protocol is 11-bit only. Surface an
            // extended frame as an Other error so the caller's retry
            // logic treats it like any other unexpected bus noise.
            return Err(TransportError::Other(
                "SocketCAN: received extended (29-bit) CAN frame; bootloader uses 11-bit only"
                    .into(),
            ));
        }
    };
    let data = frame.data();
    if data.len() > 8 {
        return Err(TransportError::Other(format!(
            "SocketCAN: received frame with {} bytes of data (> 8 on classic CAN)",
            data.len()
        )));
    }
    let mut out = [0u8; 8];
    out[..data.len()].copy_from_slice(data);
    Ok(CanFrame {
        id,
        data: out,
        len: data.len() as u8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: u16, data: &[u8]) -> CanFrame {
        CanFrame::new(id, data).unwrap()
    }

    #[test]
    fn our_to_socketcan_standard_frame() {
        let f = frame(0x003, &[0x01, 0x02, 0x03]);
        let sc = our_frame_to_socketcan(&f).unwrap();
        let data_frame = match &sc {
            socketcan::CanFrame::Data(d) => d,
            other => panic!("expected Data, got {other:?}"),
        };
        match data_frame.id() {
            Id::Standard(std) => assert_eq!(std.as_raw(), 0x003),
            Id::Extended(_) => panic!("unexpected extended"),
        }
        assert_eq!(data_frame.data(), &[0x01, 0x02, 0x03]);
    }

    #[test]
    fn our_to_socketcan_full_payload() {
        let f = frame(0x7FF, &[1, 2, 3, 4, 5, 6, 7, 8]);
        let sc = our_frame_to_socketcan(&f).unwrap();
        let data = match sc {
            socketcan::CanFrame::Data(d) => d,
            _ => panic!("expected Data"),
        };
        assert_eq!(data.data().len(), 8);
    }

    #[test]
    fn our_to_socketcan_rejects_extended_id() {
        // 0x800 sets bit 11 → we treat as extended and refuse.
        let extended = CanFrame {
            id: 0x800,
            data: [0u8; 8],
            len: 0,
        };
        assert!(matches!(
            our_frame_to_socketcan(&extended),
            Err(TransportError::Other(_))
        ));
    }

    #[test]
    fn socketcan_to_our_standard_frame() {
        let id = StandardId::new(0x230).unwrap();
        let sc = socketcan::CanDataFrame::new(id, &[0x10, 0xFE]).unwrap();
        let ours = socketcan_data_to_our(&sc).unwrap();
        assert_eq!(ours.id, 0x230);
        assert_eq!(ours.len, 2);
        assert_eq!(&ours.data[..2], &[0x10, 0xFE]);
    }

    #[test]
    fn socketcan_to_our_empty_payload() {
        let id = StandardId::new(0x100).unwrap();
        let sc = socketcan::CanDataFrame::new(id, &[]).unwrap();
        let ours = socketcan_data_to_our(&sc).unwrap();
        assert_eq!(ours.id, 0x100);
        assert_eq!(ours.len, 0);
    }

    #[test]
    fn roundtrip_through_socketcan() {
        let originals = [
            frame(0x003, &[0x01, 0x00, 0x01]),
            frame(0x7FF, &[0xAB, 0xCD]),
            frame(0x100, &[]),
            frame(0x1EF, &[0xAA; 8]),
        ];
        for f in originals {
            let sc = our_frame_to_socketcan(&f).unwrap();
            let data = extract_data_frame(&sc).unwrap();
            let back = socketcan_data_to_our(data).unwrap();
            assert_eq!(f, back, "roundtrip of {f:?}");
        }
    }

    #[test]
    fn extract_data_frame_recognises_data_variant() {
        let id = StandardId::new(0x123).unwrap();
        let data = socketcan::CanDataFrame::new(id, &[0xAA, 0xBB]).unwrap();
        // CanDataFrame is Copy — just move it in and out.
        let wrapped = socketcan::CanFrame::Data(data);
        let extracted = extract_data_frame(&wrapped).expect("should extract data frame");
        assert_eq!(extracted.data(), &[0xAA, 0xBB]);
    }

    #[test]
    fn open_rejects_empty_interface() {
        // Can't use .unwrap_err() because SocketCanBackend doesn't
        // implement Debug (the CanSocket from socketcan::tokio doesn't
        // either). Destructure manually.
        let result = SocketCanBackend::open("");
        let err = result.err().expect("empty interface should fail");
        match err {
            TransportError::InvalidChannel { channel, .. } => {
                assert!(channel.is_empty());
            }
            other => panic!("expected InvalidChannel, got {other:?}"),
        }
    }
}
