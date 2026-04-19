//! PCAN-Basic SDK backend — PEAK System adapters on Windows and macOS.
//!
//! Linux users go through [`super::socketcan::SocketCanBackend`]
//! instead, because the `peak_usb` kernel module exposes PCAN devices
//! as ordinary SocketCAN interfaces. This module is therefore
//! cfg-gated to `windows | macos`.
//!
//! ## Runtime dynamic loading
//!
//! We resolve the PCAN-Basic library at runtime via [`libloading`],
//! **not** at build time. That keeps the flasher a single static
//! binary that anyone can run — users who never touch `--interface
//! pcan` don't need the PEAK SDK installed at all. A missing library
//! surfaces at `open_backend` as
//! [`TransportError::AdapterMissing`] with a download pointer, not a
//! link-time failure.
//!
//! Search order (matches REQUIREMENTS.md § PcanBackend):
//!
//! 1. `$PCAN_LIB_PATH` environment variable — explicit override for
//!    unusual deployments.
//! 2. Platform default path (Windows: `PCANBasic.dll` via the normal
//!    DLL search path so `System32\` gets hit; macOS:
//!    `/usr/local/lib/libPCBUSB.dylib`).
//! 3. The executable's own directory — useful for portable bundles.
//!
//! ## Channel strings
//!
//! Accepts `PCAN_USBBUS1`..`PCAN_USBBUS16` (case-insensitive) per the
//! PEAK constant naming, plus raw hex like `0x51` for the same set
//! of values. Anything else returns
//! [`TransportError::InvalidChannel`] with the valid list.
//!
//! ## Threading model
//!
//! PCAN-Basic's `CAN_Read` is non-blocking and returns
//! `PCAN_ERROR_QRCVEMPTY` when there's no message. We run a dedicated
//! reader thread that polls `CAN_Read` in a tight loop with a short
//! sleep between empty reads, and hands decoded frames to the async
//! side through a `tokio::sync::mpsc` channel — the same shape
//! [`super::slcan::SlcanBackend`] uses.

#![cfg(any(target_os = "windows", target_os = "macos"))]

use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread;
use std::time::Duration;

use async_trait::async_trait;
use libloading::Library;
use tokio::sync::{mpsc, Mutex as TokioMutex};
use tracing::{debug, warn};

use crate::protocol::CanFrame;

use super::{CanBackend, Result, TransportError};

// ---- PCAN-Basic constants (from PCANBasic.h) ----

/// Channel handle range for USB adapters. `PCAN_USBBUS1` = `0x51`,
/// `PCAN_USBBUS16` = `0x60`.
const PCAN_USBBUS_BASE: u16 = 0x51;
const PCAN_USBBUS_MAX_INDEX: u8 = 16;

// Baud rate constants (Btr0Btr1 encoding). Values match
// PCAN_BAUD_* from PCANBasic.h.
const PCAN_BAUD_1M: u16 = 0x0014;
const PCAN_BAUD_800K: u16 = 0x0016;
const PCAN_BAUD_500K: u16 = 0x001C;
const PCAN_BAUD_250K: u16 = 0x011C;
const PCAN_BAUD_125K: u16 = 0x031C;
const PCAN_BAUD_100K: u16 = 0x432F;
const PCAN_BAUD_50K: u16 = 0x472F;
const PCAN_BAUD_33K: u16 = 0x8B2F;
const PCAN_BAUD_20K: u16 = 0x532F;
const PCAN_BAUD_10K: u16 = 0x672F;

// Status / return codes (bitmasks).
const PCAN_ERROR_OK: u32 = 0x00000;
const PCAN_ERROR_QRCVEMPTY: u32 = 0x00020;

// Message-type byte for 11-bit standard data frames.
const PCAN_MESSAGE_STANDARD: u8 = 0x00;
// Bitmask values we filter *out* — extended / remote / error / status
// frames never carry bootloader payload.
const PCAN_MESSAGE_RTR: u8 = 0x01;
const PCAN_MESSAGE_EXTENDED: u8 = 0x02;
const PCAN_MESSAGE_ERRFRAME: u8 = 0x40;
const PCAN_MESSAGE_STATUS: u8 = 0x80;

/// Background poll interval when the RX queue is empty. Keeps the
/// reader thread from pinning a CPU while still responding to
/// incoming frames within ~1 ms.
const RX_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// MPSC queue depth for the async recv side. Matches the SLCAN
/// backend.
const RX_QUEUE_DEPTH: usize = 256;

// ---- C ABI structs (match PCANBasic.h) ----

/// PCAN's classic CAN message. Field order + types mirror `TPCANMsg`
/// exactly — `#[repr(C)]` ensures the layout round-trips across the
/// FFI boundary.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct TPCANMsg {
    id: u32,
    msgtype: u8,
    len: u8,
    data: [u8; 8],
}

/// Hardware timestamp PCAN returns alongside each message. We don't
/// use it today but the API requires we supply a valid pointer.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct TPCANTimestamp {
    millis: u32,
    millis_overflow: u16,
    micros: u16,
}

// ---- Function-pointer types ----
//
// `extern "system"` picks stdcall on 32-bit Windows and cdecl
// everywhere else — matches the C `PCAN_API` macro in PCANBasic.h.

type FnInitialize = unsafe extern "system" fn(
    channel: u16,
    baudrate: u16,
    hwtype: u8,
    ioport: u32,
    interrupt: u16,
) -> u32;

type FnUninitialize = unsafe extern "system" fn(channel: u16) -> u32;

type FnRead = unsafe extern "system" fn(
    channel: u16,
    msg: *mut TPCANMsg,
    timestamp: *mut TPCANTimestamp,
) -> u32;

type FnWrite = unsafe extern "system" fn(channel: u16, msg: *mut TPCANMsg) -> u32;

type FnGetErrorText = unsafe extern "system" fn(error: u32, language: u16, buffer: *mut u8) -> u32;

// ---- Loaded API handles ----

/// One set of resolved function pointers plus a keep-alive handle on
/// the loaded library. Cheap to clone via `Arc` so the reader thread
/// and the async send path can both hold a reference.
struct PcanApi {
    _lib: Arc<Library>,
    initialize: FnInitialize,
    uninitialize: FnUninitialize,
    read: FnRead,
    write: FnWrite,
    get_error_text: FnGetErrorText,
}

impl PcanApi {
    /// Load the library, resolve every symbol we use, bundle them.
    fn load(lib: Arc<Library>) -> Result<Self> {
        // SAFETY: each `lib.get` looks up an exported symbol by name.
        // If any symbol is missing, we bubble up a descriptive error
        // and never call through the resulting pointer.
        unsafe {
            let initialize = *lib
                .get::<FnInitialize>(b"CAN_Initialize\0")
                .map_err(|e| missing_symbol("CAN_Initialize", e))?;
            let uninitialize = *lib
                .get::<FnUninitialize>(b"CAN_Uninitialize\0")
                .map_err(|e| missing_symbol("CAN_Uninitialize", e))?;
            let read = *lib
                .get::<FnRead>(b"CAN_Read\0")
                .map_err(|e| missing_symbol("CAN_Read", e))?;
            let write = *lib
                .get::<FnWrite>(b"CAN_Write\0")
                .map_err(|e| missing_symbol("CAN_Write", e))?;
            let get_error_text = *lib
                .get::<FnGetErrorText>(b"CAN_GetErrorText\0")
                .map_err(|e| missing_symbol("CAN_GetErrorText", e))?;
            Ok(Self {
                _lib: lib,
                initialize,
                uninitialize,
                read,
                write,
                get_error_text,
            })
        }
    }

    fn error_text(&self, code: u32) -> String {
        // PCAN's buffer is documented as at least 256 bytes.
        let mut buf = [0u8; 256];
        // Language 0x09 = English (Windows LANGID). PCBUSB on macOS
        // ignores the language parameter.
        let lang: u16 = 0x09;
        let rc = unsafe { (self.get_error_text)(code, lang, buf.as_mut_ptr()) };
        if rc != PCAN_ERROR_OK {
            return format!("PCAN error 0x{code:05X} (get-error-text failed: 0x{rc:05X})");
        }
        // Trim to the first NUL and lossy-UTF-8 decode.
        let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..len]).into_owned()
    }
}

fn missing_symbol(name: &'static str, err: libloading::Error) -> TransportError {
    TransportError::AdapterMissing {
        name: "pcan",
        reason: format!(
            "PCAN-Basic library is missing required symbol `{name}`: {err}. \
             Is this an older / partial PCAN-Basic install? Try reinstalling \
             from https://www.peak-system.com/Software-APIs.305.0.html"
        ),
    }
}

// ---- Backend ----

/// PCAN-Basic-backed implementation of [`CanBackend`].
pub struct PcanBackend {
    api: Arc<PcanApi>,
    channel: u16,
    rx: Arc<TokioMutex<mpsc::Receiver<CanFrame>>>,
    shutdown: Arc<AtomicBool>,
    reader_handle: StdMutex<Option<thread::JoinHandle<()>>>,
    description: String,
}

impl PcanBackend {
    /// Open `channel` at `bitrate`. See the module docs for the
    /// channel string format and supported bitrate list.
    pub fn open(channel_str: &str, nominal_bps: u32) -> Result<Self> {
        let channel = parse_channel(channel_str)?;
        let baud =
            bitrate_to_pcan_code(nominal_bps).ok_or_else(|| TransportError::InvalidChannel {
                channel: channel_str.to_string(),
                reason: format!(
                    "PCAN: unsupported bitrate {nominal_bps} bps — supported: \
                     10k / 20k / 33k / 50k / 100k / 125k / 250k / 500k / 800k / 1M"
                ),
            })?;

        let lib = load_library()?;
        let api = PcanApi::load(Arc::new(lib))?;

        // CAN_Initialize(channel, baudrate, hwtype=0, ioport=0, interrupt=0)
        // The last three are ignored for USB adapters.
        let rc = unsafe { (api.initialize)(channel, baud, 0, 0, 0) };
        if rc != PCAN_ERROR_OK {
            return Err(TransportError::InvalidChannel {
                channel: channel_str.to_string(),
                reason: format!(
                    "PCAN_Initialize(0x{channel:02X}, 0x{baud:04X}) failed: {} (code 0x{rc:05X})",
                    api.error_text(rc)
                ),
            });
        }

        let description = format!("PCAN (channel 0x{channel:02X} @ {nominal_bps} bps)");
        let api = Arc::new(api);
        let shutdown = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel(RX_QUEUE_DEPTH);

        let reader_api = Arc::clone(&api);
        let reader_shutdown = Arc::clone(&shutdown);
        let reader_handle = thread::Builder::new()
            .name("pcan-reader".into())
            .spawn(move || reader_loop(reader_api, channel, tx, reader_shutdown))
            .map_err(|e| TransportError::Other(format!("spawn reader thread: {e}")))?;

        Ok(Self {
            api,
            channel,
            rx: Arc::new(TokioMutex::new(rx)),
            shutdown,
            reader_handle: StdMutex::new(Some(reader_handle)),
            description,
        })
    }
}

#[async_trait]
impl CanBackend for PcanBackend {
    async fn send(&self, frame: CanFrame) -> Result<()> {
        let mut msg = our_frame_to_tpcan(&frame)?;
        let api = Arc::clone(&self.api);
        let channel = self.channel;

        tokio::task::spawn_blocking(move || -> Result<()> {
            let rc = unsafe { (api.write)(channel, &mut msg) };
            if rc != PCAN_ERROR_OK {
                return Err(TransportError::Other(format!(
                    "PCAN_Write failed: {} (code 0x{rc:05X})",
                    api.error_text(rc)
                )));
            }
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

    async fn set_bitrate(&self, _nominal_bps: u32) -> Result<()> {
        // PCAN-Basic requires `CAN_Uninitialize` + `CAN_Initialize` to
        // switch bitrate, which would race the reader thread. The
        // flasher doesn't exercise rate changes mid-session (the
        // bootloader doesn't support it either), so no-op for now.
        // If a future caller needs this, the clean fix is to pause
        // the reader, uninit / reinit, then resume.
        debug!(
            channel = self.channel,
            "PCAN: runtime bitrate change not implemented — close and reopen the backend instead"
        );
        Ok(())
    }

    fn description(&self) -> String {
        self.description.clone()
    }
}

impl Drop for PcanBackend {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Uninitialize before joining so the reader's next poll
        // returns promptly with an error we ignore.
        let rc = unsafe { (self.api.uninitialize)(self.channel) };
        if rc != PCAN_ERROR_OK {
            warn!(
                channel = self.channel,
                code = format!("0x{rc:05X}"),
                text = %self.api.error_text(rc),
                "PCAN_Uninitialize returned an error; ignoring"
            );
        }
        if let Ok(mut handle) = self.reader_handle.lock() {
            if let Some(h) = handle.take() {
                let _ = h.join();
            }
        }
    }
}

// ---- Library loading ----

fn load_library() -> Result<Library> {
    let lib_name = if cfg!(target_os = "windows") {
        "PCANBasic.dll"
    } else {
        "libPCBUSB.dylib"
    };

    // 1. Explicit override via PCAN_LIB_PATH.
    if let Ok(override_path) = env::var("PCAN_LIB_PATH") {
        if let Ok(lib) = unsafe { Library::new(&override_path) } {
            return Ok(lib);
        }
    }

    // 2. Default path — Windows uses the OS-wide search (System32 /
    // SysWOW64) by passing the bare filename; macOS typically lands
    // at /usr/local/lib.
    let default_path = if cfg!(target_os = "windows") {
        lib_name.to_string()
    } else {
        format!("/usr/local/lib/{lib_name}")
    };
    if let Ok(lib) = unsafe { Library::new(&default_path) } {
        return Ok(lib);
    }

    // 3. Alongside the current executable — portable bundles.
    if let Ok(mut exe) = env::current_exe() {
        exe.pop();
        exe.push(lib_name);
        if let Ok(lib) = unsafe { Library::new(&exe) } {
            return Ok(lib);
        }
    }

    Err(TransportError::AdapterMissing {
        name: "pcan",
        reason: format!(
            "PCAN-Basic library `{lib_name}` not found. Searched: \
             PCAN_LIB_PATH env var, default path ({default_path}), \
             executable directory. Download and install the SDK from \
             https://www.peak-system.com/Software-APIs.305.0.html \
             — or set PCAN_LIB_PATH to an existing install."
        ),
    })
}

// ---- Reader thread ----

fn reader_loop(
    api: Arc<PcanApi>,
    channel: u16,
    tx: mpsc::Sender<CanFrame>,
    shutdown: Arc<AtomicBool>,
) {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }

        // SAFETY: we supply valid out-pointers and obey PCAN-Basic's
        // calling convention on every platform we compile for.
        let mut msg = TPCANMsg {
            id: 0,
            msgtype: 0,
            len: 0,
            data: [0; 8],
        };
        let mut ts = TPCANTimestamp::default();
        let rc = unsafe { (api.read)(channel, &mut msg, &mut ts) };

        match rc {
            PCAN_ERROR_OK => {
                // Skip non-data frames (extended / remote / error /
                // status) — the bootloader only uses 11-bit standard.
                if msg.msgtype
                    & (PCAN_MESSAGE_EXTENDED
                        | PCAN_MESSAGE_RTR
                        | PCAN_MESSAGE_ERRFRAME
                        | PCAN_MESSAGE_STATUS)
                    != 0
                {
                    continue;
                }
                let frame = match tpcan_to_our(&msg) {
                    Ok(f) => f,
                    Err(err) => {
                        warn!(?err, "pcan reader: frame conversion failed");
                        continue;
                    }
                };
                if tx.blocking_send(frame).is_err() {
                    // Async receiver dropped — the backend is gone.
                    return;
                }
            }
            PCAN_ERROR_QRCVEMPTY => {
                // No message — park briefly so we don't spin a core.
                thread::sleep(RX_POLL_INTERVAL);
            }
            other => {
                warn!(
                    code = format!("0x{other:05X}"),
                    text = %api.error_text(other),
                    "pcan reader: CAN_Read returned error status"
                );
                thread::sleep(RX_POLL_INTERVAL);
            }
        }
    }
}

// ---- Channel string parsing ----

/// Accepts `PCAN_USBBUS1..=PCAN_USBBUS16` (case-insensitive) or raw
/// hex `0x51..=0x60`. Surfaces any other input as
/// [`TransportError::InvalidChannel`].
fn parse_channel(s: &str) -> Result<u16> {
    let trimmed = s.trim();
    let upper = trimmed.to_ascii_uppercase();
    if let Some(idx_str) = upper.strip_prefix("PCAN_USBBUS") {
        let n: u8 = idx_str
            .parse()
            .map_err(|_| TransportError::InvalidChannel {
                channel: s.to_string(),
                reason: format!("PCAN channel index `{idx_str}` is not a number 1..=16"),
            })?;
        if !(1..=PCAN_USBBUS_MAX_INDEX).contains(&n) {
            return Err(TransportError::InvalidChannel {
                channel: s.to_string(),
                reason: format!("PCAN_USBBUS index out of range (1..=16): got {n}"),
            });
        }
        return Ok(PCAN_USBBUS_BASE + u16::from(n) - 1);
    }

    if let Some(hex) = upper.strip_prefix("0X") {
        let val = u16::from_str_radix(hex, 16).map_err(|_| TransportError::InvalidChannel {
            channel: s.to_string(),
            reason: format!("PCAN channel `{s}` is not a valid hex value"),
        })?;
        let max = PCAN_USBBUS_BASE + u16::from(PCAN_USBBUS_MAX_INDEX) - 1;
        if (PCAN_USBBUS_BASE..=max).contains(&val) {
            return Ok(val);
        }
        return Err(TransportError::InvalidChannel {
            channel: s.to_string(),
            reason: format!(
                "PCAN channel 0x{val:02X} is outside the supported USB range \
                 0x{PCAN_USBBUS_BASE:02X}..=0x{max:02X}"
            ),
        });
    }

    Err(TransportError::InvalidChannel {
        channel: s.to_string(),
        reason: "expected PCAN_USBBUS1..PCAN_USBBUS16 or a hex constant in 0x51..=0x60".to_string(),
    })
}

fn bitrate_to_pcan_code(bps: u32) -> Option<u16> {
    match bps {
        1_000_000 => Some(PCAN_BAUD_1M),
        800_000 => Some(PCAN_BAUD_800K),
        500_000 => Some(PCAN_BAUD_500K),
        250_000 => Some(PCAN_BAUD_250K),
        125_000 => Some(PCAN_BAUD_125K),
        100_000 => Some(PCAN_BAUD_100K),
        50_000 => Some(PCAN_BAUD_50K),
        33_333 => Some(PCAN_BAUD_33K),
        20_000 => Some(PCAN_BAUD_20K),
        10_000 => Some(PCAN_BAUD_10K),
        _ => None,
    }
}

// ---- Detection ----

/// Return a list of currently-open-able PCAN USB channels. For each
/// `PCAN_USBBUSn`, try `CAN_Initialize` / `CAN_Uninitialize` at a
/// nominal bitrate — a successful initialize means the channel is
/// real. Slow on paper (N round-trips) but N is ≤ 16 and each probe
/// is sub-millisecond in practice.
///
/// Skips entirely if the PCAN-Basic library isn't installed.
pub fn detect() -> Vec<PcanAdapterInfo> {
    let Ok(lib) = load_library() else {
        return Vec::new();
    };
    let Ok(api) = PcanApi::load(Arc::new(lib)) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for idx in 1..=PCAN_USBBUS_MAX_INDEX {
        let channel = PCAN_USBBUS_BASE + u16::from(idx) - 1;
        let rc = unsafe { (api.initialize)(channel, PCAN_BAUD_500K, 0, 0, 0) };
        if rc == PCAN_ERROR_OK {
            let _ = unsafe { (api.uninitialize)(channel) };
            out.push(PcanAdapterInfo {
                channel_name: format!("PCAN_USBBUS{idx}"),
                channel_byte: channel,
            });
        }
    }
    out
}

/// Enumerated PCAN channel, surfaced by the `adapters` subcommand.
#[derive(Debug, Clone)]
pub struct PcanAdapterInfo {
    pub channel_name: String,
    pub channel_byte: u16,
}

// ---- Frame conversion ----

fn our_frame_to_tpcan(frame: &CanFrame) -> Result<TPCANMsg> {
    if frame.id > 0x7FF {
        return Err(TransportError::Other(format!(
            "PCAN: extended (29-bit) ID 0x{:X} not supported by v1 flasher",
            frame.id
        )));
    }
    if frame.len > 8 {
        return Err(TransportError::Other(format!(
            "PCAN: frame len {} exceeds classic-CAN max 8",
            frame.len
        )));
    }
    Ok(TPCANMsg {
        id: u32::from(frame.id),
        msgtype: PCAN_MESSAGE_STANDARD,
        len: frame.len,
        data: frame.data,
    })
}

fn tpcan_to_our(msg: &TPCANMsg) -> Result<CanFrame> {
    if msg.msgtype & PCAN_MESSAGE_EXTENDED != 0 {
        return Err(TransportError::Other(
            "PCAN: received extended (29-bit) frame; bootloader uses 11-bit only".into(),
        ));
    }
    if msg.id > 0x7FF {
        return Err(TransportError::Other(format!(
            "PCAN: standard-flagged frame has out-of-range ID 0x{:X}",
            msg.id
        )));
    }
    if msg.len > 8 {
        return Err(TransportError::Other(format!(
            "PCAN: frame len {} > 8",
            msg.len
        )));
    }
    Ok(CanFrame {
        id: msg.id as u16,
        data: msg.data,
        len: msg.len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Channel parsing ----

    #[test]
    fn parse_channel_usbbus_index() {
        assert_eq!(parse_channel("PCAN_USBBUS1").unwrap(), 0x51);
        assert_eq!(parse_channel("PCAN_USBBUS2").unwrap(), 0x52);
        assert_eq!(parse_channel("PCAN_USBBUS16").unwrap(), 0x60);
    }

    #[test]
    fn parse_channel_case_insensitive() {
        assert_eq!(parse_channel("pcan_usbbus1").unwrap(), 0x51);
        assert_eq!(parse_channel("Pcan_UsbBus16").unwrap(), 0x60);
    }

    #[test]
    fn parse_channel_rejects_out_of_range_index() {
        assert!(matches!(
            parse_channel("PCAN_USBBUS17"),
            Err(TransportError::InvalidChannel { .. })
        ));
        assert!(matches!(
            parse_channel("PCAN_USBBUS0"),
            Err(TransportError::InvalidChannel { .. })
        ));
    }

    #[test]
    fn parse_channel_accepts_hex() {
        assert_eq!(parse_channel("0x51").unwrap(), 0x51);
        assert_eq!(parse_channel("0X60").unwrap(), 0x60);
    }

    #[test]
    fn parse_channel_rejects_hex_outside_usb_range() {
        assert!(matches!(
            parse_channel("0x50"),
            Err(TransportError::InvalidChannel { .. })
        ));
        assert!(matches!(
            parse_channel("0x61"),
            Err(TransportError::InvalidChannel { .. })
        ));
    }

    #[test]
    fn parse_channel_rejects_junk() {
        assert!(matches!(
            parse_channel("not-a-channel"),
            Err(TransportError::InvalidChannel { .. })
        ));
        assert!(matches!(
            parse_channel(""),
            Err(TransportError::InvalidChannel { .. })
        ));
    }

    // ---- Bitrate mapping ----

    #[test]
    fn bitrate_to_pcan_covers_standard_rates() {
        assert_eq!(bitrate_to_pcan_code(500_000), Some(PCAN_BAUD_500K));
        assert_eq!(bitrate_to_pcan_code(1_000_000), Some(PCAN_BAUD_1M));
        assert_eq!(bitrate_to_pcan_code(10_000), Some(PCAN_BAUD_10K));
    }

    #[test]
    fn bitrate_to_pcan_rejects_arbitrary_rate() {
        assert_eq!(bitrate_to_pcan_code(33_000), None);
        assert_eq!(bitrate_to_pcan_code(250_001), None);
        assert_eq!(bitrate_to_pcan_code(0), None);
    }

    // ---- Frame conversion ----

    fn frame(id: u16, data: &[u8]) -> CanFrame {
        CanFrame::new(id, data).unwrap()
    }

    #[test]
    fn our_to_tpcan_standard_frame() {
        let f = frame(0x003, &[1, 2, 3]);
        let msg = our_frame_to_tpcan(&f).unwrap();
        assert_eq!(msg.id, 3);
        assert_eq!(msg.msgtype, PCAN_MESSAGE_STANDARD);
        assert_eq!(msg.len, 3);
        assert_eq!(&msg.data[..3], &[1, 2, 3]);
    }

    #[test]
    fn our_to_tpcan_rejects_extended_id() {
        let bad = CanFrame {
            id: 0x800,
            data: [0; 8],
            len: 0,
        };
        assert!(matches!(
            our_frame_to_tpcan(&bad),
            Err(TransportError::Other(_))
        ));
    }

    #[test]
    fn tpcan_to_our_standard_frame() {
        let msg = TPCANMsg {
            id: 0x230,
            msgtype: PCAN_MESSAGE_STANDARD,
            len: 2,
            data: [0x10, 0xFE, 0, 0, 0, 0, 0, 0],
        };
        let f = tpcan_to_our(&msg).unwrap();
        assert_eq!(f.id, 0x230);
        assert_eq!(f.len, 2);
        assert_eq!(&f.data[..2], &[0x10, 0xFE]);
    }

    #[test]
    fn tpcan_to_our_rejects_extended() {
        let msg = TPCANMsg {
            id: 0x1ABCD,
            msgtype: PCAN_MESSAGE_EXTENDED,
            len: 0,
            data: [0; 8],
        };
        assert!(matches!(tpcan_to_our(&msg), Err(TransportError::Other(_))));
    }

    #[test]
    fn tpcan_to_our_rejects_oversize_dlc() {
        let msg = TPCANMsg {
            id: 0x100,
            msgtype: PCAN_MESSAGE_STANDARD,
            len: 9,
            data: [0; 8],
        };
        assert!(matches!(tpcan_to_our(&msg), Err(TransportError::Other(_))));
    }

    #[test]
    fn frame_conversion_roundtrip() {
        for f in [
            frame(0x003, &[0x01, 0x00, 0x01]),
            frame(0x7FF, &[0xAB, 0xCD]),
            frame(0x100, &[]),
            frame(0x1EF, &[0xAA; 8]),
        ] {
            let msg = our_frame_to_tpcan(&f).unwrap();
            let back = tpcan_to_our(&msg).unwrap();
            assert_eq!(f, back, "roundtrip {f:?}");
        }
    }
}
