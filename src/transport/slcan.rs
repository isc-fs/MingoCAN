//! SLCAN (serial-line CAN) backend — CANable adapters and any other
//! SLCAN-compatible USB-CDC device across all supported host OSes.
//!
//! ## Protocol snapshot
//!
//! SLCAN is an ASCII-over-serial protocol. Every command and every
//! received frame ends with `\r`. The subset the flasher speaks:
//!
//! | Direction | Format | Meaning |
//! |-----------|--------|---------|
//! | `H → A`   | `S<N>\r` | Set a standard bitrate (N in 0..=8, see table below). |
//! | `H → A`   | `O\r` | Open the CAN channel — frames start flowing. |
//! | `H → A`   | `C\r` | Close the CAN channel. |
//! | `H → A`   | `t<III><L><data>\r` | Transmit 11-bit standard frame. |
//! | `A → H`   | `t<III><L><data>\r` | Received 11-bit standard frame. |
//! | `A → H`   | `\r` | Acknowledgement of a command. |
//! | `A → H`   | `\x07` | Error response — the previous command failed. |
//!
//! Where `III` is a 3-digit upper-case hex ID, `L` is a 1-digit hex
//! length (`0..=8`), and `data` is `2L` hex characters.
//!
//! Extended IDs (`T`), remote frames (`r` / `R`), and CAN FD (`d`)
//! are out of scope for the v1 flasher — the bootloader uses only
//! 11-bit standard classic CAN.
//!
//! ## Bitrate map
//!
//! Bitrates map to single-digit commands exactly per the CANable
//! / SLCAN spec:
//!
//! | Command | Bitrate |
//! |---------|---------|
//! | `S0`    | 10 kbps |
//! | `S1`    | 20 kbps |
//! | `S2`    | 50 kbps |
//! | `S3`    | 100 kbps |
//! | `S4`    | 125 kbps |
//! | `S5`    | 250 kbps |
//! | `S6`    | 500 kbps |
//! | `S7`    | 800 kbps |
//! | `S8`    | 1 Mbps |
//!
//! Anything else earns [`TransportError::InvalidChannel`] with an
//! explanation of which rates are supported.
//!
//! ## Threading model
//!
//! `serialport` is a blocking crate. We keep a single handle wrapped
//! in an `std::sync::Mutex` and run a dedicated reader thread that
//! blocks on short reads, line-buffers the incoming bytes, parses
//! each `\r`-terminated line, and pushes decoded `CanFrame`s into a
//! `tokio::sync::mpsc` channel. Async callers await that channel.
//!
//! The reader thread shares the mutex with the send path so an
//! outgoing `t…\r` command briefly blocks the reader — at the
//! 50 ms read timeout we're using, that's a bounded stall of at most
//! 50 ms before the TX path gets the mutex. Acceptable for the
//! latencies we care about (multi-millisecond CAN bus round trips).

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread;
use std::time::Duration;

use async_trait::async_trait;
use serialport::{SerialPort, SerialPortType};
use tokio::sync::{mpsc, Mutex as TokioMutex};
use tracing::{debug, trace, warn};

use crate::protocol::CanFrame;

use super::{CanBackend, Result, TransportError};

// ---- Constants ----

/// Serial-line baud rate for the CDC ACM link. SLCAN adapters expose
/// a fixed-speed USB CDC interface; the host-side baud is a legacy
/// Terminal-emulator holdover and has no effect on the CAN bitrate
/// itself, but we pick a conventionally high value so firmware that
/// does honour it gets the best throughput.
const SERIAL_BAUD: u32 = 2_000_000;

/// How long each reader-side serial read blocks before returning
/// with `ErrorKind::TimedOut`. Keeps the mutex held for at most
/// this long, bounding the TX path's worst-case wait.
const READ_TIMEOUT: Duration = Duration::from_millis(50);

/// How long the open sequence waits for the adapter to acknowledge
/// a command (`\r`) or error (`\x07`). Plenty for USB latency.
const COMMAND_ACK_TIMEOUT: Duration = Duration::from_millis(500);

/// RX queue depth. Sized for the worst-case burst of a full `DTC_READ`
/// multi-frame reply (~92 CFs at 8 bytes) plus some safety margin.
const RX_QUEUE_DEPTH: usize = 256;

// ---- USB VID/PID hints for the `adapters` subcommand ----

/// Canonical CANable (normaldotcom/canable-fw slcan build, flashed
/// by the official canable.io updater onto CANable 1.x boards).
const USB_VID_CANABLE: u16 = 0x1D50;
const USB_PID_CANABLE: u16 = 0x606F;

/// CANtact Pro — FTDI-based SLCAN adapter.
const USB_VID_CANTACT: u16 = 0x0403;
const USB_PID_CANTACT: u16 = 0x6015;

/// normaldotcom/canable-fw slcan build as shipped by **Protofusion
/// Labs** (CANable 2.0 retail). Wire protocol is identical; only the
/// USB descriptor differs from the canonical CANable. Discovered in
/// the wild on an IFS08 dev bench; see fix/5-slcan-canable-forks.
const USB_VID_PROTOFUSION_CANABLE: u16 = 0xAD50;
const USB_PID_PROTOFUSION_CANABLE: u16 = 0x60C4;

/// Best-effort description of an enumerated SLCAN candidate.
#[derive(Debug, Clone)]
pub struct SlcanAdapterInfo {
    pub channel: String,
    pub description: String,
    pub vid_pid: Option<(u16, u16)>,
}

/// Scan the host for likely SLCAN adapters. Used by the `adapters`
/// subcommand. Never blocks on actual adapter I/O — just walks
/// `/dev/serial/by-id`, Windows' COM enumeration, etc.
pub fn detect() -> Vec<SlcanAdapterInfo> {
    let Ok(ports) = serialport::available_ports() else {
        return Vec::new();
    };

    ports
        .into_iter()
        .filter_map(|p| {
            match &p.port_type {
                SerialPortType::UsbPort(usb) => {
                    if !looks_like_slcan(usb.vid, usb.pid, usb.product.as_deref()) {
                        return None;
                    }
                    let description = slcan_description_for(&p.port_name, usb);
                    Some(SlcanAdapterInfo {
                        channel: p.port_name,
                        description,
                        vid_pid: Some((usb.vid, usb.pid)),
                    })
                }
                // Non-USB serial ports (like ttyS0) aren't likely to be
                // SLCAN adapters and would spam the list on Linux
                // systems with legacy hardware. Skip.
                _ => None,
            }
        })
        .collect()
}

/// Decide whether a USB serial port looks like a SLCAN adapter.
/// Two signals, tried in order:
///
/// 1. **Known VID/PID pair** — canonical CANable, CANtact, or the
///    Protofusion Labs CANable fork. Fast path, no allocation.
/// 2. **Product-name substring match** on `"canable"` / `"cantact"`
///    (case-insensitive) — catches future forks whose VID/PID we
///    haven't catalogued yet. Users with a brand-new clone still
///    get a working `adapters` listing; the backend doesn't care
///    about VID/PID, it just opens the port.
///
/// Conservative enough that a random CDC-ACM serial port (Arduino,
/// USB modem, …) doesn't pollute the list; generous enough that a
/// new firmware fork works out of the box.
fn looks_like_slcan(vid: u16, pid: u16, product: Option<&str>) -> bool {
    match (vid, pid) {
        (USB_VID_CANABLE, USB_PID_CANABLE)
        | (USB_VID_CANTACT, USB_PID_CANTACT)
        | (USB_VID_PROTOFUSION_CANABLE, USB_PID_PROTOFUSION_CANABLE) => return true,
        _ => {}
    }
    let name = product.unwrap_or("").to_ascii_lowercase();
    name.contains("canable") || name.contains("cantact")
}

fn slcan_description_for(channel: &str, usb: &serialport::UsbPortInfo) -> String {
    let product = usb.product.as_deref().unwrap_or("");
    let manufacturer = usb.manufacturer.as_deref().unwrap_or("");
    let label = match (manufacturer.is_empty(), product.is_empty()) {
        (false, false) => format!("{manufacturer} {product}"),
        (false, true) => manufacturer.to_string(),
        (true, false) => product.to_string(),
        _ => match (usb.vid, usb.pid) {
            (USB_VID_CANABLE, USB_PID_CANABLE)
            | (USB_VID_PROTOFUSION_CANABLE, USB_PID_PROTOFUSION_CANABLE) => "CANable".to_string(),
            (USB_VID_CANTACT, USB_PID_CANTACT) => "CANtact".to_string(),
            _ => "SLCAN adapter".to_string(),
        },
    };
    format!("{label} ({channel}, USB {:04x}:{:04x})", usb.vid, usb.pid)
}

// ---- Backend ----

/// Opens a serial port, drives the SLCAN open handshake, and spawns
/// a reader thread that decodes incoming frames into a Tokio channel.
pub struct SlcanBackend {
    port: Arc<StdMutex<Box<dyn SerialPort>>>,
    rx: Arc<TokioMutex<mpsc::Receiver<CanFrame>>>,
    shutdown: Arc<AtomicBool>,
    reader_handle: StdMutex<Option<thread::JoinHandle<()>>>,
    description: String,
}

impl SlcanBackend {
    /// Open the named port at the requested bitrate and spin up the
    /// reader thread. On success the channel is already opened
    /// (`O\r`) and ready for TX/RX.
    pub fn open(channel: &str, nominal_bps: u32) -> Result<Self> {
        let bitrate_cmd =
            bitrate_command(nominal_bps).ok_or_else(|| TransportError::InvalidChannel {
                channel: channel.to_string(),
                reason: format!(
                    "unsupported bitrate {nominal_bps} bps — SLCAN supports \
                     10k / 20k / 50k / 100k / 125k / 250k / 500k / 800k / 1M"
                ),
            })?;

        let mut port = serialport::new(channel, SERIAL_BAUD)
            .timeout(COMMAND_ACK_TIMEOUT)
            .open()
            .map_err(|e| TransportError::InvalidChannel {
                channel: channel.to_string(),
                reason: format!("could not open serial port: {e}"),
            })?;

        // Drain anything lingering from a previous session. The adapter
        // might have been left open; sending `C\r` is a no-op for a
        // closed port and resets for an open one.
        let _ = port.write_all(b"C\r");
        let _ = port.flush();
        drain(&mut *port);

        // Set bitrate, then open channel.
        debug!(
            channel,
            nominal_bps,
            command = bitrate_cmd,
            "SLCAN open: setting bitrate"
        );
        send_command_and_wait_ack(&mut *port, &format!("{bitrate_cmd}\r"))?;

        debug!(channel, "SLCAN open: opening channel");
        send_command_and_wait_ack(&mut *port, "O\r")?;

        // Switch to the short reader timeout now that the handshake is done.
        port.set_timeout(READ_TIMEOUT)
            .map_err(|e| TransportError::InvalidChannel {
                channel: channel.to_string(),
                reason: format!("could not set read timeout: {e}"),
            })?;

        let description = format!("SLCAN on {channel} @ {nominal_bps} bps");
        let port = Arc::new(StdMutex::new(port));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel(RX_QUEUE_DEPTH);

        let reader_port = Arc::clone(&port);
        let reader_shutdown = Arc::clone(&shutdown);
        let reader_handle = thread::Builder::new()
            .name("slcan-reader".into())
            .spawn(move || reader_loop(reader_port, tx, reader_shutdown))
            .map_err(|e| TransportError::Other(format!("spawn reader thread: {e}")))?;

        Ok(Self {
            port,
            rx: Arc::new(TokioMutex::new(rx)),
            shutdown,
            reader_handle: StdMutex::new(Some(reader_handle)),
            description,
        })
    }
}

#[async_trait]
impl CanBackend for SlcanBackend {
    async fn send(&self, frame: CanFrame) -> Result<()> {
        let encoded = encode_frame(&frame)?;
        let port = Arc::clone(&self.port);

        // Serial I/O is blocking; hand it to the blocking pool so we
        // don't stall the async runtime.
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut port = port
                .lock()
                .map_err(|_| TransportError::Other("serial port mutex poisoned".into()))?;
            port.write_all(&encoded)?;
            port.flush()?;
            Ok(())
        })
        .await
        .map_err(|e| TransportError::Other(format!("spawn_blocking join failed: {e}")))?
    }

    async fn recv(&self, timeout: Duration) -> Result<CanFrame> {
        let mut rx = self.rx.lock().await;
        match tokio::time::timeout(timeout, rx.recv()).await {
            Ok(Some(frame)) => Ok(frame),
            Ok(None) => Err(TransportError::Disconnected),
            Err(_elapsed) => Err(TransportError::Timeout(timeout)),
        }
    }

    async fn set_bitrate(&self, nominal_bps: u32) -> Result<()> {
        let bitrate_cmd = bitrate_command(nominal_bps).ok_or_else(|| {
            TransportError::Other(format!("unsupported bitrate {nominal_bps} bps"))
        })?;
        let port = Arc::clone(&self.port);
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut port = port
                .lock()
                .map_err(|_| TransportError::Other("serial port mutex poisoned".into()))?;
            // Bus has to be closed before changing bitrate.
            send_command_and_wait_ack(&mut **port, "C\r")?;
            send_command_and_wait_ack(&mut **port, &format!("{bitrate_cmd}\r"))?;
            send_command_and_wait_ack(&mut **port, "O\r")?;
            Ok(())
        })
        .await
        .map_err(|e| TransportError::Other(format!("spawn_blocking join failed: {e}")))?
    }

    fn description(&self) -> String {
        self.description.clone()
    }
}

impl Drop for SlcanBackend {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Best-effort: tell the adapter to close the CAN side. If the
        // mutex is poisoned we skip — nothing productive left to do.
        if let Ok(mut port) = self.port.lock() {
            let _ = port.write_all(b"C\r");
            let _ = port.flush();
        }
        if let Ok(mut handle) = self.reader_handle.lock() {
            if let Some(h) = handle.take() {
                let _ = h.join();
            }
        }
    }
}

// ---- ASCII encoder / parser ----

/// Encode a CanFrame into an SLCAN `t…\r` byte sequence suitable for
/// writing to the serial port. Only 11-bit standard IDs are
/// supported; extended / remote / FD frames return an error.
pub fn encode_frame(frame: &CanFrame) -> Result<Vec<u8>> {
    if frame.id > 0x7FF {
        return Err(TransportError::Other(format!(
            "SLCAN: extended (29-bit) ID 0x{:X} not supported by v1 flasher",
            frame.id
        )));
    }
    let len = frame.len as usize;
    if len > CanFrame::MAX_LEN {
        return Err(TransportError::Other(format!(
            "SLCAN: frame len {len} exceeds classic-CAN max 8"
        )));
    }

    let mut buf = Vec::with_capacity(5 + len * 2);
    buf.push(b't');
    // 3 upper-case hex chars for the 11-bit ID, zero-padded.
    write!(buf, "{:03X}", frame.id).expect("writing to Vec cannot fail");
    // 1 hex char for length.
    write!(buf, "{len:X}").expect("writing to Vec cannot fail");
    // Payload as 2*len upper-case hex chars.
    for &b in frame.payload() {
        write!(buf, "{b:02X}").expect("writing to Vec cannot fail");
    }
    buf.push(b'\r');
    Ok(buf)
}

/// Parse outcome of one line of adapter output. Not every line is a
/// frame — responses to commands are single-byte status lines.
#[derive(Debug, PartialEq, Eq)]
pub enum SlcanLine {
    /// `t…` line decoded to a classic-CAN frame.
    Frame(CanFrame),
    /// Bare `\r` — the previous command succeeded.
    Ack,
    /// `\x07` (BEL) — the previous command failed.
    Nack,
    /// Something else — an unknown / unsupported line. The SLCAN
    /// spec reserves a few letters for FD / extended / remote /
    /// timestamp / version replies; we surface them as `Unknown`
    /// rather than silently dropping so tests can assert on them.
    Unknown(Vec<u8>),
}

/// Parse a single `\r`-terminated line (**without** the trailing CR).
/// Returns the interpreted outcome or an error on a malformed `t…`
/// line. Empty input is treated as `Ack` — the SLCAN spec uses a
/// bare `\r` for a success response.
pub fn parse_line(line: &[u8]) -> Result<SlcanLine> {
    if line.is_empty() {
        return Ok(SlcanLine::Ack);
    }
    match line[0] {
        0x07 => Ok(SlcanLine::Nack),
        b't' => parse_t_frame(&line[1..]).map(SlcanLine::Frame),
        // Known SLCAN replies we don't consume today.
        b'T' | b'r' | b'R' | b'd' | b'D' | b'V' | b'N' | b'F' => {
            Ok(SlcanLine::Unknown(line.to_vec()))
        }
        _ => Ok(SlcanLine::Unknown(line.to_vec())),
    }
}

fn parse_t_frame(rest: &[u8]) -> Result<CanFrame> {
    // `t<III><L><data>`: at minimum 4 chars (ID + len, data can be
    // zero-length).
    if rest.len() < 4 {
        return Err(TransportError::Other(format!(
            "SLCAN: 't' frame too short ({} bytes)",
            rest.len()
        )));
    }
    let id = parse_hex_u16(&rest[..3])?;
    let len = parse_hex_nibble(rest[3])?;
    if len > 8 {
        return Err(TransportError::Other(format!(
            "SLCAN: 't' frame DLC {len} > 8"
        )));
    }
    let expected = len as usize * 2;
    if rest.len() < 4 + expected {
        return Err(TransportError::Other(format!(
            "SLCAN: 't' frame payload truncated (have {}, need {})",
            rest.len() - 4,
            expected
        )));
    }
    let mut data = [0u8; 8];
    for (i, slot) in data.iter_mut().enumerate().take(len as usize) {
        let off = 4 + i * 2;
        *slot = parse_hex_byte(&rest[off..off + 2])?;
    }
    Ok(CanFrame { id, data, len })
}

fn parse_hex_nibble(b: u8) -> Result<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        _ => Err(TransportError::Other(format!(
            "SLCAN: non-hex character 0x{b:02X}"
        ))),
    }
}

fn parse_hex_byte(slice: &[u8]) -> Result<u8> {
    Ok((parse_hex_nibble(slice[0])? << 4) | parse_hex_nibble(slice[1])?)
}

fn parse_hex_u16(slice: &[u8]) -> Result<u16> {
    let mut out = 0u16;
    for &b in slice {
        out = (out << 4) | u16::from(parse_hex_nibble(b)?);
    }
    Ok(out)
}

// ---- Bitrate mapping ----

fn bitrate_command(nominal_bps: u32) -> Option<&'static str> {
    match nominal_bps {
        10_000 => Some("S0"),
        20_000 => Some("S1"),
        50_000 => Some("S2"),
        100_000 => Some("S3"),
        125_000 => Some("S4"),
        250_000 => Some("S5"),
        500_000 => Some("S6"),
        800_000 => Some("S7"),
        1_000_000 => Some("S8"),
        _ => None,
    }
}

// ---- Reader thread ----

fn reader_loop(
    port: Arc<StdMutex<Box<dyn SerialPort>>>,
    tx: mpsc::Sender<CanFrame>,
    shutdown: Arc<AtomicBool>,
) {
    let mut line_buf: Vec<u8> = Vec::with_capacity(64);
    let mut scratch = [0u8; 256];

    loop {
        if shutdown.load(Ordering::SeqCst) {
            trace!("slcan reader: shutdown flag set, exiting");
            return;
        }

        let read_result = {
            let mut port = match port.lock() {
                Ok(p) => p,
                Err(_) => {
                    warn!("slcan reader: port mutex poisoned, exiting");
                    return;
                }
            };
            port.read(&mut scratch)
        };

        match read_result {
            Ok(0) => continue, // No bytes available; loop back and re-check shutdown.
            Ok(n) => {
                for &byte in &scratch[..n] {
                    if byte == b'\r' {
                        match parse_line(&line_buf) {
                            Ok(SlcanLine::Frame(frame)) => {
                                if tx.blocking_send(frame).is_err() {
                                    // Receiver dropped — backend is gone.
                                    return;
                                }
                            }
                            Ok(SlcanLine::Ack | SlcanLine::Nack) => {
                                // TX acks / nacks aren't observed by
                                // the reader path today — `send` fires
                                // the command and doesn't wait. In a
                                // stricter implementation we'd route
                                // these back to a per-command oneshot;
                                // that's an enhancement we can add
                                // when it starts mattering.
                            }
                            Ok(SlcanLine::Unknown(line)) => {
                                trace!(?line, "slcan reader: unknown line");
                            }
                            Err(err) => {
                                warn!(?err, ?line_buf, "slcan reader: line parse failed");
                            }
                        }
                        line_buf.clear();
                    } else {
                        // SLCAN lines are never longer than ~22 bytes
                        // (`t` + 3 id + 1 len + 16 hex data). Guard
                        // against runaway buffers from serial garbage.
                        if line_buf.len() < 64 {
                            line_buf.push(byte);
                        } else {
                            warn!("slcan reader: line buffer overflow, resetting");
                            line_buf.clear();
                        }
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut => continue,
            Err(e) => {
                warn!(?e, "slcan reader: read error, exiting");
                return;
            }
        }
    }
}

// ---- Open-sequence helpers ----

fn send_command_and_wait_ack(port: &mut dyn SerialPort, command: &str) -> Result<()> {
    port.write_all(command.as_bytes())?;
    port.flush()?;
    wait_for_ack(port, command)
}

fn wait_for_ack(port: &mut dyn SerialPort, command: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + COMMAND_ACK_TIMEOUT;
    let mut byte = [0u8; 1];
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(TransportError::Other(format!(
                "SLCAN: no ACK for command '{}' within {:?}",
                command.trim_end_matches('\r'),
                COMMAND_ACK_TIMEOUT
            )));
        }
        match port.read(&mut byte) {
            Ok(0) => continue,
            Ok(_) => match byte[0] {
                b'\r' => return Ok(()),
                0x07 => {
                    return Err(TransportError::Other(format!(
                        "SLCAN: adapter rejected command '{}'",
                        command.trim_end_matches('\r')
                    )));
                }
                _ => continue, // stale data from before the handshake; ignore
            },
            Err(e) if e.kind() == io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(TransportError::Io(e)),
        }
    }
}

fn drain(port: &mut dyn SerialPort) {
    let mut scratch = [0u8; 64];
    let end = std::time::Instant::now() + Duration::from_millis(50);
    while std::time::Instant::now() < end {
        match port.read(&mut scratch) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Encoder ----

    fn frame(id: u16, data: &[u8]) -> CanFrame {
        CanFrame::new(id, data).unwrap()
    }

    #[test]
    fn encode_standard_frame_with_payload() {
        let f = frame(0x003, &[0x01, 0x00, 0x01]);
        let bytes = encode_frame(&f).unwrap();
        assert_eq!(bytes, b"t0033010001\r");
    }

    #[test]
    fn encode_uses_uppercase_hex() {
        let f = frame(0x7FF, &[0xAB, 0xCD]);
        let bytes = encode_frame(&f).unwrap();
        assert_eq!(bytes, b"t7FF2ABCD\r");
    }

    #[test]
    fn encode_empty_payload_has_length_nibble_zero() {
        let f = frame(0x100, &[]);
        let bytes = encode_frame(&f).unwrap();
        assert_eq!(bytes, b"t1000\r");
    }

    #[test]
    fn encode_full_eight_byte_payload() {
        let f = frame(0x123, &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]);
        let bytes = encode_frame(&f).unwrap();
        assert_eq!(bytes, b"t12380102030405060708\r");
    }

    #[test]
    fn encode_rejects_extended_id() {
        let f = CanFrame {
            id: 0x800, // bit 11 set
            data: [0u8; 8],
            len: 0,
        };
        assert!(matches!(encode_frame(&f), Err(TransportError::Other(_))));
    }

    // ---- Parser ----

    #[test]
    fn parse_ack_from_empty_line() {
        assert_eq!(parse_line(&[]).unwrap(), SlcanLine::Ack);
    }

    #[test]
    fn parse_nack_from_bel() {
        assert_eq!(parse_line(&[0x07]).unwrap(), SlcanLine::Nack);
    }

    #[test]
    fn parse_standard_frame() {
        let got = parse_line(b"t12380102030405060708").unwrap();
        match got {
            SlcanLine::Frame(f) => {
                assert_eq!(f.id, 0x123);
                assert_eq!(f.len, 8);
                assert_eq!(&f.data[..], &[1, 2, 3, 4, 5, 6, 7, 8]);
            }
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[test]
    fn parse_empty_payload_frame() {
        let got = parse_line(b"t1000").unwrap();
        match got {
            SlcanLine::Frame(f) => {
                assert_eq!(f.id, 0x100);
                assert_eq!(f.len, 0);
            }
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[test]
    fn parse_lowercase_hex_accepted() {
        // Per the SLCAN de-facto spec upper-case is preferred, but
        // some adapters emit lower-case. Be lenient on RX.
        let got = parse_line(b"t7ff2abcd").unwrap();
        match got {
            SlcanLine::Frame(f) => {
                assert_eq!(f.id, 0x7FF);
                assert_eq!(f.len, 2);
                assert_eq!(&f.data[..2], &[0xAB, 0xCD]);
            }
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_truncated_payload() {
        // Claims DLC=4 but only provides 3 bytes of hex.
        let err = parse_line(b"t12340102030").unwrap_err();
        assert!(matches!(err, TransportError::Other(_)));
    }

    #[test]
    fn parse_rejects_bad_hex_in_id() {
        let err = parse_line(b"tGGG0").unwrap_err();
        assert!(matches!(err, TransportError::Other(_)));
    }

    #[test]
    fn parse_rejects_dlc_over_eight() {
        // Len nibble = 9. Even if we supplied 18 bytes, DLC > 8 is
        // always invalid on classic CAN.
        let err = parse_line(b"t1239010203040506070809").unwrap_err();
        assert!(matches!(err, TransportError::Other(_)));
    }

    #[test]
    fn parse_extended_frame_surfaces_as_unknown() {
        // `T…` is an extended (29-bit) frame — v1 doesn't consume
        // these, but we surface them so tests catch unexpected
        // traffic.
        let got = parse_line(b"T00000100000").unwrap();
        assert!(matches!(got, SlcanLine::Unknown(_)));
    }

    // ---- Round-trip ----

    #[test]
    fn encode_then_parse_round_trips() {
        let originals = [
            frame(0x003, &[0x01, 0x00, 0x01]),
            frame(0x7FF, &[0xAB, 0xCD]),
            frame(0x100, &[]),
            frame(0x230, &[0x10, 0xFE]),
            frame(0x1EF, &[0xAA; 8]),
        ];
        for f in originals {
            let encoded = encode_frame(&f).unwrap();
            // Strip the trailing \r before handing to the parser.
            assert_eq!(encoded.last(), Some(&b'\r'));
            let line = &encoded[..encoded.len() - 1];
            // Also strip the leading `t` — parse_line expects the
            // raw line including the tag byte.
            match parse_line(line).unwrap() {
                SlcanLine::Frame(parsed) => assert_eq!(parsed, f),
                other => panic!("expected Frame, got {other:?}"),
            }
        }
    }

    // ---- Bitrate mapping ----

    #[test]
    fn bitrate_map_covers_standard_rates() {
        assert_eq!(bitrate_command(10_000), Some("S0"));
        assert_eq!(bitrate_command(500_000), Some("S6"));
        assert_eq!(bitrate_command(1_000_000), Some("S8"));
    }

    #[test]
    fn bitrate_map_rejects_arbitrary_rate() {
        assert!(bitrate_command(33_333).is_none());
        assert!(bitrate_command(250_001).is_none());
        assert!(bitrate_command(0).is_none());
    }

    // ---- Detection filter ----

    #[test]
    fn looks_like_slcan_accepts_canonical_canable() {
        assert!(looks_like_slcan(
            USB_VID_CANABLE,
            USB_PID_CANABLE,
            Some("CANable"),
        ));
    }

    #[test]
    fn looks_like_slcan_accepts_canonical_cantact() {
        assert!(looks_like_slcan(
            USB_VID_CANTACT,
            USB_PID_CANTACT,
            Some("CANtact Pro"),
        ));
    }

    #[test]
    fn looks_like_slcan_accepts_protofusion_labs_canable_fork() {
        // The Protofusion Labs / normaldotcom CANable build — same
        // slcan protocol, different USB descriptor. This is the
        // variant that triggered fix/5; the literal product name
        // is the one the CANable 2.0 we tested on actually emits.
        assert!(looks_like_slcan(
            USB_VID_PROTOFUSION_CANABLE,
            USB_PID_PROTOFUSION_CANABLE,
            Some("CANable 9fddea4 github_com_norm"),
        ));
    }

    #[test]
    fn looks_like_slcan_accepts_unknown_vid_pid_with_canable_in_product_name() {
        // Future fork with yet another VID/PID but the brand name
        // still in the descriptor. Better to show it than hide it —
        // the backend opens the port regardless of VID/PID.
        assert!(looks_like_slcan(
            0x1234,
            0x5678,
            Some("Random CANable Clone v7"),
        ));
        assert!(looks_like_slcan(0x0000, 0x0000, Some("Some CANtact Mk2")));
    }

    #[test]
    fn looks_like_slcan_is_case_insensitive_on_product_name() {
        assert!(looks_like_slcan(0x0000, 0x0000, Some("canable")));
        assert!(looks_like_slcan(0x0000, 0x0000, Some("CANABLE")));
        assert!(looks_like_slcan(0x0000, 0x0000, Some("CANtAcT")));
    }

    #[test]
    fn looks_like_slcan_rejects_unrelated_usb_serial_device() {
        // Arduino / ESP / generic modem with no CANable hint —
        // don't pollute the list.
        assert!(!looks_like_slcan(0x2341, 0x0043, Some("Arduino Uno")));
        assert!(!looks_like_slcan(0x0000, 0x0000, None));
        assert!(!looks_like_slcan(0x0000, 0x0000, Some("USB Modem ACM")));
    }

    // ---- Description formatter ----

    fn usb_info(
        vid: u16,
        pid: u16,
        manufacturer: Option<&str>,
        product: Option<&str>,
    ) -> serialport::UsbPortInfo {
        serialport::UsbPortInfo {
            vid,
            pid,
            serial_number: None,
            manufacturer: manufacturer.map(str::to_string),
            product: product.map(str::to_string),
        }
    }

    #[test]
    fn description_labels_protofusion_labs_canable_cleanly() {
        // With both strings present the formatter takes the
        // "{manufacturer} {product}" path — verify it doesn't mangle
        // the Protofusion descriptor.
        let usb = usb_info(
            USB_VID_PROTOFUSION_CANABLE,
            USB_PID_PROTOFUSION_CANABLE,
            Some("Protofusion Labs"),
            Some("CANable 9fddea4 github_com_norm"),
        );
        let desc = slcan_description_for("/dev/cu.usbmodem1101", &usb);
        assert!(desc.starts_with("Protofusion Labs CANable"), "got: {desc}");
        assert!(desc.contains("USB ad50:60c4"), "got: {desc}");
    }

    #[test]
    fn description_falls_back_to_canable_label_when_strings_missing() {
        // USB descriptor with no product/manufacturer strings —
        // the Protofusion VID/PID should still yield "CANable" not
        // the generic "SLCAN adapter".
        let usb = usb_info(
            USB_VID_PROTOFUSION_CANABLE,
            USB_PID_PROTOFUSION_CANABLE,
            None,
            None,
        );
        let desc = slcan_description_for("/dev/cu.usbmodem1101", &usb);
        assert!(desc.starts_with("CANable"), "got: {desc}");
    }
}
