//! Vector XL Driver Library backend — VN-series and compatible Vector
//! adapters on Windows.
//!
//! Linux support is planned; on Linux Vector hardware is exposed via a
//! proprietary kernel driver that is not yet wired into this crate.
//! This module is therefore `cfg`-gated to `target_os = "windows"`.
//!
//! ## Runtime dynamic loading
//!
//! The XL Driver Library is resolved at runtime via [`libloading`],
//! **not** at link time. Users who never touch `--interface vector`
//! don't need the SDK installed at all. A missing DLL surfaces at
//! `open_backend` as [`TransportError::AdapterMissing`] with a
//! download pointer, not a link failure.
//!
//! Search order:
//!
//! 1. `$VECTOR_LIB_PATH` environment variable — explicit override.
//! 2. `vxlapi64.dll` via the Windows DLL search path (the Vector
//!    installer places the DLL in `System32`).
//! 3. The directory containing the currently-running executable —
//!    useful for portable bundles.
//!
//! ## Channel strings
//!
//! `--channel N` where N is the 0-based XL channel index as shown by
//! `can-flasher adapters`. A VN1610 appears as two consecutive
//! channel indices (one per physical port). Run `adapters` first to
//! discover the indices on a given machine.
//!
//! ## Threading model
//!
//! `xlReceive` is non-blocking and returns `XL_ERR_QUEUE_IS_EMPTY`
//! when the queue is empty. We run a dedicated reader thread that
//! polls `xlReceive` in a tight loop with a short sleep between empty
//! reads, and hands decoded frames to the async side through a
//! `tokio::sync::mpsc` channel — the same shape the PCAN backend uses.
//!
//! ## Struct-layout note
//!
//! The XL Driver Library's `XLdriverConfig` and `XLchannelConfig`
//! structs are large and their exact layout has changed across SDK
//! versions. Rather than reproducing the full C layout in Rust, this
//! module treats the config as an opaque byte buffer and accesses
//! fields at well-known byte offsets derived from XL Driver Library
//! 20.30 headers (natural C alignment, 64-bit Windows).
//!
//! `XL_CHANNEL_CONFIG_SIZE` is the critical constant. If `detect()`
//! returns empty or wrong results, compare it against
//! `sizeof(XLchannelConfig)` from your installed `vxlapi.h`.

#![cfg(target_os = "windows")]

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

// ---- XL Driver Library constants (from vxlapi.h, SDK 20.30) ----

const XL_MAX_LENGTH: usize = 31;
const XL_CONFIG_MAX_CHANNELS: usize = 64;

const XL_BUS_TYPE_CAN: u32 = 0x0000_0001;
const XL_BUS_ACTIVE_CAP_CAN: u64 = 0x0000_0000_0000_0001;
const XL_INTERFACE_VERSION: u32 = 3;
const XL_INVALID_PORTHANDLE: i64 = -1;
const XL_ACTIVATE_RESET_CLOCK: u32 = 8;
const XL_RX_QUEUE_SIZE: u32 = 256;

const XL_SUCCESS: i32 = 0;
const XL_ERR_QUEUE_IS_EMPTY: i32 = 10;

// XLevent.tag values.
const XL_RECEIVE_MSG: u8 = 1;
const XL_TRANSMIT_MSG: u8 = 10;

// XL_CAN_MSG.flags bitmasks. TX echoes and error/remote frames are
// filtered out in the reader; only clean data frames reach the session.
const XL_CAN_MSG_FLAG_TX_COMPLETED: u16 = 0x0004;
const XL_CAN_MSG_FLAG_TX_REQUEST: u16 = 0x0008;
const XL_CAN_MSG_FLAG_ERROR_FRAME: u16 = 0x0020;
const XL_CAN_MSG_FLAG_REMOTE_FRAME: u16 = 0x0010;

/// Reader poll interval when the RX queue is empty.
const RX_POLL_INTERVAL: Duration = Duration::from_millis(1);
/// Async MPSC queue depth (matches the SLCAN / PCAN backends).
const RX_QUEUE_DEPTH: usize = 256;

// ---- XLdriverConfig byte layout ----
//
// XLchannelConfig field byte offsets (natural C alignment, 64-bit):
//
//   0   name[32]               char[XL_MAX_LENGTH+1]
//  32   hw_type                u8
//  33   hw_index               u8
//  34   hw_channel             u8
//  35   (pad 1)
//  36   transceiver_type       u16
//  38   transceiver_state      u16
//  40   config_error           u16
//  42   (pad 6 — align u64)
//  48   channel_mask           u64
//  56   channel_index          u32
//  60   (pad 4 — align u64)
//  64   channel_capabilities   u64
//  72   channel_bus_capabilities u64
//  80   is_on_bus              u8
//  81   connected              u8
//  82   channel_bus_active_cap u16
//  84   bus_params             XLbusParams (4 + 32 = 36 bytes)
// 120   _reserved[32]          u32[32] = 128 bytes
// 248   transceiver_type2      u32
// 252   transceiver_state2     u16
// 254   (pad 2)
// 256   transceiver_name[32]   char[XL_MAX_LENGTH+1]
// 288   channel_capabilities2  i32
// 292   (pad 4 — align u64)
// 296   channel_capabilities4  u64
//
// Natural-alignment total: 304 bytes. Observed SDK size (20.30 64-bit):
// 344 bytes — the extra 40 bytes are additional capability fields
// appended in later SDK revisions that we do not need to access.
//
// XLdriverConfig layout:
//   0   dll_version     u32
//   4   channel_count   u32
//   8   _reserved[10]   u32[10] = 40 bytes
//  48   channel[64]     XLchannelConfig[64]

/// Byte size of one XLchannelConfig. Update if `detect()` misbehaves
/// on a different SDK version — compare against `sizeof(XLchannelConfig)`
/// from your installed `vxlapi.h`.
const XL_CHANNEL_CONFIG_SIZE: usize = 344;

const XL_DRIVER_CONFIG_HEADER: usize = 48;
const XL_DRIVER_CONFIG_SIZE: usize =
    XL_DRIVER_CONFIG_HEADER + XL_CONFIG_MAX_CHANNELS * XL_CHANNEL_CONFIG_SIZE;

// XLchannelConfig field offsets (relative to channel entry start).
const OFF_NAME: usize = 0;
const OFF_HW_TYPE: usize = 32;
const OFF_HW_INDEX: usize = 33;
const OFF_HW_CHANNEL: usize = 34;
const OFF_CHAN_MASK: usize = 48;
const OFF_CHAN_INDEX: usize = 56;
const OFF_CHAN_BUS_CAP: usize = 72;
const OFF_TRANS_NAME: usize = 256;

// XLdriverConfig header offsets.
const OFF_DRV_CHAN_COUNT: usize = 4;
const OFF_DRV_CHANNELS: usize = XL_DRIVER_CONFIG_HEADER;

// ---- C ABI types ----

type XLstatus = i32;
type XLaccess = u64;
type XLportHandle = i64;

/// Classic CAN message payload inside an XLevent.
#[repr(C)]
#[derive(Clone, Copy)]
struct XLcanMsg {
    id: u32,
    flags: u16,
    dlc: u16,
    res1: u64,
    data: [u8; 8],
    res2: u64,
}

/// Tagged-union payload of an XLevent. Only the `msg` arm is used for
/// classic CAN; `raw` fixes the union size at 32 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
union XLtagData {
    msg: XLcanMsg,
    raw: [u8; 32],
}

/// CAN event delivered by `xlReceive` and consumed by `xlCanTransmit`.
///
/// Layout is fixed at 48 bytes:
///   8 bytes header  (tag, chanIndex, transId, portHandle, flags, reserved)
///   8 bytes         timeStamp
///  32 bytes         tagData union
#[repr(C)]
struct XLevent {
    tag: u8,
    chan_index: u8,
    trans_id: u16,
    port_handle: u16,
    flags: u8,
    reserved: u8,
    timestamp: u64,
    tag_data: XLtagData,
}

// ---- Function-pointer types ----
//
// The XL API macro `XLAPI` expands to `__cdecl` on Windows, which
// Rust spells `extern "C"`. On 64-bit Windows there is only one
// calling convention for `extern "C"`, so this is unambiguous.

type FnXlOpenDriver = unsafe extern "C" fn() -> XLstatus;
type FnXlCloseDriver = unsafe extern "C" fn() -> XLstatus;
type FnXlGetDriverConfig = unsafe extern "C" fn(pDriverConfig: *mut u8) -> XLstatus;
type FnXlOpenPort = unsafe extern "C" fn(
    pPortHandle: *mut XLportHandle,
    userName: *const i8,
    accessMask: XLaccess,
    permissionMask: *mut XLaccess,
    rxQueueSize: u32,
    xlInterfaceVersion: u32,
    busType: u32,
) -> XLstatus;
type FnXlActivateChannel = unsafe extern "C" fn(
    portHandle: XLportHandle,
    accessMask: XLaccess,
    busType: u32,
    flags: u32,
) -> XLstatus;
type FnXlDeactivateChannel =
    unsafe extern "C" fn(portHandle: XLportHandle, accessMask: XLaccess) -> XLstatus;
type FnXlClosePort = unsafe extern "C" fn(portHandle: XLportHandle) -> XLstatus;
type FnXlCanSetChannelBitrate =
    unsafe extern "C" fn(portHandle: XLportHandle, accessMask: XLaccess, bitrate: u32) -> XLstatus;
type FnXlReceive = unsafe extern "C" fn(
    portHandle: XLportHandle,
    pEventCount: *mut u32,
    pEvents: *mut XLevent,
) -> XLstatus;
type FnXlCanTransmit = unsafe extern "C" fn(
    portHandle: XLportHandle,
    accessMask: XLaccess,
    messageCount: *mut u32,
    pMessages: *mut XLevent,
) -> XLstatus;
type FnXlGetErrorString = unsafe extern "C" fn(err: XLstatus) -> *const i8;

// ---- Loaded API ----

/// Resolved function pointers plus a keep-alive handle on the loaded
/// library. Wrapped in `Arc` so the reader thread and the async send
/// path share it without copying.
struct VectorApi {
    _lib: Arc<Library>,
    open_driver: FnXlOpenDriver,
    close_driver: FnXlCloseDriver,
    get_driver_config: FnXlGetDriverConfig,
    open_port: FnXlOpenPort,
    activate_channel: FnXlActivateChannel,
    deactivate_channel: FnXlDeactivateChannel,
    close_port: FnXlClosePort,
    set_channel_bitrate: FnXlCanSetChannelBitrate,
    receive: FnXlReceive,
    transmit: FnXlCanTransmit,
    get_error_string: FnXlGetErrorString,
}

impl VectorApi {
    fn load(lib: Arc<Library>) -> Result<Self> {
        // SAFETY: each `lib.get` resolves an exported symbol by name.
        // If any symbol is absent we surface a clear error and never
        // call through the resulting pointer.
        unsafe {
            let open_driver = *lib
                .get::<FnXlOpenDriver>(b"xlOpenDriver\0")
                .map_err(|e| missing_symbol("xlOpenDriver", e))?;
            let close_driver = *lib
                .get::<FnXlCloseDriver>(b"xlCloseDriver\0")
                .map_err(|e| missing_symbol("xlCloseDriver", e))?;
            let get_driver_config = *lib
                .get::<FnXlGetDriverConfig>(b"xlGetDriverConfig\0")
                .map_err(|e| missing_symbol("xlGetDriverConfig", e))?;
            let open_port = *lib
                .get::<FnXlOpenPort>(b"xlOpenPort\0")
                .map_err(|e| missing_symbol("xlOpenPort", e))?;
            let activate_channel = *lib
                .get::<FnXlActivateChannel>(b"xlActivateChannel\0")
                .map_err(|e| missing_symbol("xlActivateChannel", e))?;
            let deactivate_channel = *lib
                .get::<FnXlDeactivateChannel>(b"xlDeactivateChannel\0")
                .map_err(|e| missing_symbol("xlDeactivateChannel", e))?;
            let close_port = *lib
                .get::<FnXlClosePort>(b"xlClosePort\0")
                .map_err(|e| missing_symbol("xlClosePort", e))?;
            let set_channel_bitrate = *lib
                .get::<FnXlCanSetChannelBitrate>(b"xlCanSetChannelBitrate\0")
                .map_err(|e| missing_symbol("xlCanSetChannelBitrate", e))?;
            let receive = *lib
                .get::<FnXlReceive>(b"xlReceive\0")
                .map_err(|e| missing_symbol("xlReceive", e))?;
            let transmit = *lib
                .get::<FnXlCanTransmit>(b"xlCanTransmit\0")
                .map_err(|e| missing_symbol("xlCanTransmit", e))?;
            let get_error_string = *lib
                .get::<FnXlGetErrorString>(b"xlGetErrorString\0")
                .map_err(|e| missing_symbol("xlGetErrorString", e))?;
            Ok(Self {
                _lib: lib,
                open_driver,
                close_driver,
                get_driver_config,
                open_port,
                activate_channel,
                deactivate_channel,
                close_port,
                set_channel_bitrate,
                receive,
                transmit,
                get_error_string,
            })
        }
    }

    fn error_text(&self, status: XLstatus) -> String {
        // SAFETY: xlGetErrorString returns a pointer to a static string
        // owned by the library. It is never null and lives as long as
        // the library is loaded (guaranteed by _lib's keep-alive Arc).
        let ptr = unsafe { (self.get_error_string)(status) };
        if ptr.is_null() {
            return format!("XL error 0x{status:04X}");
        }
        unsafe { std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned() }
    }
}

fn missing_symbol(name: &'static str, err: libloading::Error) -> TransportError {
    TransportError::AdapterMissing {
        name: "vector",
        reason: format!(
            "XL Driver Library is missing required symbol `{name}`: {err}. \
             Reinstall the Vector XL Driver Library from \
             https://www.vector.com/int/en/products/products-a-z/software/xl-driver-library/"
        ),
    }
}

// ---- Library loading ----

fn load_library() -> Result<Library> {
    // 64-bit DLL. The 32-bit variant is `vxlapi.dll`; we target
    // 64-bit Windows exclusively.
    const LIB_NAME: &str = "vxlapi64.dll";

    // 1. Explicit override via VECTOR_LIB_PATH.
    if let Ok(path) = env::var("VECTOR_LIB_PATH") {
        // SAFETY: path comes from an env var the user controls.
        if let Ok(lib) = unsafe { Library::new(&path) } {
            return Ok(lib);
        }
    }

    // 2. Bare filename — Windows searches System32 / SysWOW64 /
    // PATH, which is where the Vector installer places the DLL.
    if let Ok(lib) = unsafe { Library::new(LIB_NAME) } {
        return Ok(lib);
    }

    // 3. Alongside the current executable — portable bundles.
    if let Ok(mut exe) = env::current_exe() {
        exe.pop();
        exe.push(LIB_NAME);
        if let Ok(lib) = unsafe { Library::new(&exe) } {
            return Ok(lib);
        }
    }

    Err(TransportError::AdapterMissing {
        name: "vector",
        reason: format!(
            "Vector XL Driver Library `{LIB_NAME}` not found. \
             Searched: VECTOR_LIB_PATH env var, system DLL path, executable directory. \
             Download and install from \
             https://www.vector.com/int/en/products/products-a-z/software/xl-driver-library/ \
             — or set VECTOR_LIB_PATH to point to an existing install."
        ),
    })
}

// ---- Driver-config buffer helpers ----

fn read_u32_le(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap())
}

fn read_u64_le(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap())
}

fn read_cstr(buf: &[u8], offset: usize, max_len: usize) -> String {
    let slice = &buf[offset..offset + max_len];
    let end = slice.iter().position(|&b| b == 0).unwrap_or(max_len);
    String::from_utf8_lossy(&slice[..end]).trim().to_owned()
}

/// Internal channel descriptor populated from the XLdriverConfig buffer.
struct XLChannelInfo {
    index: u32,
    mask: u64,
    name: String,
    hw_type: u8,
    hw_index: u8,
    hw_channel: u8,
    transceiver_name: String,
}

/// Walk the driver-config buffer and collect every CAN-capable channel.
fn parse_can_channels(buf: &[u8]) -> Vec<XLChannelInfo> {
    let count = (read_u32_le(buf, OFF_DRV_CHAN_COUNT) as usize).min(XL_CONFIG_MAX_CHANNELS);
    let mut out = Vec::new();
    for i in 0..count {
        let base = OFF_DRV_CHANNELS + i * XL_CHANNEL_CONFIG_SIZE;
        if base + XL_CHANNEL_CONFIG_SIZE > buf.len() {
            break;
        }
        let bus_cap = read_u64_le(buf, base + OFF_CHAN_BUS_CAP);
        if bus_cap & XL_BUS_ACTIVE_CAP_CAN == 0 {
            continue;
        }
        out.push(XLChannelInfo {
            index: read_u32_le(buf, base + OFF_CHAN_INDEX),
            mask: read_u64_le(buf, base + OFF_CHAN_MASK),
            name: read_cstr(buf, base + OFF_NAME, XL_MAX_LENGTH + 1),
            hw_type: buf[base + OFF_HW_TYPE],
            hw_index: buf[base + OFF_HW_INDEX],
            hw_channel: buf[base + OFF_HW_CHANNEL],
            transceiver_name: read_cstr(buf, base + OFF_TRANS_NAME, XL_MAX_LENGTH + 1),
        });
    }
    out
}

// ---- Detection ----

/// Return a list of CAN-capable Vector channels visible to the XL
/// Driver Library. Returns an empty vector if the library is not
/// installed or no CAN hardware is connected.
pub fn detect() -> Vec<VectorAdapterInfo> {
    let Ok(lib) = load_library() else {
        return Vec::new();
    };
    let Ok(api) = VectorApi::load(Arc::new(lib)) else {
        return Vec::new();
    };

    // SAFETY: xlOpenDriver has no preconditions.
    let rc = unsafe { (api.open_driver)() };
    if rc != XL_SUCCESS {
        return Vec::new();
    }

    let mut buf = vec![0u8; XL_DRIVER_CONFIG_SIZE];
    // SAFETY: buf is valid, writable, and at least XL_DRIVER_CONFIG_SIZE bytes.
    let rc = unsafe { (api.get_driver_config)(buf.as_mut_ptr()) };

    // Always pair every xlOpenDriver with xlCloseDriver.
    // SAFETY: driver was successfully opened above.
    let _ = unsafe { (api.close_driver)() };

    if rc != XL_SUCCESS {
        return Vec::new();
    }

    parse_can_channels(&buf)
        .into_iter()
        .map(|ch| VectorAdapterInfo {
            channel_index: ch.index,
            name: ch.name,
            hw_type: ch.hw_type,
            hw_index: ch.hw_index,
            hw_channel: ch.hw_channel,
            transceiver_name: ch.transceiver_name,
        })
        .collect()
}

/// Enumerated Vector CAN channel, surfaced by the `adapters` subcommand.
#[derive(Debug, Clone)]
pub struct VectorAdapterInfo {
    /// 0-based XL channel index — pass as `--channel N`.
    pub channel_index: u32,
    /// Human-readable channel name from the XL config (e.g. `"VN1610 1 Channel 1"`).
    pub name: String,
    pub hw_type: u8,
    pub hw_index: u8,
    pub hw_channel: u8,
    /// Transceiver name (e.g. `"CAN - TJA1041"`).
    pub transceiver_name: String,
}

// ---- Channel string parsing ----

/// Parse `"0"`, `"1"`, etc. into an XL channel index.
fn parse_channel(s: &str) -> Result<u32> {
    s.trim()
        .parse::<u32>()
        .map_err(|_| TransportError::InvalidChannel {
            channel: s.to_string(),
            reason: "Vector channel must be a non-negative integer matching the index \
                 shown by `can-flasher adapters` (e.g. 0, 1, 2)"
                .into(),
        })
}

// ---- Backend ----

/// Vector XL Driver Library backend implementing [`CanBackend`].
pub struct VectorBackend {
    api: Arc<VectorApi>,
    port_handle: i64,
    access_mask: u64,
    rx: Arc<TokioMutex<mpsc::Receiver<CanFrame>>>,
    shutdown: Arc<AtomicBool>,
    reader_handle: StdMutex<Option<thread::JoinHandle<()>>>,
    description: String,
}

impl VectorBackend {
    /// Open Vector channel `channel_str` at `nominal_bps`. The channel
    /// string is the decimal XL channel index shown by `can-flasher adapters`.
    pub fn open(channel_str: &str, nominal_bps: u32) -> Result<Self> {
        let target_index = parse_channel(channel_str)?;

        let lib = load_library()?;
        let api = VectorApi::load(Arc::new(lib))?;

        // SAFETY: xlOpenDriver has no preconditions; status checked below.
        let rc = unsafe { (api.open_driver)() };
        if rc != XL_SUCCESS {
            return Err(TransportError::AdapterMissing {
                name: "vector",
                reason: format!("xlOpenDriver failed: {} (0x{rc:04X})", api.error_text(rc)),
            });
        }

        // Enumerate channels to find the target index and its mask.
        let mut buf = vec![0u8; XL_DRIVER_CONFIG_SIZE];
        // SAFETY: buf is valid and at least XL_DRIVER_CONFIG_SIZE bytes.
        let rc = unsafe { (api.get_driver_config)(buf.as_mut_ptr()) };
        if rc != XL_SUCCESS {
            let _ = unsafe { (api.close_driver)() };
            return Err(TransportError::InvalidChannel {
                channel: channel_str.to_string(),
                reason: format!(
                    "xlGetDriverConfig failed: {} (0x{rc:04X})",
                    api.error_text(rc)
                ),
            });
        }

        let channels = parse_can_channels(&buf);
        let Some(ch) = channels.into_iter().find(|c| c.index == target_index) else {
            let _ = unsafe { (api.close_driver)() };
            return Err(TransportError::InvalidChannel {
                channel: channel_str.to_string(),
                reason: format!(
                    "no CAN-capable Vector channel with index {target_index}; \
                     run `can-flasher adapters` to list available channels"
                ),
            });
        };

        let access_mask = ch.mask;
        let chan_name = ch.name.clone();

        // Open a port. We request init access (permission_mask = access_mask)
        // so we can set the bitrate; if another app holds the bus we get a
        // slave-only port and warn that the bitrate was not set.
        let app_name = b"can-flasher\0".as_ptr() as *const i8;
        let mut port_handle: i64 = XL_INVALID_PORTHANDLE;
        let mut permission_mask: u64 = access_mask;

        // SAFETY: app_name is a valid NUL-terminated ASCII string;
        // port_handle and permission_mask are valid out-params.
        let rc = unsafe {
            (api.open_port)(
                &mut port_handle,
                app_name,
                access_mask,
                &mut permission_mask,
                XL_RX_QUEUE_SIZE,
                XL_INTERFACE_VERSION,
                XL_BUS_TYPE_CAN,
            )
        };
        if rc != XL_SUCCESS || port_handle == XL_INVALID_PORTHANDLE {
            let _ = unsafe { (api.close_driver)() };
            return Err(TransportError::InvalidChannel {
                channel: channel_str.to_string(),
                reason: format!(
                    "xlOpenPort failed: {} (0x{rc:04X}). \
                     Another application may hold exclusive access to channel {target_index}.",
                    api.error_text(rc)
                ),
            });
        }

        // Set the bitrate only if we obtained init (write) access.
        if permission_mask & access_mask != 0 {
            // SAFETY: port_handle is valid; function preconditions met.
            let rc = unsafe { (api.set_channel_bitrate)(port_handle, access_mask, nominal_bps) };
            if rc != XL_SUCCESS {
                let _ = unsafe { (api.close_port)(port_handle) };
                let _ = unsafe { (api.close_driver)() };
                return Err(TransportError::InvalidChannel {
                    channel: channel_str.to_string(),
                    reason: format!(
                        "xlCanSetChannelBitrate({nominal_bps} bps) failed: {} (0x{rc:04X})",
                        api.error_text(rc)
                    ),
                });
            }
        } else {
            warn!(
                channel = target_index,
                bitrate = nominal_bps,
                "Vector: no init access obtained — bitrate was not set; \
                 bus must already be running at the correct rate"
            );
        }

        // Activate the channel so frames start flowing.
        // SAFETY: port_handle is valid; XL_BUS_TYPE_CAN / flags are known constants.
        let rc = unsafe {
            (api.activate_channel)(
                port_handle,
                access_mask,
                XL_BUS_TYPE_CAN,
                XL_ACTIVATE_RESET_CLOCK,
            )
        };
        if rc != XL_SUCCESS {
            let _ = unsafe { (api.close_port)(port_handle) };
            let _ = unsafe { (api.close_driver)() };
            return Err(TransportError::InvalidChannel {
                channel: channel_str.to_string(),
                reason: format!(
                    "xlActivateChannel failed: {} (0x{rc:04X})",
                    api.error_text(rc)
                ),
            });
        }

        let description = format!(
            "Vector {} (channel {}, {} bps)",
            chan_name, target_index, nominal_bps
        );
        let api = Arc::new(api);
        let shutdown = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel(RX_QUEUE_DEPTH);

        let reader_api = Arc::clone(&api);
        let reader_shutdown = Arc::clone(&shutdown);
        let reader_handle = thread::Builder::new()
            .name("vector-reader".into())
            .spawn(move || reader_loop(reader_api, port_handle, tx, reader_shutdown))
            .map_err(|e| TransportError::Other(format!("spawn reader thread: {e}")))?;

        Ok(Self {
            api,
            port_handle,
            access_mask,
            rx: Arc::new(TokioMutex::new(rx)),
            shutdown,
            reader_handle: StdMutex::new(Some(reader_handle)),
            description,
        })
    }
}

#[async_trait]
impl CanBackend for VectorBackend {
    async fn send(&self, frame: CanFrame) -> Result<()> {
        if frame.id > 0x7FF {
            return Err(TransportError::Other(format!(
                "Vector: extended (29-bit) ID 0x{:X} not supported by v1 flasher",
                frame.id
            )));
        }

        let mut event = XLevent {
            tag: XL_TRANSMIT_MSG,
            chan_index: 0,
            trans_id: 0,
            port_handle: 0,
            flags: 0,
            reserved: 0,
            timestamp: 0,
            tag_data: XLtagData {
                msg: XLcanMsg {
                    id: u32::from(frame.id),
                    flags: 0,
                    dlc: u16::from(frame.len),
                    res1: 0,
                    data: frame.data,
                    res2: 0,
                },
            },
        };

        let api = Arc::clone(&self.api);
        let port_handle = self.port_handle;
        let access_mask = self.access_mask;

        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut count: u32 = 1;
            // SAFETY: port_handle is valid; event is a properly-formed XLevent.
            let rc = unsafe { (api.transmit)(port_handle, access_mask, &mut count, &mut event) };
            if rc != XL_SUCCESS {
                return Err(TransportError::Other(format!(
                    "xlCanTransmit failed: {} (0x{rc:04X})",
                    api.error_text(rc)
                )));
            }
            Ok(())
        })
        .await
        .map_err(|e| TransportError::Other(format!("spawn_blocking join: {e}")))?
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
        // Changing bitrate mid-session requires deactivate + close +
        // reopen — the flasher doesn't exercise this path.
        debug!(
            port = self.port_handle,
            "Vector: runtime bitrate change not implemented; \
             close and reopen the backend instead"
        );
        Ok(())
    }

    fn description(&self) -> String {
        self.description.clone()
    }
}

impl Drop for VectorBackend {
    fn drop(&mut self) {
        // Signal the reader to exit, then clean up the XL side before
        // joining so the reader's next xlReceive returns quickly with
        // an error rather than blocking the join.
        self.shutdown.store(true, Ordering::SeqCst);
        // SAFETY: port_handle and access_mask are valid for our lifetime.
        let _ = unsafe { (self.api.deactivate_channel)(self.port_handle, self.access_mask) };
        let _ = unsafe { (self.api.close_port)(self.port_handle) };
        let _ = unsafe { (self.api.close_driver)() };
        if let Ok(mut guard) = self.reader_handle.lock() {
            if let Some(h) = guard.take() {
                let _ = h.join();
            }
        }
    }
}

// ---- Reader thread ----

fn reader_loop(
    api: Arc<VectorApi>,
    port_handle: i64,
    tx: mpsc::Sender<CanFrame>,
    shutdown: Arc<AtomicBool>,
) {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }

        let mut event_count: u32 = 1;
        let mut event = XLevent {
            tag: 0,
            chan_index: 0,
            trans_id: 0,
            port_handle: 0,
            flags: 0,
            reserved: 0,
            timestamp: 0,
            tag_data: XLtagData { raw: [0; 32] },
        };

        // SAFETY: port_handle is valid while VectorBackend is alive;
        // event is a properly-sized out-param.
        let rc = unsafe { (api.receive)(port_handle, &mut event_count, &mut event) };

        match rc {
            XL_SUCCESS => {
                if event.tag != XL_RECEIVE_MSG {
                    continue;
                }
                // SAFETY: tag == XL_RECEIVE_MSG guarantees the msg arm is active.
                let msg = unsafe { event.tag_data.msg };

                // Skip TX echoes (TX_COMPLETED / TX_REQUEST) and
                // error / remote frames — only data frames matter.
                if msg.flags & (XL_CAN_MSG_FLAG_TX_COMPLETED | XL_CAN_MSG_FLAG_TX_REQUEST) != 0 {
                    continue;
                }
                if msg.flags & (XL_CAN_MSG_FLAG_ERROR_FRAME | XL_CAN_MSG_FLAG_REMOTE_FRAME) != 0 {
                    continue;
                }
                // The bootloader only uses 11-bit standard IDs.
                if msg.id > 0x7FF {
                    continue;
                }

                let dlc = (msg.dlc as u8).min(8);
                let frame = CanFrame {
                    id: msg.id as u16,
                    data: msg.data,
                    len: dlc,
                };
                if tx.blocking_send(frame).is_err() {
                    // Async receiver dropped — VectorBackend is gone.
                    return;
                }
            }
            XL_ERR_QUEUE_IS_EMPTY => {
                thread::sleep(RX_POLL_INTERVAL);
            }
            other => {
                warn!(
                    code = format!("0x{other:04X}"),
                    text = %api.error_text(other),
                    "vector reader: xlReceive returned error status"
                );
                thread::sleep(RX_POLL_INTERVAL);
            }
        }
    }
}

// ---- Tests ----

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Channel string parsing ----

    #[test]
    fn parse_channel_accepts_zero() {
        assert_eq!(parse_channel("0").unwrap(), 0);
    }

    #[test]
    fn parse_channel_accepts_positive_indices() {
        assert_eq!(parse_channel("1").unwrap(), 1);
        assert_eq!(parse_channel("63").unwrap(), 63);
    }

    #[test]
    fn parse_channel_trims_whitespace() {
        assert_eq!(parse_channel("  2  ").unwrap(), 2);
    }

    #[test]
    fn parse_channel_rejects_negative() {
        assert!(matches!(
            parse_channel("-1"),
            Err(TransportError::InvalidChannel { .. })
        ));
    }

    #[test]
    fn parse_channel_rejects_junk() {
        assert!(matches!(
            parse_channel("abc"),
            Err(TransportError::InvalidChannel { .. })
        ));
        assert!(matches!(
            parse_channel(""),
            Err(TransportError::InvalidChannel { .. })
        ));
    }

    // ---- ABI layout assertions ----

    #[test]
    fn xlcan_msg_is_32_bytes() {
        assert_eq!(std::mem::size_of::<XLcanMsg>(), 32);
    }

    #[test]
    fn xltag_data_is_32_bytes() {
        assert_eq!(std::mem::size_of::<XLtagData>(), 32);
    }

    #[test]
    fn xlevent_is_48_bytes() {
        // 8 header + 8 timestamp + 32 tagData = 48.
        assert_eq!(std::mem::size_of::<XLevent>(), 48);
    }

    // ---- Buffer read helpers ----

    #[test]
    fn read_u32_le_correct() {
        let buf = [0x78u8, 0x56, 0x34, 0x12, 0, 0, 0, 0];
        assert_eq!(read_u32_le(&buf, 0), 0x1234_5678);
    }

    #[test]
    fn read_u64_le_correct() {
        let buf = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        assert_eq!(read_u64_le(&buf, 0), 0x0807_0605_0403_0201);
    }

    #[test]
    fn read_cstr_stops_at_nul() {
        let buf = b"VN1610\0remainder";
        assert_eq!(read_cstr(buf, 0, buf.len()), "VN1610");
    }

    #[test]
    fn read_cstr_handles_no_nul() {
        let buf = b"VN1610";
        assert_eq!(read_cstr(buf, 0, buf.len()), "VN1610");
    }

    #[test]
    fn read_cstr_trims_trailing_whitespace() {
        let buf = b"VN1610   \0";
        assert_eq!(read_cstr(buf, 0, buf.len()), "VN1610");
    }

    // ---- parse_can_channels ----

    fn make_config_buf(channel_count: u32, channels: &[(u32, u64, u64, &str, &str)]) -> Vec<u8> {
        // channels: (channel_index, channel_mask, bus_cap, name, transceiver_name)
        let mut buf = vec![0u8; XL_DRIVER_CONFIG_SIZE];
        // channel_count at offset 4
        buf[OFF_DRV_CHAN_COUNT..OFF_DRV_CHAN_COUNT + 4]
            .copy_from_slice(&channel_count.to_le_bytes());

        for (slot, &(ch_idx, ch_mask, bus_cap, name, trans)) in channels.iter().enumerate() {
            let base = OFF_DRV_CHANNELS + slot * XL_CHANNEL_CONFIG_SIZE;
            // name
            let name_bytes = name.as_bytes();
            let nlen = name_bytes.len().min(XL_MAX_LENGTH);
            buf[base + OFF_NAME..base + OFF_NAME + nlen].copy_from_slice(&name_bytes[..nlen]);
            // channel_mask
            buf[base + OFF_CHAN_MASK..base + OFF_CHAN_MASK + 8]
                .copy_from_slice(&ch_mask.to_le_bytes());
            // channel_index
            buf[base + OFF_CHAN_INDEX..base + OFF_CHAN_INDEX + 4]
                .copy_from_slice(&ch_idx.to_le_bytes());
            // channel_bus_capabilities
            buf[base + OFF_CHAN_BUS_CAP..base + OFF_CHAN_BUS_CAP + 8]
                .copy_from_slice(&bus_cap.to_le_bytes());
            // transceiver_name
            let tn_bytes = trans.as_bytes();
            let tnlen = tn_bytes.len().min(XL_MAX_LENGTH);
            buf[base + OFF_TRANS_NAME..base + OFF_TRANS_NAME + tnlen]
                .copy_from_slice(&tn_bytes[..tnlen]);
        }
        buf
    }

    #[test]
    fn parse_can_channels_returns_only_can_capable() {
        // Slot 0: CAN capable; slot 1: LIN only (bit 1 set, not bit 0).
        let buf = make_config_buf(
            2,
            &[
                (0, 0x01, XL_BUS_ACTIVE_CAP_CAN, "VN1610 Ch1", "CAN TJA1041"),
                (1, 0x02, 0x0000_0000_0000_0002, "VN1610 Ch2 LIN", "LIN"),
            ],
        );
        let channels = parse_can_channels(&buf);
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].index, 0);
        assert_eq!(channels[0].name, "VN1610 Ch1");
    }

    #[test]
    fn parse_can_channels_respects_channel_count() {
        // 64 slots in the buffer but count says 1.
        let buf = make_config_buf(1, &[(0, 0x01, XL_BUS_ACTIVE_CAP_CAN, "VN1610", "CAN")]);
        let channels = parse_can_channels(&buf);
        assert_eq!(channels.len(), 1);
    }

    #[test]
    fn parse_can_channels_empty_on_zero_count() {
        let buf = make_config_buf(0, &[]);
        assert!(parse_can_channels(&buf).is_empty());
    }

    #[test]
    fn parse_can_channels_reads_mask_and_transceiver() {
        let mask: u64 = 0xDEAD_BEEF_0000_0001;
        let buf = make_config_buf(
            1,
            &[(0, mask, XL_BUS_ACTIVE_CAP_CAN, "VN1610", "CAN - TJA1041")],
        );
        let channels = parse_can_channels(&buf);
        assert_eq!(channels[0].mask, mask);
        assert_eq!(channels[0].transceiver_name, "CAN - TJA1041");
    }
}
