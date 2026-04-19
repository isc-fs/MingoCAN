//! In-process stub bootloader.
//!
//! A tiny `run` loop that pretends to be the STM32 CAN bootloader
//! for the purposes of integration tests. Just enough protocol
//! behaviour to drive a full host-side pipeline through ISO-TP
//! segmentation + reassembly + opcode dispatch without hardware.
//!
//! ## Implemented opcodes (feat/4 scope)
//!
//! - `CMD_CONNECT` — replies with ACK echoing the host's protocol
//!   version, flips a local "session active" flag.
//! - `CMD_DISCONNECT` — replies with ACK, clears the session flag.
//! - `CMD_DISCOVER` — replies with `[CMD_DISCOVER, node_id, major,
//!   minor]` as `TYPE=DISCOVER`.
//!
//! Every other opcode earns `NACK(UNSUPPORTED)`. Later feat branches
//! extend the stub as real subcommands land (feat/5 adds GET_HEALTH
//! for the discover UI, feat/6 adds a flash pipeline, etc.).
//!
//! ## What the stub doesn't model
//!
//! - 30 s session watchdog — the real bootloader drops a stale
//!   session; the stub keeps the latch until `CMD_DISCONNECT`.
//! - NOTIFY_HEARTBEAT / NOTIFY_LOG / NOTIFY_LIVE_DATA streams.
//! - Flash programming of any kind.
//!
//! These land as the features that need them do.

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::oneshot;
use tracing::{debug, trace, warn};

use crate::protocol::commands::{PROTOCOL_VERSION_MAJOR, PROTOCOL_VERSION_MINOR};
use crate::protocol::ids::{FrameId, MessageType};
use crate::protocol::isotp::{IsoTpSegmenter, ReassembleOutcome, Reassembler};
use crate::protocol::opcodes::{CommandOpcode, NackCode};
use crate::protocol::records::OB_APPLY_TOKEN;
use crate::protocol::CanFrame;

use super::{CanBackend, Result, TransportError};

/// Upper bound on a single NVM value, per REQUIREMENTS.md § NVM
/// store — matches the bootloader's `BL_NVM_MAX_VALUE_LEN` (20 B).
const STUB_NVM_MAX_VALUE_LEN: usize = 20;

/// Stub's synthetic option-byte snapshot — nothing WRP-protected,
/// level-0 RDP, default BOR. Matches what a dev board out of the
/// box would report.
const STUB_OB_RDP_LEVEL: u8 = 0xAA; // OB_RDP_LEVEL_0
const STUB_OB_BOR_LEVEL: u8 = 0x00;
const STUB_OB_USER_CONFIG: u32 = 0x00FF_FAAD; // arbitrary realistic-looking value

/// How long we wait between frames before giving up the current read
/// attempt and returning to the top of the loop. Short enough that
/// a shutdown signal is picked up promptly, long enough that the
/// loop is quiet while idle.
const READ_SLICE: Duration = Duration::from_millis(50);

/// Minimal stub bootloader. Spin it up in a tokio task; it runs
/// until either the cancel handle fires or the underlying backend
/// disconnects.
pub struct StubDevice {
    backend: Box<dyn CanBackend>,
    node_id: u8,
    reasm: Reassembler,
    session_active: bool,
    /// Message type carried by the last SF or FF seen. CFs ride as
    /// `TYPE=DATA` per the bootloader's ISO-TP convention, so the
    /// reassembler (which operates purely on frame payload bytes)
    /// can't know the originating type — we capture it at the
    /// frame-ID layer and consume it when the reassembly completes.
    pending_msg_type: Option<MessageType>,
    /// In-memory NVM store. Real bootloader persists to sector 7;
    /// stub keeps it RAM-only per run so integration tests get a
    /// fresh KV space each time. Tombstoned entries get evicted
    /// (map entry removed) so `NVM_READ` after erase surfaces
    /// `NACK(NVM_NOT_FOUND)` as expected.
    nvm: HashMap<u16, Vec<u8>>,
    /// Current WRP sector bitmap. Real hardware OB flips this via
    /// `OB_APPLY_WRP`; the stub mirrors the operation so subsequent
    /// `OB_READ` reflects the applied mask. Reset to 0 on construction.
    wrp_sector_mask: u32,
    /// If set, `CMD_FLASH_VERIFY` only ACKs when the submitted
    /// `(crc, size, version)` triple matches exactly. `None` means
    /// "accept whatever the host sends" — useful for happy-path
    /// tests where we just want to confirm the wire round-trip
    /// without pretending to have a specific installed image.
    expected_verify: Option<(u32, u32, u32)>,
}

impl StubDevice {
    /// Wrap a `CanBackend` as a stub device answering from `node_id`
    /// (4-bit, 1..=14 — `0x0` is reserved for the host, `0xF` for
    /// broadcast).
    pub fn new(backend: Box<dyn CanBackend>, node_id: u8) -> Self {
        debug_assert!(node_id != 0 && node_id != 0xF, "invalid stub node id");
        Self {
            backend,
            node_id,
            reasm: Reassembler::new(),
            session_active: false,
            pending_msg_type: None,
            nvm: HashMap::new(),
            wrp_sector_mask: 0,
            expected_verify: None,
        }
    }

    /// Configure the `CMD_FLASH_VERIFY` gate. `Some((crc, size,
    /// version))` means the stub ACKs only that exact triple and
    /// NACKs anything else with `CrcMismatch`. `None` (the default)
    /// ACKs any well-formed verify request — useful when a test just
    /// cares about the wire path and not the semantics.
    pub fn set_expected_verify(&mut self, expected: Option<(u32, u32, u32)>) {
        self.expected_verify = expected;
    }

    /// Run the dispatch loop. Terminates gracefully when `cancel`
    /// fires or the backend disconnects; both cases return `Ok(())`.
    /// Unexpected transport errors bubble up as-is.
    pub async fn run(mut self, mut cancel: oneshot::Receiver<()>) -> Result<()> {
        loop {
            tokio::select! {
                biased;
                _ = &mut cancel => {
                    debug!(node = self.node_id, "stub device: cancel signalled, exiting");
                    return Ok(());
                }
                frame = self.backend.recv(READ_SLICE) => {
                    match frame {
                        Ok(frame) => {
                            if let Err(err) = self.handle_frame(frame).await {
                                warn!(node = self.node_id, ?err, "stub device: handle_frame failed");
                                // A transport error is terminal; anything
                                // else we swallow and keep running.
                                if matches!(err, TransportError::Disconnected) {
                                    return Ok(());
                                }
                            }
                        }
                        Err(TransportError::Timeout(_)) => continue,
                        Err(TransportError::Disconnected) => {
                            debug!(node = self.node_id, "stub device: backend disconnected");
                            return Ok(());
                        }
                        Err(other) => return Err(other),
                    }
                }
            }
        }
    }

    async fn handle_frame(&mut self, frame: CanFrame) -> Result<()> {
        // Filter by the 11-bit ID: ignore frames addressed to a
        // different node unless they're broadcast.
        let id = match FrameId::decode(frame.id) {
            Ok(id) => id,
            Err(_) => {
                trace!(raw_id = frame.id, "stub: dropping frame with bad ID");
                return Ok(());
            }
        };
        if !id.addressed_to(self.node_id) {
            return Ok(());
        }

        // Capture the message type whenever the current frame carries
        // it — i.e. when it's an SF (PCI high nibble 0) or FF (PCI
        // high nibble 1). CFs (nibble 2) and FCs (nibble 3) ride as
        // TYPE=DATA and would overwrite the real type if we took
        // `id.message_type` unconditionally.
        let payload_bytes = frame.payload();
        if let Some(pci_hi) = payload_bytes.first().map(|b| b & 0xF0) {
            if pci_hi == 0x00 || pci_hi == 0x10 {
                self.pending_msg_type = Some(id.message_type);
            }
        }

        // Feed the ISO-TP reassembler. `now_ms` is a synthetic clock
        // — the stub doesn't run a timeout scheduler, the tick count
        // is just used to drive the reassembler's internal
        // bookkeeping.
        let now_ms = static_ms();
        let outcome = self.reasm.feed(payload_bytes, now_ms);

        match outcome {
            Ok(ReassembleOutcome::Ongoing) => Ok(()),
            Ok(ReassembleOutcome::Complete(payload)) => {
                let msg_type = self.pending_msg_type.take().unwrap_or(id.message_type);
                self.dispatch(id.src, msg_type, &payload).await
            }
            Err(_err) => {
                // The real bootloader emits NACK(TRANSPORT_*) here.
                // For the stub we bail silently — tests that care
                // about framing errors feed the reassembler directly.
                self.pending_msg_type = None;
                Ok(())
            }
        }
    }

    async fn dispatch(
        &mut self,
        peer: u8,
        message_type: MessageType,
        payload: &[u8],
    ) -> Result<()> {
        if message_type != MessageType::Cmd && message_type != MessageType::Discover {
            // Stub only answers CMD / DISCOVER; other types are
            // host-bound and shouldn't appear here.
            return Ok(());
        }

        let opcode = match payload.first() {
            Some(b) => *b,
            None => {
                trace!("stub: empty payload after reassembly");
                return Ok(());
            }
        };

        let decoded = CommandOpcode::try_from(opcode);
        match decoded {
            Ok(CommandOpcode::Connect) => self.handle_connect(peer, payload).await,
            Ok(CommandOpcode::Disconnect) => self.handle_disconnect(peer).await,
            Ok(CommandOpcode::Discover) => self.handle_discover(peer).await,
            Ok(CommandOpcode::GetHealth) => self.handle_get_health(peer).await,
            Ok(CommandOpcode::DtcRead) => self.handle_dtc_read(peer).await,
            Ok(CommandOpcode::DtcClear) => self.handle_dtc_clear(peer).await,
            Ok(CommandOpcode::LogStreamStart) => {
                self.handle_session_gated_ack(peer, CommandOpcode::LogStreamStart)
                    .await
            }
            Ok(CommandOpcode::LogStreamStop) => {
                self.handle_session_gated_ack(peer, CommandOpcode::LogStreamStop)
                    .await
            }
            Ok(CommandOpcode::LiveDataStart) => {
                self.handle_session_gated_ack(peer, CommandOpcode::LiveDataStart)
                    .await
            }
            Ok(CommandOpcode::LiveDataStop) => {
                self.handle_session_gated_ack(peer, CommandOpcode::LiveDataStop)
                    .await
            }
            Ok(CommandOpcode::Reset) => self.handle_reset(peer, payload).await,
            Ok(CommandOpcode::ObRead) => self.handle_ob_read(peer).await,
            Ok(CommandOpcode::ObApplyWrp) => self.handle_ob_apply_wrp(peer, payload).await,
            Ok(CommandOpcode::NvmRead) => self.handle_nvm_read(peer, payload).await,
            Ok(CommandOpcode::NvmWrite) => self.handle_nvm_write(peer, payload).await,
            Ok(CommandOpcode::FlashVerify) => self.handle_flash_verify(peer, payload).await,
            Ok(_) | Err(_) => self.send_nack(peer, opcode, NackCode::Unsupported).await,
        }
    }

    // ---- Handlers ----

    async fn handle_connect(&mut self, peer: u8, payload: &[u8]) -> Result<()> {
        if payload.len() < 3 {
            return self
                .send_nack(
                    peer,
                    CommandOpcode::Connect.as_byte(),
                    NackCode::Unsupported,
                )
                .await;
        }
        let host_major = payload[1];
        let host_minor = payload[2];
        if host_major != PROTOCOL_VERSION_MAJOR {
            return self
                .send_nack(
                    peer,
                    CommandOpcode::Connect.as_byte(),
                    NackCode::ProtocolVersion,
                )
                .await;
        }
        self.session_active = true;
        let resp = vec![
            CommandOpcode::Connect.as_byte(),
            PROTOCOL_VERSION_MAJOR,
            PROTOCOL_VERSION_MINOR,
        ];
        let _ = host_minor; // future: honour optional feature flags from minor
        self.send_message(peer, MessageType::Ack, &resp).await
    }

    async fn handle_disconnect(&mut self, peer: u8) -> Result<()> {
        self.session_active = false;
        let resp = vec![CommandOpcode::Disconnect.as_byte()];
        self.send_message(peer, MessageType::Ack, &resp).await
    }

    async fn handle_discover(&mut self, peer: u8) -> Result<()> {
        let resp = vec![
            CommandOpcode::Discover.as_byte(),
            self.node_id,
            PROTOCOL_VERSION_MAJOR,
            PROTOCOL_VERSION_MINOR,
        ];
        self.send_message(peer, MessageType::Discover, &resp).await
    }

    /// Synthetic `CMD_GET_HEALTH` reply. Produces a realistic-looking
    /// 32-byte [`HealthRecord`]: uptime comes from the
    /// [`static_ms`] monotonic clock, reset cause is latched at
    /// `POWER_ON`, and the `flags` bitmask reflects the stub's
    /// current `session_active` state. Matches `bl_health.h`'s layout
    /// exactly so host-side parsers see the same bytes they would on
    /// real hardware.
    ///
    /// [`HealthRecord`]: crate::protocol::records::HealthRecord
    async fn handle_get_health(&self, peer: u8) -> Result<()> {
        // HealthRecord::SIZE = 32. Lay the bytes out directly so the
        // stub doesn't need to pull in encoder machinery for a record
        // we only read on the host side.
        let uptime_seconds: u32 = (static_ms() / 1000) as u32;
        let reset_cause: u32 = 0x01; // BL_RESET_POWER_ON
        let flags: u32 = if self.session_active { 0b01 } else { 0b00 };

        let mut record = [0u8; 32];
        record[0..4].copy_from_slice(&uptime_seconds.to_le_bytes());
        record[4..8].copy_from_slice(&reset_cause.to_le_bytes());
        record[8..12].copy_from_slice(&flags.to_le_bytes());
        // flash_write_count, dtc_count, last_dtc_code, reserved[0..2] already 0

        let mut resp = Vec::with_capacity(1 + 32);
        resp.push(CommandOpcode::GetHealth.as_byte());
        resp.extend_from_slice(&record);
        self.send_message(peer, MessageType::Ack, &resp).await
    }

    /// Empty DTC table: `[opcode, count_le16=0]`. Zero entries.
    /// Matches the real bootloader's wire format for `CMD_DTC_READ`
    /// when no faults have been logged.
    async fn handle_dtc_read(&self, peer: u8) -> Result<()> {
        let resp = [CommandOpcode::DtcRead.as_byte(), 0x00, 0x00];
        self.send_message(peer, MessageType::Ack, &resp).await
    }

    /// Session-gated DTC clear. Returns `NACK(BAD_SESSION)` if the
    /// caller hasn't run CONNECT first; ACKs a successful clear
    /// otherwise. The stub's internal DTC list is already empty, so
    /// "clear" is a no-op.
    async fn handle_dtc_clear(&self, peer: u8) -> Result<()> {
        if !self.session_active {
            return self
                .send_nack(
                    peer,
                    CommandOpcode::DtcClear.as_byte(),
                    NackCode::BadSession,
                )
                .await;
        }
        let resp = [CommandOpcode::DtcClear.as_byte()];
        self.send_message(peer, MessageType::Ack, &resp).await
    }

    /// `CMD_RESET` takes a 1-byte mode argument. ACK the command so
    /// the host sees success; we don't actually reset the tokio task
    /// (would kill the test harness). Real hardware would reboot
    /// after emitting the ACK.
    async fn handle_reset(&self, peer: u8, payload: &[u8]) -> Result<()> {
        // payload[0] is the opcode, payload[1] should be the mode.
        if payload.len() < 2 {
            return self
                .send_nack(peer, CommandOpcode::Reset.as_byte(), NackCode::Unsupported)
                .await;
        }
        // Validate the mode is 0..=3; anything else is an invalid arg.
        if payload[1] > 3 {
            return self
                .send_nack(peer, CommandOpcode::Reset.as_byte(), NackCode::Unsupported)
                .await;
        }
        let resp = [CommandOpcode::Reset.as_byte()];
        self.send_message(peer, MessageType::Ack, &resp).await
    }

    /// Generic session-gated ACK handler. Used by `LOG_STREAM_*` and
    /// `LIVE_DATA_*` which on real hardware start / stop notification
    /// streams but here just accept the command so the host-side
    /// subscribe path can be exercised. Actual stream emission is out
    /// of scope for the stub today — hosts exercise it via real
    /// bootloader traffic.
    async fn handle_session_gated_ack(&self, peer: u8, opcode: CommandOpcode) -> Result<()> {
        if !self.session_active {
            return self
                .send_nack(peer, opcode.as_byte(), NackCode::BadSession)
                .await;
        }
        let resp = [opcode.as_byte()];
        self.send_message(peer, MessageType::Ack, &resp).await
    }

    /// Session-less `CMD_OB_READ`. Returns a synthetic 16-byte
    /// `bl_ob_status_t` reflecting the stub's current in-memory WRP
    /// mask plus fixed RDP / BOR levels.
    async fn handle_ob_read(&self, peer: u8) -> Result<()> {
        // 16 bytes of bl_ob_status_t: wrp (4 LE) + user_config (4 LE)
        // + rdp (1) + bor (1) + reserved (2) + reserved_ext (4 LE).
        let mut record = [0u8; 16];
        record[0..4].copy_from_slice(&self.wrp_sector_mask.to_le_bytes());
        record[4..8].copy_from_slice(&STUB_OB_USER_CONFIG.to_le_bytes());
        record[8] = STUB_OB_RDP_LEVEL;
        record[9] = STUB_OB_BOR_LEVEL;
        // bytes 10..12 and 12..16 stay 0 (reserved + reserved_ext).

        let mut resp = Vec::with_capacity(1 + 16);
        resp.push(CommandOpcode::ObRead.as_byte());
        resp.extend_from_slice(&record);
        self.send_message(peer, MessageType::Ack, &resp).await
    }

    /// Session-gated `CMD_OB_APPLY_WRP`. Validates the 4-byte
    /// `BL_OB_APPLY_TOKEN` brick-safety prefix and the optional
    /// 4-byte sector bitmap. On a good request the stub updates its
    /// in-memory WRP mask (so a subsequent `OB_READ` sees it) and
    /// ACKs. Real hardware would reset after the ACK drains; the
    /// stub just updates state in place so tests can inspect the
    /// result without reconnecting.
    async fn handle_ob_apply_wrp(&mut self, peer: u8, payload: &[u8]) -> Result<()> {
        if !self.session_active {
            return self
                .send_nack(
                    peer,
                    CommandOpcode::ObApplyWrp.as_byte(),
                    NackCode::BadSession,
                )
                .await;
        }
        // payload = [opcode, token_le32, sector_bitmap_le32?]
        if payload.len() < 1 + 4 {
            return self
                .send_nack(
                    peer,
                    CommandOpcode::ObApplyWrp.as_byte(),
                    NackCode::ObWrongToken,
                )
                .await;
        }
        let token = u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]);
        if token != OB_APPLY_TOKEN {
            return self
                .send_nack(
                    peer,
                    CommandOpcode::ObApplyWrp.as_byte(),
                    NackCode::ObWrongToken,
                )
                .await;
        }
        // Optional sector bitmap; default to 0x01 (bootloader sector)
        // to match the host helper's default.
        let sector_bitmap = if payload.len() >= 1 + 4 + 4 {
            u32::from_le_bytes([payload[5], payload[6], payload[7], payload[8]])
        } else {
            0x01
        };
        self.wrp_sector_mask |= sector_bitmap;

        let resp = [CommandOpcode::ObApplyWrp.as_byte()];
        self.send_message(peer, MessageType::Ack, &resp).await
    }

    /// Session-gated `CMD_NVM_READ`. Looks up `key` in the in-memory
    /// map; ACK payload is `[opcode, len, value…]` on hit,
    /// `NACK(NVM_NOT_FOUND)` on miss.
    async fn handle_nvm_read(&self, peer: u8, payload: &[u8]) -> Result<()> {
        if !self.session_active {
            return self
                .send_nack(peer, CommandOpcode::NvmRead.as_byte(), NackCode::BadSession)
                .await;
        }
        if payload.len() < 1 + 2 {
            return self
                .send_nack(
                    peer,
                    CommandOpcode::NvmRead.as_byte(),
                    NackCode::Unsupported,
                )
                .await;
        }
        let key = u16::from_le_bytes([payload[1], payload[2]]);
        match self.nvm.get(&key) {
            Some(value) => {
                let mut resp = Vec::with_capacity(1 + 1 + value.len());
                resp.push(CommandOpcode::NvmRead.as_byte());
                resp.push(value.len() as u8);
                resp.extend_from_slice(value);
                self.send_message(peer, MessageType::Ack, &resp).await
            }
            None => {
                self.send_nack(
                    peer,
                    CommandOpcode::NvmRead.as_byte(),
                    NackCode::NvmNotFound,
                )
                .await
            }
        }
    }

    /// Session-gated `CMD_NVM_WRITE`. `value_len == 0` is a
    /// tombstone and removes the key. Values longer than
    /// `STUB_NVM_MAX_VALUE_LEN` earn `NACK(UNSUPPORTED)`. Otherwise
    /// the stub stores the value and ACKs.
    async fn handle_nvm_write(&mut self, peer: u8, payload: &[u8]) -> Result<()> {
        if !self.session_active {
            return self
                .send_nack(
                    peer,
                    CommandOpcode::NvmWrite.as_byte(),
                    NackCode::BadSession,
                )
                .await;
        }
        if payload.len() < 1 + 2 {
            return self
                .send_nack(
                    peer,
                    CommandOpcode::NvmWrite.as_byte(),
                    NackCode::Unsupported,
                )
                .await;
        }
        let key = u16::from_le_bytes([payload[1], payload[2]]);
        let value = &payload[3..];
        if value.len() > STUB_NVM_MAX_VALUE_LEN {
            return self
                .send_nack(
                    peer,
                    CommandOpcode::NvmWrite.as_byte(),
                    NackCode::Unsupported,
                )
                .await;
        }

        if value.is_empty() {
            // Tombstone: drop the entry entirely so NVM_READ misses.
            self.nvm.remove(&key);
        } else {
            self.nvm.insert(key, value.to_vec());
        }

        let resp = [CommandOpcode::NvmWrite.as_byte()];
        self.send_message(peer, MessageType::Ack, &resp).await
    }

    /// Session-gated `CMD_FLASH_VERIFY`. Args on the wire are
    /// `[expected_crc_le32, expected_size_le32, expected_version_le32]`
    /// (after the opcode byte). Behaviour:
    ///
    /// - If `self.expected_verify` is `None`, the stub ACKs any
    ///   well-formed request. Happy-path tests just want to confirm
    ///   the wire round-trip, not simulate an installed image.
    /// - If `self.expected_verify` is `Some(exp)`, the stub ACKs
    ///   only when the submitted triple equals `exp`, otherwise
    ///   `NACK(CRC_MISMATCH)`. Tests set this via
    ///   `StubDevice::set_expected_verify` before `run` is called.
    async fn handle_flash_verify(&self, peer: u8, payload: &[u8]) -> Result<()> {
        if !self.session_active {
            return self
                .send_nack(
                    peer,
                    CommandOpcode::FlashVerify.as_byte(),
                    NackCode::BadSession,
                )
                .await;
        }
        // payload = [opcode, crc_le32, size_le32, version_le32]
        if payload.len() < 1 + 12 {
            return self
                .send_nack(
                    peer,
                    CommandOpcode::FlashVerify.as_byte(),
                    NackCode::Unsupported,
                )
                .await;
        }
        let crc = u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]);
        let size = u32::from_le_bytes([payload[5], payload[6], payload[7], payload[8]]);
        let version = u32::from_le_bytes([payload[9], payload[10], payload[11], payload[12]]);

        if let Some(expected) = self.expected_verify {
            if (crc, size, version) != expected {
                return self
                    .send_nack(
                        peer,
                        CommandOpcode::FlashVerify.as_byte(),
                        NackCode::CrcMismatch,
                    )
                    .await;
            }
        }

        let resp = [CommandOpcode::FlashVerify.as_byte()];
        self.send_message(peer, MessageType::Ack, &resp).await
    }

    async fn send_nack(&self, peer: u8, rejected_opcode: u8, code: NackCode) -> Result<()> {
        let payload = [rejected_opcode, code.as_byte()];
        self.send_message(peer, MessageType::Nack, &payload).await
    }

    /// Segment `payload` into ISO-TP frames and push each through
    /// the backend, keeping the bootloader convention: FF keeps the
    /// original TYPE, CFs travel as TYPE=DATA.
    async fn send_message(
        &self,
        peer: u8,
        message_type: MessageType,
        payload: &[u8],
    ) -> Result<()> {
        let segmenter = match IsoTpSegmenter::new(payload) {
            Ok(s) => s,
            Err(err) => {
                warn!(?err, "stub: segmenter rejected payload");
                return Ok(());
            }
        };

        let initial_id = FrameId::new(message_type, self.node_id, peer)
            .expect("valid node ids")
            .encode();
        let cf_id = FrameId::new(MessageType::Data, self.node_id, peer)
            .expect("valid node ids")
            .encode();

        for (idx, frame_bytes) in segmenter.enumerate() {
            let id = if idx == 0 { initial_id } else { cf_id };
            let frame = CanFrame {
                id,
                data: frame_bytes,
                len: frame_bytes.len() as u8,
            };
            self.backend.send(frame).await?;
        }
        Ok(())
    }
}

/// A monotonic "enough" millisecond counter used to feed the
/// reassembler. We don't need wall-clock accuracy — the reassembler
/// only compares timestamps for relative elapsed, and inside a single
/// process tokio's `Instant` would give the same answer with more
/// ceremony. `Instant::now()` via the std clock is fine here.
fn static_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    let start = *START.get_or_init(Instant::now);
    start.elapsed().as_millis() as u64
}
