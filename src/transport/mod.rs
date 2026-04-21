//! Transport layer — adapter I/O.
//!
//! The rest of the crate talks to a CAN bus through [`CanBackend`], a
//! tiny async trait. Each backend (SLCAN, SocketCAN, PCAN, virtual)
//! implements it independently; callers pick one at runtime via
//! [`open_backend`] based on `--interface`.
//!
//! ## Layout
//!
//! - [`CanBackend`] — the trait itself (dyn-safe via `async-trait`).
//! - [`TransportError`] — everything I/O can go wrong with, typed.
//! - [`virtual_bus::VirtualBus`] / [`virtual_bus::VirtualBackend`] —
//!   in-process loopback for testing + CI. Speaks the trait, stores
//!   no state the caller can't observe, and is the only backend
//!   that needs zero host setup.
//! - [`stub_device::StubDevice`] — minimal in-process bootloader
//!   that answers a handful of opcodes. Paired with `VirtualBus` it
//!   exercises the full ISO-TP + protocol stack without hardware.
//! - Real backends (`slcan`, `socketcan`, `pcan`) land in
//!   `feat/5…feat/7`. [`open_backend`] routes each `InterfaceType`
//!   to the right one today; unimplemented variants return
//!   `TransportError::AdapterMissing`.

use std::time::Duration;

use async_trait::async_trait;

use crate::cli::InterfaceType;
use crate::protocol::CanFrame;

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub mod pcan;
pub mod slcan;
#[cfg(target_os = "linux")]
pub mod socketcan;
pub mod stub_device;
pub mod virtual_bus;

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub use pcan::{PcanAdapterInfo, PcanBackend};
pub use slcan::{SlcanAdapterInfo, SlcanBackend};
#[cfg(target_os = "linux")]
pub use socketcan::{SocketCanAdapterInfo, SocketCanBackend};
pub use stub_device::StubDevice;
pub use virtual_bus::{StubLoopback, VirtualBackend, VirtualBus};

/// Every way the transport layer can fail.
///
/// Callers typically downcast via `match` and decide between retry
/// (on `Timeout`), reconnect (on `Disconnected`), or fail the command
/// (the rest). The `Display` impl is friendly enough to show directly
/// to the user.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// `recv` ran past the supplied deadline without a frame.
    #[error("timed out waiting for CAN frame after {}ms", .0.as_millis())]
    Timeout(Duration),

    /// Underlying channel / socket / serial port closed. `send` after
    /// this point is unrecoverable without reopening the adapter.
    #[error("CAN bus endpoint disconnected")]
    Disconnected,

    /// Adapter-specific I/O error.
    #[error("transport I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Channel string couldn't be parsed into something the backend
    /// understands (e.g. `COMfoo` to an SLCAN backend, `vcan99?` to
    /// SocketCAN, etc.).
    #[error("invalid adapter channel '{channel}': {reason}")]
    InvalidChannel { channel: String, reason: String },

    /// Backend isn't available on this build / host. On the flasher
    /// this surfaces during `open_backend` — e.g. `--interface pcan`
    /// with no `PCANBasic.dll` installed, or an `InterfaceType`
    /// variant whose backend hasn't been implemented yet.
    #[error("adapter '{name}' is unavailable: {reason}")]
    AdapterMissing { name: &'static str, reason: String },

    /// Generic fall-through for backends that surface domain errors
    /// that don't (yet) deserve a dedicated variant.
    #[error("{0}")]
    Other(String),
}

/// Typed result alias — every backend returns this from every async
/// method.
pub type Result<T> = std::result::Result<T, TransportError>;

/// The abstraction every backend speaks.
///
/// Async methods use the `async-trait` crate rather than native
/// `async fn in trait` so `Box<dyn CanBackend>` stays ergonomic for
/// runtime selection. The overhead is one heap allocation per call,
/// which for multi-millisecond CAN-frame round-trips is free.
#[async_trait]
pub trait CanBackend: Send + Sync {
    /// Send a single CAN frame. Completes once the adapter has
    /// accepted it (serial queue, SocketCAN write, PCAN Write); the
    /// frame may not have left the wire yet but the host can't do
    /// anything useful about that distinction.
    async fn send(&self, frame: CanFrame) -> Result<()>;

    /// Receive a single frame, returning `Err(Timeout)` if `timeout`
    /// elapses first. Timeouts are recoverable — the caller decides
    /// whether to retry, escalate, or bail.
    async fn recv(&self, timeout: Duration) -> Result<CanFrame>;

    /// Reconfigure the bus bitrate. Most backends require the bus to
    /// be in a stopped state; it's safe to call at init time and
    /// again whenever the host wants to change rate mid-session (the
    /// bootloader doesn't support rate changes today, but the trait
    /// is shaped for that future).
    async fn set_bitrate(&self, nominal_bps: u32) -> Result<()>;

    /// Current bus load, `0.0..=1.0`. Backends that don't measure
    /// this return `0.0`.
    fn bus_load(&self) -> f32 {
        0.0
    }

    /// `true` when the adapter supplies hardware-provided timestamps
    /// on received frames. Relevant for PCAN-FD models; false for
    /// SLCAN, virtual, most SocketCAN configurations.
    fn has_hw_timestamps(&self) -> bool {
        false
    }

    /// Human-readable description used in logs and the audit-log
    /// row. Example: `"CANable 2.0 (USB 1d50:606f)"`.
    fn description(&self) -> String;

    /// Monotonic count of adapter-reported errors since this backend
    /// was opened. "Adapter error" here is a low-level refusal from
    /// the adapter itself (as opposed to a NACK from the bootloader)
    /// — on SLCAN this is every BEL byte, which signals bus-off, TX
    /// buffer full, or stuck-dominant. Other backends that don't
    /// distinguish adapter from bus errors return 0 and rely on
    /// their own `recv()`/`send()` error types.
    ///
    /// Callers snapshot this before and after an operation to
    /// detect adapter-level failures that would otherwise only
    /// surface as a `TransportError::Timeout` after the full
    /// `command_timeout` — turning a 5 s silent wait into an
    /// immediate, informative error.
    ///
    /// Default returns 0. Non-zero is specific to SLCAN today.
    fn adapter_error_count(&self) -> u32 {
        0
    }
}

/// Router: pick the right backend for the given `--interface` /
/// `--channel` combination and return it as a `Box<dyn CanBackend>`.
///
/// Must be called from within a tokio runtime context — the virtual
/// arm spawns a [`StubDevice`] task via `tokio::spawn`.
pub fn open_backend(
    iface: InterfaceType,
    channel: Option<&str>,
    bitrate: u32,
) -> Result<Box<dyn CanBackend>> {
    match iface {
        InterfaceType::Slcan => {
            let channel = channel.ok_or_else(|| TransportError::InvalidChannel {
                channel: String::new(),
                reason: "--channel required for --interface slcan (e.g. /dev/ttyACM0, \
                          /dev/cu.usbmodemNNN, COM3)"
                    .into(),
            })?;
            Ok(Box::new(slcan::SlcanBackend::open(channel, bitrate)?))
        }
        InterfaceType::Socketcan => {
            #[cfg(target_os = "linux")]
            {
                let channel = channel.ok_or_else(|| TransportError::InvalidChannel {
                    channel: String::new(),
                    reason: "--channel required for --interface socketcan (e.g. can0, \
                              vcan0)"
                        .into(),
                })?;
                Ok(Box::new(socketcan::SocketCanBackend::open(channel)?))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (channel, bitrate);
                Err(TransportError::AdapterMissing {
                    name: "socketcan",
                    reason: "SocketCAN is Linux-only — use --interface slcan or \
                             --interface pcan on macOS / Windows"
                        .into(),
                })
            }
        }
        InterfaceType::Pcan => {
            #[cfg(target_os = "linux")]
            {
                // On Linux the `peak_usb` kernel module exposes PCAN
                // adapters as SocketCAN interfaces, so --interface pcan
                // and --interface socketcan land on the same code
                // path — the user still picks a SocketCAN channel
                // name (usually `can0`).
                let channel = channel.ok_or_else(|| TransportError::InvalidChannel {
                    channel: String::new(),
                    reason: "--channel required for --interface pcan on Linux; use \
                              the SocketCAN interface name the peak_usb module \
                              exposed (e.g. can0)"
                        .into(),
                })?;
                Ok(Box::new(socketcan::SocketCanBackend::open(channel)?))
            }
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            {
                let channel = channel.ok_or_else(|| TransportError::InvalidChannel {
                    channel: String::new(),
                    reason: "--channel required for --interface pcan (e.g. \
                              PCAN_USBBUS1)"
                        .into(),
                })?;
                Ok(Box::new(pcan::PcanBackend::open(channel, bitrate)?))
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
            {
                let _ = (channel, bitrate);
                Err(TransportError::AdapterMissing {
                    name: "pcan",
                    reason: "PCAN is supported on Linux (via SocketCAN + peak_usb), \
                             Windows, and macOS only"
                        .into(),
                })
            }
        }
        InterfaceType::Virtual => {
            // `--channel` is ignored for the virtual bus; whatever
            // string the user passes just identifies their test
            // scenario in the audit log. `--bitrate` is similarly
            // advisory (a virtual bus has no wire rate).
            let _ = (channel, bitrate);
            // The stub answers from node 0x3 — pick a non-host,
            // non-broadcast ID so routing behaves like a real device.
            // Can be made configurable later if a `--node-id` use
            // case shows up.
            const STUB_NODE_ID: u8 = 0x3;
            Ok(Box::new(virtual_bus::StubLoopback::new(STUB_NODE_ID)?))
        }
    }
}
