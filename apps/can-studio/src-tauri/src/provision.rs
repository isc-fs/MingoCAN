// Tauri command wrapping `cf provision <role>` for the Flash tab's
// "provision after flash" toggle. Mirrors the wire shape of the
// CLI's NVM_WRITE + CMD_RESET flow exactly — same session
// connect → write → reset → disconnect dance, same Bootloader
// reset mode so the board comes back up in the BL with the new
// node-id resolved from NVM.
//
// Kept as its own module rather than folded into flash.rs because
// the call surface is small and the responsibility is distinct:
// `flash` writes app firmware over CAN, `provision` writes one
// NVM key. Both are end-of-flow operations the operator may want
// to chain together, but they share no state.

use std::time::Duration;

use serde::Deserialize;

use can_flasher::cli::InterfaceType;
use can_flasher::protocol::commands::{cmd_nvm_write, cmd_reset};
use can_flasher::protocol::records::ResetMode;
use can_flasher::protocol::response::Response;
use can_flasher::session::{Session, SessionConfig};
use can_flasher::transport::open_backend;

/// The host's name for the bootloader's `BL_NVM_KEY_NODE_ID`.
const BL_NVM_KEY_NODE_ID: u16 = 0x0001;

/// Roles → node-id, kept in lockstep with `src/cli/provision.rs`
/// in the can-flasher crate. Three entries today; grows when the
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
    /// Adapter selection. Same shape as `FlashRequest` —
    /// mirror its fields so the Flash tab can pass the same
    /// adapter context.
    pub interface: String,
    pub channel: Option<String>,
    pub bitrate: u32,
    /// Target node ID. `None` means broadcast (0x3 in our
    /// session-default), used for fresh boards.
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

    // Resolve InterfaceType from the string (mirrors FlashRequest's
    // hand-rolled mapping — the Studio uses a String there rather
    // than serde'ing the enum directly so the UI stays simple).
    let interface = match request.interface.as_str() {
        "slcan" => InterfaceType::Slcan,
        "socketcan" => InterfaceType::Socketcan,
        "pcan" => InterfaceType::Pcan,
        "vector" => InterfaceType::Vector,
        "virtual" => InterfaceType::Virtual,
        other => return Err(format!("unknown interface {other:?}")),
    };

    let target_node = request.node_id.unwrap_or(0x3);
    let backend = open_backend(
        interface,
        request.channel.as_deref(),
        request.bitrate,
    )
    .map_err(|e| format!("open adapter: {e}"))?;

    let session = Session::open(
        backend,
        SessionConfig {
            target_node,
            timeout: Duration::from_millis(u64::from(request.timeout_ms)),
            keepalive_interval: Duration::from_millis(5_000),
        },
    )
    .map_err(|e| format!("open session: {e}"))?;

    session
        .connect()
        .await
        .map_err(|e| format!("CONNECT before NVM_WRITE: {e}"))?;

    // Write the NVM key. We don't disconnect early — same reason
    // the CLI's run_nvm_write doesn't: Session::disconnect(self)
    // consumes by value, and we need the session alive to fire
    // CMD_RESET afterwards.
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

    // Fire-and-forget CMD_RESET[Bootloader]. The chip reboots
    // before sending an ACK so the send call typically returns an
    // error — that's expected, swallow it. The real verification
    // of "did the new node-id take" is the operator's next
    // `discover`.
    let _reset_send = session
        .send_command(&cmd_reset(ResetMode::Bootloader))
        .await;
    let _ = session.disconnect().await;

    Ok(())
}
