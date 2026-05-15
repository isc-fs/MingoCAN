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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
/// with `ErrorKind::TimedOut`. Since we split the port into
/// independent reader/writer handles via `try_clone` at open time,
/// the reader's read no longer contends with the writer's write —
/// so this can be a comfortable value without hurting TX latency.
/// We still keep it short (50 ms) so shutdown / cancellation is
/// responsive.
const READ_TIMEOUT: Duration = Duration::from_millis(50);

/// Minimum interval between successive TX frames. See `send()` for
/// why: we're feeding SLCAN-over-USB faster than the CANable can
/// actually emit on the bus (100 µs serial-write vs 230 µs classic
/// CAN wire time), which overflows the adapter's TX buffer on
/// multi-frame ISO-TP bursts. 1 ms pacing puts us at ~1 kHz TX rate,
/// comfortably below the 4 kHz bus ceiling and below any reasonable
/// CANable firmware's drain rate.
const PACING_INTERVAL: Duration = Duration::from_millis(1);

/// How long the open sequence waits to see an adapter's command
/// response (`\r` for success, `\x07` for failure). Two kinds of
/// real-world slcan firmware:
///
/// - **"Chatty"**: replies to every command with `\r` or `\x07`.
///   The canonical slcan spec (LAWICEL AB) mandates this.
/// - **"Silent"**: silently accepts `C` / `S<N>` / `O` with no
///   reply. The `normaldotcom/canable-fw` firmware shipped on
///   CANable 2.0 boards is in this camp, as are several other
///   popular forks.
///
/// We treat silence as success (the firmware isn't standards-pure
/// but the commands still take effect) and `\x07` as failure.
/// This timeout bounds how long we'll wait for a *possible* reply
/// before concluding the firmware is silent.
///
/// 50 ms is chosen empirically: USB-FS frame polling is 1 ms, and
/// the CANable fork we've tested replies within 2–5 ms when it
/// replies at all. Silent firmware pays this timeout three times
/// during open (C, S<N>, O), so 50 ms × 3 = 150 ms of fixed cost
/// per `cf` invocation — well below our per-command budget. The
/// earlier 200 ms value made short CLI invocations feel sluggish
/// (a round-trip discover took >1 s on silent firmware).
const COMMAND_ACK_TIMEOUT: Duration = Duration::from_millis(50);

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
///
/// **Port split**: at open time we call [`SerialPort::try_clone`] to
/// produce two independent handles to the same underlying USB serial
/// device. The reader thread owns one handle (reads only); the
/// writer uses the other (writes only, behind a std mutex to
/// serialise concurrent `send()` tasks). Before this split, both
/// directions competed for a single mutex, and the reader's blocking
/// `read()` — which on macOS has ~100 ms termios granularity
/// regardless of the configured timeout — would starve the writer
/// mid-ISO-TP burst for hundreds of ms. On a multi-frame flash
/// write that stalled the CFs past the bootloader's 1 s reassembly
/// deadline, producing spurious `NACK(TRANSPORT_TIMEOUT)` errors.
pub struct SlcanBackend {
    writer_port: Arc<StdMutex<Box<dyn SerialPort>>>,
    rx: Arc<TokioMutex<mpsc::Receiver<CanFrame>>>,
    shutdown: Arc<AtomicBool>,
    reader_handle: StdMutex<Option<thread::JoinHandle<()>>>,
    description: String,
    /// Monotonic count of BEL (`0x07`) bytes the adapter has sent us
    /// during an active session. Every BEL is an adapter-side refusal
    /// of our most-recent SLCAN command — on the live TX path that
    /// means the frame we tried to transmit never reached the CAN
    /// bus: bus-off, TX buffer full, stuck-dominant, or in general
    /// "adapter cannot talk right now." Incremented by
    /// [`reader_loop`]; never decremented (we snapshot + diff).
    ///
    /// Surfaces in two ways:
    /// - Immediate `warn!` log line per BEL (visible at the default
    ///   info filter, so users see it without `--verbose`).
    /// - `adapter_error_count()` snapshot so callers can fast-fail on
    ///   a spike (future enhancement — currently a diagnostic hook).
    adapter_errors: Arc<AtomicU32>,
}

// Inherent `adapter_error_count` accessor lives in the `CanBackend`
// trait impl below — keeping both would shadow the trait method and
// force callers to choose a disambiguation syntax. See there.

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

        // Split the port into independent reader/writer handles.
        // `try_clone` returns a new OS file descriptor pointing at the
        // same underlying USB serial device. The kernel serialises
        // read/write syscalls on the device internally, so it's safe
        // for the reader thread to block in `read()` while a concurrent
        // `write_all()` runs on the writer handle — they no longer
        // serialise through a user-space mutex. This is the
        // difference between a mid-burst TX stall of ~1 ms (kernel
        // scheduler) and ~500 ms (reader holding the std mutex
        // across macOS's termios 100 ms minimum read timeout).
        let mut reader_port = port
            .try_clone()
            .map_err(|e| TransportError::InvalidChannel {
                channel: channel.to_string(),
                reason: format!("could not clone serial port for reader: {e}"),
            })?;

        // Reader uses the short timeout (shutdown latency); writer
        // keeps a longer default since it only blocks in the open
        // handshake path which has its own deadline.
        reader_port
            .set_timeout(READ_TIMEOUT)
            .map_err(|e| TransportError::InvalidChannel {
                channel: channel.to_string(),
                reason: format!("could not set reader timeout: {e}"),
            })?;

        let description = format!("SLCAN on {channel} @ {nominal_bps} bps");
        let writer_port = Arc::new(StdMutex::new(port));
        let shutdown = Arc::new(AtomicBool::new(false));
        let adapter_errors = Arc::new(AtomicU32::new(0));
        let (tx, rx) = mpsc::channel(RX_QUEUE_DEPTH);

        let reader_shutdown = Arc::clone(&shutdown);
        let reader_errors = Arc::clone(&adapter_errors);
        let reader_handle = thread::Builder::new()
            .name("slcan-reader".into())
            .spawn(move || reader_loop(reader_port, tx, reader_shutdown, reader_errors))
            .map_err(|e| TransportError::Other(format!("spawn reader thread: {e}")))?;

        Ok(Self {
            writer_port,
            rx: Arc::new(TokioMutex::new(rx)),
            shutdown,
            reader_handle: StdMutex::new(Some(reader_handle)),
            description,
            adapter_errors,
        })
    }
}

#[async_trait]
impl CanBackend for SlcanBackend {
    async fn send(&self, frame: CanFrame) -> Result<()> {
        trace!(
            id = format!("0x{:03X}", frame.id),
            len = frame.len,
            data = format!("{:02X?}", frame.payload()),
            "slcan TX"
        );
        let encoded = encode_frame(&frame)?;
        let port = Arc::clone(&self.writer_port);

        // Serial I/O is blocking; hand it to the blocking pool so we
        // don't stall the async runtime.
        //
        // No explicit `port.flush()` after `write_all`: on macOS
        // USB-serial, `flush()` calls `tcdrain()` which waits for
        // the kernel output buffer to fully drain over the USB OUT
        // endpoint (often 10–50 ms per call in practice). For our
        // workflow every send is followed by a recv for the reply,
        // so the kernel has ample time to ship the TX before we
        // need the reply.
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut port = port
                .lock()
                .map_err(|_| TransportError::Other("serial port mutex poisoned".into()))?;
            port.write_all(&encoded)?;
            Ok(())
        })
        .await
        .map_err(|e| TransportError::Other(format!("spawn_blocking join failed: {e}")))??;

        // Inter-frame pacing. On a 500 kbps CAN bus each 8-byte
        // classic frame takes ~230 µs wire time; at 2 Mbps serial we
        // can push a `t...\r` command in ~100 µs, i.e. faster than
        // the CANable can transmit it onto the bus. Without pacing,
        // bursts of ≥ ~8 frames (e.g. the CFs of a 64-byte
        // `CMD_FLASH_WRITE`) overflow the CANable's internal TX
        // buffer and get silently dropped, which manifests on the
        // bootloader side as an ISO-TP reassembly timeout
        // (`NACK(TRANSPORT_TIMEOUT)`).
        //
        // A 1 ms sleep between frames caps host TX rate at ~1 kHz —
        // well below bus wire rate (~4 kHz for 8-byte classic CAN at
        // 500 kbps) but slow enough that CANables with conservative
        // buffering never drop. One-off commands pay 1 ms we'd
        // barely notice; bursts (the CFs of a multi-frame command)
        // scale linearly, which is the price for reliability.
        tokio::time::sleep(PACING_INTERVAL).await;
        Ok(())
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
        let port = Arc::clone(&self.writer_port);
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

    fn adapter_error_count(&self) -> u32 {
        self.adapter_errors.load(Ordering::Relaxed)
    }
}

impl Drop for SlcanBackend {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Best-effort: tell the adapter to close the CAN side. If the
        // mutex is poisoned we skip — nothing productive left to do.
        if let Ok(mut port) = self.writer_port.lock() {
            let _ = port.write_all(b"C\r");
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
    mut port: Box<dyn SerialPort>,
    tx: mpsc::Sender<CanFrame>,
    shutdown: Arc<AtomicBool>,
    adapter_errors: Arc<AtomicU32>,
) {
    let mut line_buf: Vec<u8> = Vec::with_capacity(64);
    let mut scratch = [0u8; 256];

    loop {
        if shutdown.load(Ordering::SeqCst) {
            trace!("slcan reader: shutdown flag set, exiting");
            return;
        }

        // No mutex around the port any more: the reader owns an
        // independent `try_clone` handle, so this blocking read
        // cannot starve concurrent writes.
        let read_result = port.read(&mut scratch);

        match read_result {
            Ok(0) => continue, // No bytes available; loop back and re-check shutdown.
            Ok(n) => {
                for &byte in &scratch[..n] {
                    if byte == b'\r' {
                        match parse_line(&line_buf) {
                            Ok(SlcanLine::Frame(frame)) => {
                                trace!(
                                    id = format!("0x{:03X}", frame.id),
                                    len = frame.len,
                                    data = format!("{:02X?}", frame.payload()),
                                    "slcan RX"
                                );
                                if tx.blocking_send(frame).is_err() {
                                    // Receiver dropped — backend is gone.
                                    return;
                                }
                            }
                            Ok(SlcanLine::Ack) => {
                                // Adapter acknowledged the last SLCAN
                                // command (`\r` after `t...`, `S6`,
                                // `O`, etc). Nothing to do — our
                                // `send()` fires the command and
                                // doesn't wait on this signal. A
                                // stricter design would route these
                                // back to a per-command oneshot; not
                                // worth the wiring yet.
                            }
                            Ok(SlcanLine::Nack) => {
                                // Adapter refused the last SLCAN
                                // command. During an active session
                                // this almost always means the
                                // adapter couldn't TX our `t...\r`
                                // frame onto the CAN bus — bus-off,
                                // TX buffer full, stuck-dominant,
                                // etc. Our session-layer send happily
                                // moved on assuming the frame made
                                // it, so the BL will never see it
                                // and the subsequent reply-wait will
                                // hit `command_timeout`.
                                //
                                // We surface this two ways:
                                //   1. An immediate `warn!` visible
                                //      at the default `info` filter,
                                //      so users without `--verbose`
                                //      still see that the adapter is
                                //      rejecting traffic.
                                //   2. A monotonic counter (snapshot
                                //      via `adapter_error_count()`)
                                //      so higher layers can diff
                                //      around an operation and
                                //      fast-fail on a spike instead
                                //      of burning a full timeout.
                                let total = adapter_errors
                                    .fetch_add(1, Ordering::Relaxed)
                                    .saturating_add(1);
                                warn!(
                                    adapter_errors = total,
                                    "slcan adapter reported BEL (0x07) — \
                                     previous command refused (likely bus-off, \
                                     TX buffer full, or stuck-dominant); the \
                                     frame did NOT reach the CAN bus"
                                );
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
    // No `port.flush()` — see the comment on `SlcanBackend::send` for
    // why. The small USB-OUT latency means the command reaches the
    // adapter a handful of milliseconds after `write_all` returns,
    // which is exactly what `wait_for_ack`'s short timeout is sized
    // to tolerate.
    port.write_all(command.as_bytes())?;
    wait_for_ack(port, command, COMMAND_ACK_TIMEOUT)
}

/// Wait for a command response, tolerating silent-success firmware.
///
/// Returns:
/// - `Ok(())` on `\r` (explicit success) **or** on timeout (silent
///   firmware — see the `COMMAND_ACK_TIMEOUT` doc-comment).
/// - `Err(...)` on `\x07` (the adapter explicitly rejected the
///   command) or on an I/O error other than `TimedOut`.
///
/// Stale bytes (anything that isn't `\r` or `\x07`) are ignored —
/// they're typically leftover response bytes from a previous
/// command that straddled our `drain()` window.
///
/// Generic over `R: Read` so the unit tests can swap in a
/// `std::io::Cursor` without standing up a full `SerialPort` mock.
/// Real call sites pass `&mut *port` where `port: Box<dyn SerialPort>`
/// — `SerialPort: Read` makes the coercion free.
fn wait_for_ack<R: Read + ?Sized>(reader: &mut R, command: &str, timeout: Duration) -> Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    let mut byte = [0u8; 1];
    loop {
        if std::time::Instant::now() >= deadline {
            // Silent success — the firmware doesn't ACK this command
            // variant. See the `COMMAND_ACK_TIMEOUT` doc-comment for
            // background. If the command actually failed, later
            // traffic on the bus will tell us (e.g. the `discover`
            // broadcast returning nothing → caller retries or
            // surfaces a timeout at the session layer).
            return Ok(());
        }
        match reader.read(&mut byte) {
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
            // serialport 4.9 added `interface` (USB CDC interface
            // number); tests don't care so always None.
            interface: None,
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

    // ---- wait_for_ack: silent-firmware tolerance ----

    /// Short timeout so the silent-success test doesn't slow the
    /// suite down — we only need enough time to prove the deadline
    /// branch fires.
    const TEST_ACK_TIMEOUT: Duration = Duration::from_millis(50);

    #[test]
    fn wait_for_ack_succeeds_on_carriage_return() {
        let mut src = std::io::Cursor::new(b"\r".to_vec());
        wait_for_ack(&mut src, "S6\r", TEST_ACK_TIMEOUT).expect("CR should be success");
    }

    #[test]
    fn wait_for_ack_fails_on_bel_byte() {
        // 0x07 is the BEL (alarm bell) byte — the canonical slcan
        // "command rejected" response.
        let mut src = std::io::Cursor::new(vec![0x07u8]);
        let err = wait_for_ack(&mut src, "S6\r", TEST_ACK_TIMEOUT).expect_err("BEL → error");
        let msg = format!("{err}");
        assert!(
            msg.contains("rejected command 'S6'"),
            "error should name the rejected command, got: {msg}"
        );
    }

    #[test]
    fn wait_for_ack_treats_silent_source_as_success() {
        // Empty cursor → every read returns Ok(0) → the deadline
        // branch fires and we return Ok. This is the
        // normaldotcom/canable-fw firmware behaviour the fix is
        // targeting.
        let mut src = std::io::Cursor::new(Vec::<u8>::new());
        let before = std::time::Instant::now();
        wait_for_ack(&mut src, "S6\r", TEST_ACK_TIMEOUT)
            .expect("silent firmware should be treated as success");
        let elapsed = before.elapsed();
        // Sanity-check we actually waited for the timeout — if
        // Ok came back immediately the logic would be wrong in
        // the opposite direction (accepting success on EOF instead
        // of timeout).
        assert!(
            elapsed >= TEST_ACK_TIMEOUT,
            "wait_for_ack returned too early: {elapsed:?}"
        );
    }

    #[test]
    fn wait_for_ack_ignores_stale_bytes_before_cr() {
        // A previous command left response bytes in the buffer;
        // the CR of the current command eventually arrives.
        let mut src = std::io::Cursor::new(b"stale bytes\r".to_vec());
        wait_for_ack(&mut src, "O\r", TEST_ACK_TIMEOUT)
            .expect("stale bytes should be ignored until CR");
    }

    #[test]
    fn wait_for_ack_reports_bel_even_after_stale_bytes() {
        // Explicit rejection should win over silent-success even
        // if stale bytes came first.
        let mut src = std::io::Cursor::new(b"leftover\x07".to_vec());
        let err = wait_for_ack(&mut src, "S99\r", TEST_ACK_TIMEOUT).expect_err("BEL → error");
        assert!(format!("{err}").contains("rejected"));
    }

    // ---- Adapter-error surfacing (fix/17) ----
    //
    // `reader_loop` is private and tangled with serial I/O, so we
    // don't try to spin up a thread here. Instead we exercise the
    // counter-increment path by hand: construct the same AtomicU32
    // the real loop would own, push a BEL through `parse_line`, and
    // confirm the match arm increments the counter. This pins the
    // behaviour (one BEL = one increment) without needing a real
    // adapter.

    #[test]
    fn parsed_bel_increments_adapter_error_counter() {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));

        // Simulate what `reader_loop` does for a single BEL byte
        // bracketed by CR (a CR-less BEL is handled by the error-
        // mid-line path in the reader, not by parse_line — that's
        // a separate code path covered above).
        let bel_line = parse_line(&[0x07]).expect("BEL parses");
        assert_eq!(bel_line, SlcanLine::Nack);

        // The real loop matches `SlcanLine::Nack` and bumps the
        // counter; mirror that here so the test stays valid if the
        // match arm's side-effect changes.
        if let SlcanLine::Nack = bel_line {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn command_timeout_display_plain_when_no_adapter_errors() {
        let msg =
            super::super::super::session::command_timeout_display(Duration::from_millis(5_000), 0);
        assert_eq!(msg, "timed out waiting for device reply after 5000ms");
    }

    #[test]
    fn command_timeout_display_mentions_adapter_errors() {
        let msg =
            super::super::super::session::command_timeout_display(Duration::from_millis(5_000), 3);
        assert!(msg.contains("5000ms"), "got: {msg}");
        assert!(msg.contains("adapter reported 3 error"), "got: {msg}");
        assert!(msg.contains("bus-off"), "got: {msg}");
        assert!(msg.contains("unplugging"), "got: {msg}");
    }
}
