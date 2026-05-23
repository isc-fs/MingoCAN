// Tauri command for the Flash tab's "provision after flash"
// toggle. Mirrors the wire shape of the CLI's NVM_WRITE + CMD_RESET
// flow exactly — same session connect → write → reset → disconnect
// dance, same Bootloader reset mode so the board comes back up in
// the BL with the new node-id resolved from NVM.
//
// Kept as its own module rather than folded into flash.rs because
// the call surface is small and the responsibility is distinct:
// `flash` writes app firmware over CAN, `provision` writes one NVM
// key. Both are end-of-flow operations the operator may want to
// chain together, but they share no state.
//
// API paths mirror the CLI's `src/cli/config.rs::run_nvm_write`
// verbatim — if that file's session-open pattern works, this one
// works.

use std::time::Duration;

use serde::Deserialize;

use can_flasher::protocol::commands::{cmd_nvm_write, cmd_reset};
use can_flasher::protocol::opcodes::ResetMode;
use can_flasher::protocol::Response;
use can_flasher::session::{Session, SessionConfig};
use can_flasher::transport::open_backend;

use crate::flash::parse_interface;

/// The host's name for the bootloader's `BL_NVM_KEY_NODE_ID`.
const BL_NVM_KEY_NODE_ID: u16 = 0x0001;

/// Roles → node-id. Kept in lockstep with the CLI registry in
/// `src/cli/provision.rs`. Three entries today; grows when the
/// team adds boards to the shared bus.
fn role_to_node_id(role: &str) -> Option<u8> {
    match role.trim().to_ascii_lowercase().as_str() {
        "ecu" => Some(0x01),
        "ams" => Some(0x02),
        "udv" => Some(0x03),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionRequest {
    /// `ecu`, `ams`, or `udv` (case-insensitive).
    pub role: String,
    /// Adapter selection — mirrors `FlashRequest`'s shape so the
    /// Flash tab can hand its own values through unchanged.
    pub interface: String,
    pub channel: Option<String>,
    pub bitrate: u32,
    /// Target node id. `None` means use the session default
    /// (0x3, broadcast-ish for the current bus layout). Operators
    /// re-provisioning an already-numbered board pass its current
    /// id so the session reaches the right device.
    pub node_id: Option<u8>,
    pub timeout_ms: u32,
}

#[tauri::command]
pub async fn provision_node_id(request: ProvisionRequest) -> Result<(), String> {
    let new_node_id = role_to_node_id(&request.role).ok_or_else(|| {
        format!(
            "unknown role {:?}; expected one of: ecu, ams, udv",
            request.role
        )
    })?;

    let interface = parse_interface(&request.interface).map_err(|e| format!("interface: {e}"))?;
    let backend = open_backend(interface, request.channel.as_deref(), request.bitrate)
        .map_err(|e| format!("open adapter: {e}"))?;

    let target_node = request.node_id.unwrap_or(0x3);
    // `Session::attach` is infallible — same call shape as the
    // existing flash.rs Tauri command. SessionConfig has more
    // fields than we set; `..SessionConfig::default()` keeps the
    // wire-protocol knobs at their library defaults.
    let session = Session::attach(
        backend,
        SessionConfig {
            target_node,
            keepalive_interval: Duration::from_millis(5_000),
            command_timeout: Duration::from_millis(u64::from(request.timeout_ms)),
            ..SessionConfig::default()
        },
    );
    session
        .connect()
        .await
        .map_err(|e| format!("CONNECT before NVM_WRITE: {e}"))?;

    // Stage 1 — NVM_WRITE. We don't disconnect early: the
    // following CMD_RESET needs the session alive, and
    // `Session::disconnect(self)` consumes the session by value.
    let write_resp = session
        .send_command(&cmd_nvm_write(BL_NVM_KEY_NODE_ID, &[new_node_id]))
        .await;
    let write_resp = match write_resp {
        Ok(r) => r,
        Err(err) => {
            let _ = session.disconnect().await;
            return Err(format!("sending NVM_WRITE: {err}"));
        }
    };
    match write_resp {
        Response::Ack { .. } => { /* fall through to reset */ }
        Response::Nack {
            rejected_opcode,
            code,
        } => {
            let _ = session.disconnect().await;
            return Err(format!(
                "device NACK'd NVM_WRITE (opcode 0x{rejected_opcode:02X}): {code}"
            ));
        }
        other => {
            let kind = other.kind_str();
            let _ = session.disconnect().await;
            return Err(format!("unexpected reply to NVM_WRITE: {kind}"));
        }
    }

    // Stage 2 — fire-and-forget CMD_RESET[Bootloader]. The chip
    // reboots before sending an ACK so the send call typically
    // returns an error; that's expected, swallow it. The real
    // verification of "did the new node-id take" is the operator's
    // next `discover` (or the next CAN flash they run).
    let _reset_send = session
        .send_command(&cmd_reset(ResetMode::Bootloader))
        .await;
    let _ = session.disconnect().await;

    Ok(())
}
