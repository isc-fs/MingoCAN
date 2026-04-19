//! `adapters` subcommand — enumerate detectable CAN adapters on the
//! current machine.
//!
//! Enumeration logic (per REQUIREMENTS.md § adapters subcommand):
//!
//! - **SLCAN**: walk serial ports and filter by known USB VID/PID list.
//! - **PCAN**: attempt to load PCAN-Basic; if found, probe each channel
//!   constant and collect the ones that come back `PCAN_ERROR_OK`.
//! - **SocketCAN** (Linux only): enumerate `/sys/class/net/*` entries
//!   with `type == 280`.
//!
//! All three paths land in `feat/4…6` as the respective backends come
//! online. Until then this stub does nothing useful but keeps `--help`
//! stable so the UX shape is already visible to consumers (CI scripts,
//! docs, etc.).

use anyhow::{bail, Result};

use super::GlobalFlags;

pub async fn run(_global: &GlobalFlags) -> Result<()> {
    bail!(
        "`adapters` is not implemented yet — pending feat branches for \
         each backend's enumeration helper. See REQUIREMENTS.md § \
         adapters subcommand."
    )
}
