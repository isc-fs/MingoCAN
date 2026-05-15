//! can-flasher — host-side library surface.
//!
//! Everything the binary uses is also publicly available here so
//! integration tests under `tests/` and any future external consumer
//! can import the typed API without going through CLI argument
//! parsing. The CLI itself (`main.rs`) is a thin entry point that
//! parses `clap` args and dispatches into [`cli`].

#![doc(html_root_url = "https://docs.rs/can-flasher")]

pub mod cli;
pub mod firmware;
pub mod flash;
pub mod logging;
pub mod protocol;
pub mod session;
#[cfg(feature = "swd")]
pub mod swd;
pub mod transport;
