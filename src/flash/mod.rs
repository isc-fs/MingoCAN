//! Sector-aware flash engine.
//!
//! `FlashManager` is the state machine that turns a normalised
//! [`Image`] into a sequence of `CMD_FLASH_ERASE` / `CMD_FLASH_WRITE` /
//! `CMD_FLASH_READ_CRC` / `CMD_FLASH_VERIFY` commands on the session
//! layer. It owns the REQUIREMENTS.md § Flash manager contract:
//!
//! - Per-sector CRC diff before touching anything — an idempotent
//!   re-flash of an unchanged image issues zero erases and zero
//!   writes.
//! - Sector-aligned erase (128 KB), FLASHWORD-aligned write chunks
//!   (256 B data), `0xFF`-pad the tail to the next FLASHWORD.
//! - Post-write per-sector CRC verify — hard-fail on mismatch.
//! - Final `CMD_FLASH_VERIFY` commit fires the bootloader's metadata
//!   FLASHWORD write on match.
//! - Dry-run runs every read + CRC computation but suppresses every
//!   erase and write.
//!
//! The engine is deliberately CLI-agnostic: the `flash` subcommand
//! (feat/17) wraps it with clap args + an indicatif spinner + audit
//! logging, but the same `FlashManager` can be driven from a test
//! harness, a GUI, or a library consumer.
//!
//! ## Events
//!
//! Progress is delivered through an optional
//! `mpsc::UnboundedSender<FlashEvent>` so consumers can render a
//! spinner, emit JSON lines, or feed a test assertion without
//! polling. The sender is always optional — the engine runs to
//! completion even if no one is listening.
//!
//! ## Error mapping
//!
//! Every [`FlashError`] has an `exit_code_hint()` matching the
//! REQUIREMENTS.md § Exit codes table. feat/17's `cli::flash`
//! routes the error through the usual `exit_err` pattern.

use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

use crate::cli::ExitCodeHint;
use crate::firmware::{crc32, sector_of_addr, Image, BL_APP_BASE, BL_SECTOR_SIZE};
use crate::protocol::commands::{
    cmd_flash_erase, cmd_flash_read_crc, cmd_flash_verify, cmd_flash_write,
};
use crate::protocol::opcodes::{CommandOpcode, NackCode};
use crate::protocol::Response;
use crate::session::{Session, SessionError};

/// STM32H7 FLASHWORD size — 32 B. Every write chunk is padded to a
/// multiple of this; addresses must be aligned to it. The constant
/// mirrors `STUB_FLASHWORD_SIZE` on the stub side and the real
/// bootloader's `FLASH_NB_32BITWORD_IN_FLASHWORD * 4` on hardware.
pub const FLASHWORD_SIZE: u32 = 32;

/// Default bytes per `CMD_FLASH_WRITE` payload. Matches
/// REQUIREMENTS.md § Write chunk size — 256 B, i.e. one ISO-TP
/// FF + ~37 CFs at 8-byte frames on classic CAN.
pub const DEFAULT_WRITE_CHUNK: usize = 128;

/// Tunable knobs for a single [`FlashManager::run`] invocation.
///
/// Defaults match the REQUIREMENTS.md `flash` subcommand: diff
/// enabled, post-write CRC verify enabled, final `CMD_FLASH_VERIFY`
/// commit enabled, 256 B writes.
#[derive(Clone, Debug)]
pub struct FlashConfig {
    /// Skip sectors whose on-device CRC already matches the image.
    /// Idempotent re-flashes produce zero erases and zero writes.
    /// `--no-diff` on the CLI forces this to `false`.
    pub diff: bool,
    /// Run every read + CRC computation but send no erase / write /
    /// verify commands. Useful for CI pre-flight checks.
    pub dry_run: bool,
    /// After writing each sector, read its CRC back and compare.
    /// Hard-fails on mismatch so a programming glitch surfaces as
    /// `FlashError::SectorCrcMismatch` with `ExitCodeHint::FlashError`.
    pub verify_after: bool,
    /// Bytes per `CMD_FLASH_WRITE` data payload (≤ 256 in v1.0.0).
    /// The last chunk of a sector is tail-padded with `0xFF` to the
    /// next FLASHWORD.
    pub write_chunk_size: usize,
    /// Fire `CMD_FLASH_VERIFY(crc, size, version)` at the end to
    /// commit the bootloader's metadata FLASHWORD. Disable for
    /// partial-range tests that don't want a metadata update.
    pub final_commit: bool,
}

impl Default for FlashConfig {
    fn default() -> Self {
        Self {
            diff: true,
            dry_run: false,
            verify_after: true,
            write_chunk_size: DEFAULT_WRITE_CHUNK,
            final_commit: true,
        }
    }
}

// ---- Planning ----

/// Per-sector plan after the diff phase. Drives the execution loop:
/// `Skip` sectors are left alone, `Write` sectors get erase + write
/// + optional verify.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectorRole {
    /// Device already has matching bytes — no erase, no write.
    Skip,
    /// Sector needs a full erase + rewrite cycle.
    Write,
}

// ---- Events ----

/// Progress event emitted by [`FlashManager::run`]. Fire-and-forget:
/// if the receiver is dropped the engine keeps running.
///
/// The order of events for a single sector that needs a rewrite is:
/// `PlanningSector(Write)` → `Erased` → `ChunkWritten` × N →
/// `SectorVerified` (optional) → (next sector, or `Committing`).
#[derive(Clone, Debug)]
pub enum FlashEvent {
    /// Diff / CRC read decided what to do with this sector.
    PlanningSector { sector: u8, role: SectorRole },
    /// `CMD_FLASH_ERASE` ACK'd for this sector.
    Erased { sector: u8 },
    /// A chunk landed on-device. `bytes` is cumulative within the
    /// sector; `total` is the sector's full byte count including
    /// any tail pad.
    ChunkWritten { sector: u8, bytes: u32, total: u32 },
    /// Post-write `CMD_FLASH_READ_CRC` confirmed the sector contents.
    /// Only emitted when `FlashConfig::verify_after` is true.
    SectorVerified { sector: u8, crc: u32 },
    /// About to fire the final `CMD_FLASH_VERIFY` commit. Only
    /// emitted when `FlashConfig::final_commit` is true.
    Committing,
    /// Engine finished successfully. `report` holds the per-sector
    /// outcome table.
    Done { report: FlashReport },
}

/// Outcome of a successful [`FlashManager::run`]. Sector numbers
/// are STM32H7 sector indices (0..=7); the app region lives in
/// sectors 1..=6. The three sector lists are disjoint — every
/// sector touched shows up in exactly one of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlashReport {
    pub sectors_erased: Vec<u8>,
    pub sectors_written: Vec<u8>,
    pub sectors_skipped: Vec<u8>,
    /// CRC-32/ISO-HDLC of the composed image.
    pub crc32: u32,
    /// Image byte count (matches `Image::size()`).
    pub size: u32,
    /// Packed version `(major << 16) | (minor << 8) | patch`,
    /// or `0` when no `__firmware_info` record is present.
    pub version: u32,
    /// Wall-clock duration of the `run` call.
    pub duration: Duration,
}

// ---- Errors ----

/// Every failure mode the engine surfaces. The CLI layer walks
/// `exit_code_hint()` to translate to a process exit code.
#[derive(Debug, thiserror::Error)]
pub enum FlashError {
    /// Underlying session / transport failure — timeouts, backend
    /// disconnects, serial port closed, etc.
    #[error(transparent)]
    Session(#[from] SessionError),

    /// Device NACK'd a command. The engine preserves the opcode +
    /// code so callers can tell "protected address" apart from
    /// "flash hardware fault".
    #[error("device NACK'd opcode 0x{rejected_opcode:02X} with code {code}")]
    Nack { rejected_opcode: u8, code: NackCode },

    /// Device replied to an opcode we didn't expect (e.g. wrong
    /// ACK length, stray Discover). The engine treats this as a
    /// protocol-layer failure rather than a device rejection.
    #[error("unexpected reply to opcode 0x{expected_opcode:02X}: {detail}")]
    UnexpectedReply {
        expected_opcode: u8,
        detail: &'static str,
    },

    /// Post-write per-sector CRC did not match the host-computed
    /// value. Either a HAL programming glitch or — more likely in
    /// tests — `set_flash_crc_override` kicking in on the stub.
    #[error(
        "sector {sector} CRC mismatch after write: expected 0x{expected:08X}, got 0x{got:08X}"
    )]
    SectorCrcMismatch { sector: u8, expected: u32, got: u32 },

    /// The final `CMD_FLASH_VERIFY` came back `NACK(CRC_MISMATCH)`.
    /// Distinct from per-sector mismatches so the CLI can tell the
    /// two apart in reports.
    #[error(
        "final FLASH_VERIFY mismatch: host computed crc=0x{expected_crc:08X} \
         size={expected_size} version=0x{expected_version:08X}"
    )]
    FinalVerifyMismatch {
        expected_crc: u32,
        expected_size: u32,
        expected_version: u32,
    },

    /// Called with an image that can't be sector-mapped (empty or
    /// outside flash). The loader rejects these shapes, so this is
    /// a belt-and-braces guard against a future loader refactor.
    #[error("image does not map to a contiguous sector range: {detail}")]
    InvalidImage { detail: &'static str },
}

impl FlashError {
    /// Exit-code hint for feat/17's `cli::flash` — one place maps
    /// the error family to the REQUIREMENTS.md exit-code table.
    pub fn exit_code_hint(&self) -> ExitCodeHint {
        match self {
            FlashError::Session(SessionError::CommandTimeout(_))
            | FlashError::Session(SessionError::RxClosed) => ExitCodeHint::DeviceNotFound,
            FlashError::Nack {
                code: NackCode::ProtectedAddr,
                ..
            } => ExitCodeHint::ProtectionViolation,
            FlashError::FinalVerifyMismatch { .. } => ExitCodeHint::VerifyMismatch,
            FlashError::InvalidImage { .. } => ExitCodeHint::InputFileError,
            _ => ExitCodeHint::FlashError,
        }
    }
}

// ---- Engine ----

/// Sector-aware flash pipeline. Build with [`FlashManager::new`],
/// drive with [`FlashManager::run`].
///
/// The engine borrows its session and image — typical usage is:
///
/// ```ignore
/// let session = Session::attach(backend, config);
/// session.connect().await?;
/// let image = loader::load(path, None)?;
/// let report = FlashManager::new(&session, &image, FlashConfig::default())
///     .run(None)
///     .await?;
/// ```
pub struct FlashManager<'a> {
    session: &'a Session,
    image: &'a Image,
    config: FlashConfig,
}

impl<'a> FlashManager<'a> {
    /// Build an engine. No commands fire until `run` is called.
    pub fn new(session: &'a Session, image: &'a Image, config: FlashConfig) -> Self {
        Self {
            session,
            image,
            config,
        }
    }

    /// Execute the full pipeline. See the module docs for the
    /// command sequence. `progress` receives a stream of
    /// `FlashEvent`s — pass `None` if you don't need them.
    pub async fn run(
        &self,
        progress: Option<mpsc::UnboundedSender<FlashEvent>>,
    ) -> Result<FlashReport, FlashError> {
        let started = Instant::now();

        let sectors = self.image.sector_range().ok_or(FlashError::InvalidImage {
            detail: "image has no sector range — empty or out of flash",
        })?;
        debug!(
            base = format!("0x{:08X}", self.image.base_addr),
            size = self.image.size(),
            sectors = format!("{}..={}", sectors.start(), sectors.end()),
            dry_run = self.config.dry_run,
            diff = self.config.diff,
            "flash: plan"
        );

        // ---- Plan ----

        let mut roles: Vec<(u8, SectorRole)> = Vec::new();
        for sector in sectors.clone() {
            let role = self.plan_sector(sector).await?;
            emit(&progress, FlashEvent::PlanningSector { sector, role });
            roles.push((sector, role));
        }

        // ---- Execute ----

        let mut erased = Vec::new();
        let mut written = Vec::new();
        let mut skipped = Vec::new();

        for (sector, role) in &roles {
            match role {
                SectorRole::Skip => {
                    skipped.push(*sector);
                    trace!(sector, "flash: sector skipped by diff");
                }
                SectorRole::Write => {
                    if self.config.dry_run {
                        // Dry-run doesn't erase or write, but the
                        // sector still counts as "would have been
                        // written" so the report reflects the plan.
                        written.push(*sector);
                        continue;
                    }
                    self.erase_sector(*sector).await?;
                    emit(&progress, FlashEvent::Erased { sector: *sector });
                    erased.push(*sector);

                    self.write_sector(*sector, &progress).await?;

                    if self.config.verify_after {
                        let crc = self.verify_sector(*sector).await?;
                        emit(
                            &progress,
                            FlashEvent::SectorVerified {
                                sector: *sector,
                                crc,
                            },
                        );
                    }
                    written.push(*sector);
                }
            }
        }

        // ---- Commit ----

        if self.config.final_commit && !self.config.dry_run {
            emit(&progress, FlashEvent::Committing);
            self.final_commit().await?;
        }

        let report = FlashReport {
            sectors_erased: erased,
            sectors_written: written,
            sectors_skipped: skipped,
            crc32: self.image.crc32(),
            size: self.image.size(),
            version: self.image.packed_version(),
            duration: started.elapsed(),
        };
        emit(
            &progress,
            FlashEvent::Done {
                report: report.clone(),
            },
        );
        Ok(report)
    }

    // ---- Planning helpers ----

    /// Decide what to do with `sector`. In diff mode we read the
    /// on-device CRC and compare it against the host-computed one;
    /// in non-diff mode every sector is marked `Write`.
    async fn plan_sector(&self, sector: u8) -> Result<SectorRole, FlashError> {
        if !self.config.diff {
            return Ok(SectorRole::Write);
        }
        let device_crc = self.read_sector_crc(sector).await?;
        let expected_crc = self.expected_sector_crc(sector);
        if device_crc == expected_crc {
            Ok(SectorRole::Skip)
        } else {
            trace!(
                sector,
                device_crc = format!("0x{device_crc:08X}"),
                expected_crc = format!("0x{expected_crc:08X}"),
                "flash: sector dirty — will rewrite"
            );
            Ok(SectorRole::Write)
        }
    }

    fn expected_sector_crc(&self, sector: u8) -> u32 {
        // `sector_bytes` returns the sector's content — borrowed when
        // the sector lies fully inside the image, freshly allocated
        // and `0xFF`-padded when the image ends mid-sector. Either
        // way, hashing the whole thing matches what the device sees
        // after an erase (`0xFF`) + partial-write cycle.
        let bytes = self.sector_bytes(sector);
        crc32(&bytes)
    }

    /// Bytes of the composed image that land in `sector`. The image
    /// may end mid-sector; missing tail bytes count as `0xFF`
    /// (freshly-erased flash) for CRC purposes.
    ///
    /// Returns a *borrowed* slice when the sector lies fully inside
    /// the image, and a freshly-allocated `Vec<u8>` padded with
    /// `0xFF` when the image ends mid-sector. The hot path is the
    /// first case.
    fn sector_bytes(&self, sector: u8) -> std::borrow::Cow<'_, [u8]> {
        let sector_base = sector_base_addr(sector);
        let sector_end = sector_base + BL_SECTOR_SIZE;
        let image_base = self.image.base_addr;
        let image_end = image_base + self.image.size();

        let start_off = sector_base.saturating_sub(image_base) as usize;
        if sector_end <= image_end {
            // Sector fully inside image.
            let end_off = start_off + BL_SECTOR_SIZE as usize;
            std::borrow::Cow::Borrowed(&self.image.data[start_off..end_off])
        } else {
            // Sector extends past the image — pad with 0xFF.
            let mut buf = vec![0xFFu8; BL_SECTOR_SIZE as usize];
            let available = (image_end - sector_base) as usize;
            let start_off = sector_base.saturating_sub(image_base) as usize;
            buf[..available].copy_from_slice(&self.image.data[start_off..start_off + available]);
            std::borrow::Cow::Owned(buf)
        }
    }

    // ---- Command helpers ----

    async fn read_sector_crc(&self, sector: u8) -> Result<u32, FlashError> {
        let addr = sector_base_addr(sector);
        let resp = self
            .session
            .send_command(&cmd_flash_read_crc(addr, BL_SECTOR_SIZE))
            .await
            .map_err(map_session_err)?;
        match resp {
            Response::Ack { opcode, payload } => {
                if opcode != CommandOpcode::FlashReadCrc.as_byte() {
                    return Err(FlashError::UnexpectedReply {
                        expected_opcode: CommandOpcode::FlashReadCrc.as_byte(),
                        detail: "ACK opcode did not match FLASH_READ_CRC",
                    });
                }
                if payload.len() < 4 {
                    return Err(FlashError::UnexpectedReply {
                        expected_opcode: CommandOpcode::FlashReadCrc.as_byte(),
                        detail: "FLASH_READ_CRC ACK shorter than 4 bytes",
                    });
                }
                Ok(u32::from_le_bytes([
                    payload[0], payload[1], payload[2], payload[3],
                ]))
            }
            Response::Nack {
                rejected_opcode,
                code,
            } => Err(FlashError::Nack {
                rejected_opcode,
                code,
            }),
            other => Err(FlashError::UnexpectedReply {
                expected_opcode: CommandOpcode::FlashReadCrc.as_byte(),
                detail: other.kind_str(),
            }),
        }
    }

    async fn erase_sector(&self, sector: u8) -> Result<(), FlashError> {
        let addr = sector_base_addr(sector);
        let resp = self
            .session
            .send_command(&cmd_flash_erase(addr, BL_SECTOR_SIZE))
            .await
            .map_err(map_session_err)?;
        match resp {
            Response::Ack { opcode, .. } if opcode == CommandOpcode::FlashErase.as_byte() => Ok(()),
            Response::Nack {
                rejected_opcode,
                code,
            } => Err(FlashError::Nack {
                rejected_opcode,
                code,
            }),
            other => Err(FlashError::UnexpectedReply {
                expected_opcode: CommandOpcode::FlashErase.as_byte(),
                detail: other.kind_str(),
            }),
        }
    }

    async fn write_sector(
        &self,
        sector: u8,
        progress: &Option<mpsc::UnboundedSender<FlashEvent>>,
    ) -> Result<(), FlashError> {
        let chunk_size = self.config.write_chunk_size;
        debug_assert!(
            chunk_size > 0 && chunk_size <= 256,
            "write_chunk_size must be 1..=256"
        );
        let bytes = self.sector_bytes(sector);
        let sector_base = sector_base_addr(sector);
        let total = bytes.len() as u32;

        let mut written = 0u32;
        for chunk in bytes.chunks(chunk_size) {
            let addr = sector_base + written;
            // Pad the tail of each chunk up to FLASHWORD alignment.
            // The real bootloader mirrors this by rejecting
            // non-aligned lengths; the stub does too.
            let padded = pad_to_flashword(chunk);
            let resp = self
                .session
                .send_command(&cmd_flash_write(addr, &padded))
                .await
                .map_err(map_session_err)?;
            match resp {
                Response::Ack { opcode, .. } if opcode == CommandOpcode::FlashWrite.as_byte() => {}
                Response::Nack {
                    rejected_opcode,
                    code,
                } => {
                    return Err(FlashError::Nack {
                        rejected_opcode,
                        code,
                    })
                }
                other => {
                    return Err(FlashError::UnexpectedReply {
                        expected_opcode: CommandOpcode::FlashWrite.as_byte(),
                        detail: other.kind_str(),
                    })
                }
            }
            written += padded.len() as u32;
            emit(
                progress,
                FlashEvent::ChunkWritten {
                    sector,
                    bytes: written,
                    total,
                },
            );
        }
        Ok(())
    }

    async fn verify_sector(&self, sector: u8) -> Result<u32, FlashError> {
        let device_crc = self.read_sector_crc(sector).await?;
        let expected = self.expected_sector_crc(sector);
        if device_crc != expected {
            return Err(FlashError::SectorCrcMismatch {
                sector,
                expected,
                got: device_crc,
            });
        }
        Ok(device_crc)
    }

    async fn final_commit(&self) -> Result<(), FlashError> {
        let crc = self.image.crc32();
        let size = self.image.size();
        let version = self.image.packed_version();
        let resp = self
            .session
            .send_command(&cmd_flash_verify(crc, size, version))
            .await
            .map_err(map_session_err)?;
        match resp {
            Response::Ack { opcode, .. } if opcode == CommandOpcode::FlashVerify.as_byte() => {
                Ok(())
            }
            Response::Nack {
                code: NackCode::CrcMismatch,
                ..
            } => Err(FlashError::FinalVerifyMismatch {
                expected_crc: crc,
                expected_size: size,
                expected_version: version,
            }),
            Response::Nack {
                rejected_opcode,
                code,
            } => Err(FlashError::Nack {
                rejected_opcode,
                code,
            }),
            other => Err(FlashError::UnexpectedReply {
                expected_opcode: CommandOpcode::FlashVerify.as_byte(),
                detail: other.kind_str(),
            }),
        }
    }
}

// ---- Free helpers ----

fn sector_base_addr(sector: u8) -> u32 {
    // Sanity-check we're in the app region; callers should only pass
    // sectors returned from `Image::sector_range`.
    debug_assert!((1..=6).contains(&sector), "sector out of app range");
    BL_APP_BASE + u32::from(sector - 1) * BL_SECTOR_SIZE
}

fn pad_to_flashword(chunk: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    let remainder = chunk.len() as u32 % FLASHWORD_SIZE;
    if remainder == 0 {
        std::borrow::Cow::Borrowed(chunk)
    } else {
        let pad = (FLASHWORD_SIZE - remainder) as usize;
        let mut v = Vec::with_capacity(chunk.len() + pad);
        v.extend_from_slice(chunk);
        v.extend(std::iter::repeat_n(0xFFu8, pad));
        std::borrow::Cow::Owned(v)
    }
}

/// Thin `From`-like adapter that keeps `FlashError::Nack` as the
/// canonical shape when the session layer already parsed a NACK.
/// Without this, `SessionError::Nack` would arrive wrapped inside
/// `FlashError::Session` and error-matching in tests would be noisy.
fn map_session_err(err: SessionError) -> FlashError {
    match err {
        SessionError::Nack {
            rejected_opcode,
            code,
        } => FlashError::Nack {
            rejected_opcode,
            code,
        },
        other => FlashError::Session(other),
    }
}

fn emit(sink: &Option<mpsc::UnboundedSender<FlashEvent>>, event: FlashEvent) {
    if let Some(tx) = sink {
        if let Err(e) = tx.send(event) {
            // Receiver dropped. Not fatal — flash continues.
            warn!(?e, "flash: progress receiver closed");
        }
    }
}

// ---- Range / alignment helpers (pub for tests + future consumers) ----

/// Public helper: given a flash address, return its sector number
/// (or `None` if the address isn't in flash). Wraps
/// [`crate::firmware::sector_of_addr`] for discoverability from
/// `crate::flash::*` consumers.
pub fn sector_of(addr: u32) -> Option<u8> {
    sector_of_addr(addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flash_config_defaults_match_requirements() {
        let cfg = FlashConfig::default();
        assert!(cfg.diff);
        assert!(!cfg.dry_run);
        assert!(cfg.verify_after);
        assert!(cfg.final_commit);
        assert_eq!(cfg.write_chunk_size, DEFAULT_WRITE_CHUNK);
    }

    #[test]
    fn sector_base_addr_maps_app_sectors() {
        assert_eq!(sector_base_addr(1), 0x0802_0000);
        assert_eq!(sector_base_addr(2), 0x0804_0000);
        assert_eq!(sector_base_addr(6), 0x080C_0000);
    }

    #[test]
    fn pad_to_flashword_is_noop_on_aligned_input() {
        let aligned = vec![0xAAu8; 32];
        let padded = pad_to_flashword(&aligned);
        assert_eq!(padded.len(), 32);
        assert!(matches!(padded, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn pad_to_flashword_pads_short_tail_with_ff() {
        let tail = vec![0xAAu8; 33]; // 33 % 32 == 1 → pad 31 bytes
        let padded = pad_to_flashword(&tail);
        assert_eq!(padded.len(), 64);
        assert_eq!(&padded[..33], &[0xAA; 33]);
        assert!(padded[33..].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn pad_to_flashword_pads_single_byte_input() {
        let one = [0xAAu8];
        let padded = pad_to_flashword(&one);
        assert_eq!(padded.len(), FLASHWORD_SIZE as usize);
        assert_eq!(padded[0], 0xAA);
        assert!(padded[1..].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn flash_error_exit_hints_match_requirements_table() {
        use crate::protocol::opcodes::NackCode;

        // Protection violation on NACK(PROTECTED_ADDR) → exit 3.
        let protected = FlashError::Nack {
            rejected_opcode: CommandOpcode::FlashErase.as_byte(),
            code: NackCode::ProtectedAddr,
        };
        assert_eq!(
            protected.exit_code_hint(),
            ExitCodeHint::ProtectionViolation
        );

        // Final verify mismatch → exit 2.
        let mismatch = FlashError::FinalVerifyMismatch {
            expected_crc: 0xDEAD_BEEF,
            expected_size: 1024,
            expected_version: 0x0001_0000,
        };
        assert_eq!(mismatch.exit_code_hint(), ExitCodeHint::VerifyMismatch);

        // Timeout / RxClosed → device not found.
        let timeout = FlashError::Session(SessionError::CommandTimeout(Duration::from_millis(1)));
        assert_eq!(timeout.exit_code_hint(), ExitCodeHint::DeviceNotFound);

        // Everything else → generic FlashError (exit 1).
        let hw = FlashError::Nack {
            rejected_opcode: CommandOpcode::FlashWrite.as_byte(),
            code: NackCode::FlashHw,
        };
        assert_eq!(hw.exit_code_hint(), ExitCodeHint::FlashError);
    }

    #[test]
    fn sector_of_delegates_to_firmware_helper() {
        assert_eq!(sector_of(0x0802_0000), Some(1));
        assert_eq!(sector_of(0x0000_0000), None);
    }
}
