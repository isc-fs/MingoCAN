//! STM32 DfuSe protocol (USB DFU 1.1 + ST's "DfuSe" extensions, AN3156)
//! and the bootloader-burn sequence built on it.
//!
//! Written against a two-method [`DfuTransport`] so the whole sequence —
//! erase, write, read-back verify, provisioning seed, leave — runs in
//! tests against a simulated device ([`tests::FakeDfu`]) with the same
//! state machine as the ROM bootloader.

use std::time::{Duration, Instant};

use super::layout::MemoryLayout;
use super::DfuError;
use crate::swd::{SwdOperation, SwdProgress};

// DFU class requests (DFU 1.1 §3).
const DFU_DNLOAD: u8 = 1;
const DFU_UPLOAD: u8 = 2;
const DFU_GETSTATUS: u8 = 3;
const DFU_CLRSTATUS: u8 = 4;
const DFU_ABORT: u8 = 6;

// DfuSe commands, sent as a DNLOAD of block 0 (AN3156 §6.1).
const DFUSE_SET_ADDRESS: u8 = 0x21;
const DFUSE_ERASE: u8 = 0x41;

/// `wBlockNum` of the first data block: the ROM maps block N to
/// `address_pointer + (N - 2) * wTransferSize`. We always point first and
/// send block 2, so every chunk's address is explicit.
const FIRST_DATA_BLOCK: u16 = 2;

/// H7 flash programs in 256-bit (32-byte) flashwords. Writes are padded to
/// this so the ROM never has to program a partial flashword.
pub const FLASHWORD: usize = 32;

/// DFU states (DFU 1.1 §6.1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DfuState {
    AppIdle,
    AppDetach,
    Idle,
    DnloadSync,
    DnBusy,
    DnloadIdle,
    ManifestSync,
    Manifest,
    ManifestWaitReset,
    UploadIdle,
    Error,
    Unknown(u8),
}

impl DfuState {
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::AppIdle,
            1 => Self::AppDetach,
            2 => Self::Idle,
            3 => Self::DnloadSync,
            4 => Self::DnBusy,
            5 => Self::DnloadIdle,
            6 => Self::ManifestSync,
            7 => Self::Manifest,
            8 => Self::ManifestWaitReset,
            9 => Self::UploadIdle,
            10 => Self::Error,
            n => Self::Unknown(n),
        }
    }
}

/// `bStatus` names (DFU 1.1 §6.1.2).
pub fn status_name(code: u8) -> &'static str {
    match code {
        0x00 => "OK",
        0x01 => "errTARGET (file not for this device)",
        0x02 => "errFILE",
        0x03 => "errWRITE (cannot write memory — read-protected?)",
        0x04 => "errERASE (memory erase failed)",
        0x05 => "errCHECK_ERASED",
        0x06 => "errPROG (program failed — target not erased?)",
        0x07 => "errVERIFY",
        0x08 => "errADDRESS (address out of range)",
        0x09 => "errNOTDONE",
        0x0A => "errFIRMWARE",
        0x0B => "errVENDOR",
        0x0C => "errUSBR",
        0x0D => "errPOR",
        0x0E => "errUNKNOWN",
        0x0F => "errSTALLEDPKT",
        _ => "unknown status",
    }
}

/// A `DFU_GETSTATUS` reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DfuStatus {
    pub status: u8,
    pub poll_timeout: Duration,
    pub state: DfuState,
}

impl DfuStatus {
    fn parse(bytes: &[u8]) -> Result<Self, DfuError> {
        if bytes.len() < 6 {
            return Err(DfuError::Usb(format!(
                "GETSTATUS reply is {} bytes, expected 6",
                bytes.len()
            )));
        }
        let poll_ms = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], 0]);
        Ok(Self {
            status: bytes[0],
            poll_timeout: Duration::from_millis(u64::from(poll_ms)),
            state: DfuState::from_byte(bytes[4]),
        })
    }
}

/// The two USB control transfers DFU needs. `wIndex` (the interface
/// number) is the transport's business.
pub trait DfuTransport {
    fn control_out(&mut self, request: u8, value: u16, data: &[u8]) -> Result<(), DfuError>;
    fn control_in(&mut self, request: u8, value: u16, length: u16) -> Result<Vec<u8>, DfuError>;
    /// `wTransferSize` from the DFU functional descriptor.
    fn transfer_size(&self) -> u16;
    /// Sleep between status polls (tests replace it with a no-op).
    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// Upper bound for one DNLOAD to settle. A mass erase of 1 MB on an H7
/// takes a few seconds; this only bounds a wedged device.
const BUSY_TIMEOUT: Duration = Duration::from_secs(60);

/// DfuSe operations over a transport.
pub struct DfuSe<'t, T: DfuTransport> {
    t: &'t mut T,
}

impl<'t, T: DfuTransport> DfuSe<'t, T> {
    pub fn new(t: &'t mut T) -> Self {
        Self { t }
    }

    pub fn get_status(&mut self) -> Result<DfuStatus, DfuError> {
        let bytes = self.t.control_in(DFU_GETSTATUS, 0, 6)?;
        DfuStatus::parse(&bytes)
    }

    fn clear_status(&mut self) -> Result<(), DfuError> {
        self.t.control_out(DFU_CLRSTATUS, 0, &[])
    }

    fn abort(&mut self) -> Result<(), DfuError> {
        self.t.control_out(DFU_ABORT, 0, &[])
    }

    /// Bring the device to `dfuIDLE` from whatever state a previous
    /// (interrupted) session left it in.
    pub fn reset_to_idle(&mut self) -> Result<(), DfuError> {
        let st = self.get_status()?;
        if st.state == DfuState::Error {
            self.clear_status()?;
        }
        if self.get_status()?.state != DfuState::Idle {
            self.abort()?;
        }
        let st = self.get_status()?;
        if st.state != DfuState::Idle {
            return Err(DfuError::Protocol {
                what: "reset to dfuIDLE".into(),
                status: status_name(st.status).into(),
                state: format!("{:?}", st.state),
            });
        }
        Ok(())
    }

    /// DNLOAD `data` as `block`, then poll GETSTATUS until the device
    /// leaves `dfuDNBUSY`. Any non-OK status is an error (after clearing
    /// it, so the next session starts clean).
    fn dnload_and_wait(&mut self, block: u16, data: &[u8], what: &str) -> Result<(), DfuError> {
        self.t.control_out(DFU_DNLOAD, block, data)?;
        let started = Instant::now();
        loop {
            let st = self.get_status()?;
            if st.status != 0 {
                let _ = self.clear_status();
                return Err(DfuError::Protocol {
                    what: what.to_string(),
                    status: status_name(st.status).into(),
                    state: format!("{:?}", st.state),
                });
            }
            match st.state {
                DfuState::DnBusy | DfuState::DnloadSync => {
                    if started.elapsed() > BUSY_TIMEOUT {
                        return Err(DfuError::Timeout(what.to_string()));
                    }
                    self.t.sleep(st.poll_timeout.max(Duration::from_millis(1)));
                }
                DfuState::DnloadIdle | DfuState::Idle => return Ok(()),
                other => {
                    return Err(DfuError::Protocol {
                        what: what.to_string(),
                        status: "OK".into(),
                        state: format!("unexpected {other:?}"),
                    })
                }
            }
        }
    }

    pub fn set_address(&mut self, addr: u32) -> Result<(), DfuError> {
        let mut cmd = vec![DFUSE_SET_ADDRESS];
        cmd.extend_from_slice(&addr.to_le_bytes());
        self.dnload_and_wait(0, &cmd, &format!("set address 0x{addr:08X}"))
    }

    /// Erase the whole internal flash (DfuSe erase with no address).
    pub fn mass_erase(&mut self) -> Result<(), DfuError> {
        self.dnload_and_wait(0, &[DFUSE_ERASE], "mass erase")
    }

    /// Write `data` at `addr`, one `wTransferSize` chunk at a time.
    /// `on_chunk` gets the byte count of each finished chunk.
    pub fn write(
        &mut self,
        addr: u32,
        data: &[u8],
        mut on_chunk: impl FnMut(u64),
    ) -> Result<(), DfuError> {
        let chunk = usize::from(self.t.transfer_size()).max(FLASHWORD);
        for (i, part) in data.chunks(chunk).enumerate() {
            let at = addr + (i * chunk) as u32;
            self.set_address(at)?;
            self.dnload_and_wait(FIRST_DATA_BLOCK, part, &format!("write at 0x{at:08X}"))?;
            on_chunk(part.len() as u64);
        }
        Ok(())
    }

    /// Read `len` bytes from `addr`.
    pub fn read(
        &mut self,
        addr: u32,
        len: usize,
        mut on_chunk: impl FnMut(u64),
    ) -> Result<Vec<u8>, DfuError> {
        let chunk = usize::from(self.t.transfer_size()).max(FLASHWORD);
        self.set_address(addr)?;
        // UPLOAD is only accepted from dfuIDLE.
        self.abort()?;
        let mut out = Vec::with_capacity(len);
        let mut block = FIRST_DATA_BLOCK;
        while out.len() < len {
            let want = (len - out.len()).min(chunk);
            let got = self.t.control_in(DFU_UPLOAD, block, want as u16)?;
            if got.is_empty() {
                return Err(DfuError::Usb(format!(
                    "UPLOAD returned no data at 0x{:08X}",
                    addr as usize + out.len()
                )));
            }
            on_chunk(got.len() as u64);
            out.extend_from_slice(&got[..got.len().min(want)]);
            block = block.wrapping_add(1);
        }
        self.abort()?;
        Ok(out)
    }

    /// Leave DFU and start the code at `addr`: point at it, then a
    /// zero-length DNLOAD and a GETSTATUS trigger manifestation. The
    /// device resets/jumps and may vanish mid-reply, so errors after the
    /// request is sent are expected and ignored.
    pub fn leave(&mut self, addr: u32) -> Result<(), DfuError> {
        self.set_address(addr)?;
        self.t.control_out(DFU_DNLOAD, FIRST_DATA_BLOCK, &[])?;
        let _ = self.get_status();
        Ok(())
    }
}

/// What to burn.
pub struct BurnPlan<'a> {
    pub image_base: u32,
    pub image: &'a [u8],
    /// `(node_id, seed flashword)` when provisioning.
    pub seed: Option<(u8, [u8; crate::provision_seed::SEED_LEN])>,
    pub verify: bool,
    pub leave: bool,
}

/// Erase → write → verify → seed → leave. Returns the CRC32 of the image.
pub fn burn<T: DfuTransport>(
    t: &mut T,
    layout: &MemoryLayout,
    plan: &BurnPlan<'_>,
    mut on_progress: impl FnMut(SwdProgress),
) -> Result<u32, DfuError> {
    use crate::provision_seed::{parse_seed_record, SEED_ADDR, SEED_LEN};

    if plan.image.is_empty() {
        return Err(DfuError::Artifact("the image is empty".into()));
    }
    // Pad to whole flashwords so the ROM never programs a partial one.
    let mut padded = plan.image.to_vec();
    padded.resize(plan.image.len().div_ceil(FLASHWORD) * FLASHWORD, 0xFF);
    if !layout.contains(u64::from(plan.image_base), padded.len() as u64) {
        return Err(DfuError::OutOfRange {
            base: plan.image_base,
            len: padded.len(),
            layout: format!(
                "{} 0x{:08X}..0x{:08X}",
                layout.name,
                layout.start,
                u64::from(layout.start) + layout.size()
            ),
        });
    }
    let crc = crate::firmware::crc32(plan.image);

    let mut dfu = DfuSe::new(t);
    dfu.reset_to_idle()?;

    on_progress(SwdProgress::Started {
        op: SwdOperation::Erase,
        total: None,
    });
    dfu.mass_erase()?;
    on_progress(SwdProgress::Finished {
        op: SwdOperation::Erase,
    });

    on_progress(SwdProgress::Started {
        op: SwdOperation::Program,
        total: Some(padded.len() as u64),
    });
    dfu.write(plan.image_base, &padded, |n| {
        on_progress(SwdProgress::Progress {
            op: SwdOperation::Program,
            delta: n,
        })
    })?;
    on_progress(SwdProgress::Finished {
        op: SwdOperation::Program,
    });

    if plan.verify {
        on_progress(SwdProgress::Started {
            op: SwdOperation::Verify,
            total: Some(plan.image.len() as u64),
        });
        let back = dfu.read(plan.image_base, plan.image.len(), |n| {
            on_progress(SwdProgress::Progress {
                op: SwdOperation::Verify,
                delta: n,
            })
        })?;
        let got = crate::firmware::crc32(&back);
        if got != crc {
            return Err(DfuError::VerifyMismatch {
                want: crc,
                got,
                base: plan.image_base,
                bytes: plan.image.len(),
            });
        }
        on_progress(SwdProgress::Finished {
            op: SwdOperation::Verify,
        });
    }

    if let Some((node_id, record)) = plan.seed {
        // Exactly one 32-byte DNLOAD at the seed address = exactly one
        // flashword programmed. Nothing else in that 1 KiB is touched, so
        // the app-metadata flashword at 0x080FFFE0 stays virgin for the
        // bootloader to write later (see swd::program_provision_seed).
        let addr = SEED_ADDR as u32;
        dfu.set_address(addr)?;
        dfu.dnload_and_wait(FIRST_DATA_BLOCK, &record, "write provisioning seed")?;
        let back = dfu.read(addr, SEED_LEN, |_| {})?;
        if parse_seed_record(&back) != Some(node_id) {
            return Err(DfuError::SeedReadbackMismatch { node_id });
        }
    }

    if plan.leave {
        dfu.leave(plan.image_base)?;
    }
    Ok(crc)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::provision_seed::{build_seed_record, parse_seed_record, SEED_ADDR, SEED_LEN};

    const BASE: u32 = 0x0800_0000;
    const SIZE: usize = 1024 * 1024;

    /// A simulated STM32 ROM DfuSe device with H7 flash rules: a byte can
    /// only be programmed from the erased state (0xFF), or it's errPROG.
    pub(crate) struct FakeDfu {
        pub flash: Vec<u8>,
        pointer: u32,
        state: DfuState,
        status: u8,
        pub transfer: u16,
        /// Every data write the device saw: (address, length).
        pub writes: Vec<(u32, usize)>,
        pub erased: bool,
        pub left_to: Option<u32>,
        /// Flip a bit in whatever lands at this address (simulated bad flash).
        pub corrupt_at: Option<u32>,
    }

    impl FakeDfu {
        pub(crate) fn new() -> Self {
            Self {
                flash: vec![0x00; SIZE], // NOT erased: a write without erase must fail
                pointer: BASE,
                state: DfuState::Idle,
                status: 0,
                transfer: 1024,
                writes: Vec::new(),
                erased: false,
                left_to: None,
                corrupt_at: None,
            }
        }
        fn fail(&mut self, status: u8) {
            self.status = status;
            self.state = DfuState::Error;
        }
    }

    impl DfuTransport for FakeDfu {
        fn transfer_size(&self) -> u16 {
            self.transfer
        }
        fn sleep(&mut self, _d: Duration) {}

        fn control_out(&mut self, request: u8, value: u16, data: &[u8]) -> Result<(), DfuError> {
            match request {
                DFU_CLRSTATUS => {
                    self.status = 0;
                    self.state = DfuState::Idle;
                }
                DFU_ABORT => self.state = DfuState::Idle,
                DFU_DNLOAD if value == 0 => match data {
                    [DFUSE_SET_ADDRESS, a, b, c, d] => {
                        self.pointer = u32::from_le_bytes([*a, *b, *c, *d]);
                        self.state = DfuState::DnBusy;
                    }
                    [DFUSE_ERASE] => {
                        self.flash.fill(0xFF);
                        self.erased = true;
                        self.state = DfuState::DnBusy;
                    }
                    _ => self.fail(0x0E),
                },
                DFU_DNLOAD if data.is_empty() => {
                    self.left_to = Some(self.pointer);
                    self.state = DfuState::Manifest;
                }
                DFU_DNLOAD => {
                    let addr = self.pointer + (u32::from(value) - 2) * u32::from(self.transfer);
                    let off = (addr - BASE) as usize;
                    if off + data.len() > SIZE {
                        self.fail(0x08);
                        return Ok(());
                    }
                    if self.flash[off..off + data.len()].iter().any(|b| *b != 0xFF) {
                        self.fail(0x06); // programming non-erased flash
                        return Ok(());
                    }
                    self.flash[off..off + data.len()].copy_from_slice(data);
                    if let Some(c) = self.corrupt_at {
                        if c >= addr && c < addr + data.len() as u32 {
                            self.flash[(c - BASE) as usize] ^= 0x01;
                        }
                    }
                    self.writes.push((addr, data.len()));
                    self.state = DfuState::DnBusy;
                }
                _ => self.fail(0x0E),
            }
            Ok(())
        }

        fn control_in(
            &mut self,
            request: u8,
            value: u16,
            length: u16,
        ) -> Result<Vec<u8>, DfuError> {
            match request {
                DFU_GETSTATUS => {
                    let reply = vec![self.status, 1, 0, 0, state_byte(self.state), 0];
                    if self.state == DfuState::DnBusy {
                        self.state = DfuState::DnloadIdle;
                    }
                    Ok(reply)
                }
                DFU_UPLOAD => {
                    assert_eq!(self.state, DfuState::Idle, "UPLOAD needs dfuIDLE");
                    let addr = self.pointer + (u32::from(value) - 2) * u32::from(self.transfer);
                    let off = (addr - BASE) as usize;
                    let end = (off + usize::from(length)).min(SIZE);
                    Ok(self.flash[off..end].to_vec())
                }
                _ => Err(DfuError::Usb(format!("unexpected IN request {request}"))),
            }
        }
    }

    fn state_byte(s: DfuState) -> u8 {
        match s {
            DfuState::Idle => 2,
            DfuState::DnBusy => 4,
            DfuState::DnloadIdle => 5,
            DfuState::Manifest => 7,
            DfuState::UploadIdle => 9,
            DfuState::Error => 10,
            _ => 0,
        }
    }

    fn layout() -> MemoryLayout {
        super::super::layout::parse("@Internal Flash  /0x08000000/8*128Kg").unwrap()
    }

    fn image(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + 3) as u8).collect()
    }

    #[test]
    fn burns_verifies_seeds_and_leaves() {
        let mut dev = FakeDfu::new();
        let img = image(54_712); // not a multiple of 32 — gets padded
        let seed = build_seed_record(0x2).unwrap();
        let mut events = Vec::new();
        let crc = burn(
            &mut dev,
            &layout(),
            &BurnPlan {
                image_base: BASE,
                image: &img,
                seed: Some((0x2, seed)),
                verify: true,
                leave: true,
            },
            |e| events.push(format!("{e:?}")),
        )
        .unwrap();
        assert_eq!(crc, crate::firmware::crc32(&img));
        assert!(dev.erased);
        assert_eq!(&dev.flash[..img.len()], &img[..]);
        assert!(dev.flash[img.len()..img.len().div_ceil(32) * 32]
            .iter()
            .all(|b| *b == 0xFF));
        let s = (SEED_ADDR as u32 - BASE) as usize;
        assert_eq!(parse_seed_record(&dev.flash[s..s + SEED_LEN]), Some(0x2));
        assert_eq!(dev.left_to, Some(BASE));
        assert!(events.iter().any(|e| e.contains("Verify")));
    }

    /// The seed must be one 32-byte write and nothing else may touch the
    /// seed's 1 KiB — that's what keeps the app-metadata flashword virgin.
    #[test]
    fn the_seed_is_exactly_one_flashword_write() {
        let mut dev = FakeDfu::new();
        let img = image(4096);
        burn(
            &mut dev,
            &layout(),
            &BurnPlan {
                image_base: BASE,
                image: &img,
                seed: Some((0x1, build_seed_record(0x1).unwrap())),
                verify: false,
                leave: false,
            },
            |_| {},
        )
        .unwrap();
        let top_kib = 0x080F_FC00..0x0810_0000u32;
        let in_top: Vec<_> = dev
            .writes
            .iter()
            .filter(|(a, _)| top_kib.contains(a))
            .collect();
        assert_eq!(in_top, vec![&(SEED_ADDR as u32, 32)]);
        let meta = (0x080F_FFE0 - BASE) as usize;
        assert!(
            dev.flash[meta..meta + 32].iter().all(|b| *b == 0xFF),
            "metadata word untouched"
        );
        assert!(
            dev.writes.iter().all(|(a, l)| a % 32 == 0 && l % 32 == 0),
            "whole flashwords only"
        );
    }

    #[test]
    fn a_bad_readback_is_caught() {
        let mut dev = FakeDfu::new();
        dev.corrupt_at = Some(BASE + 1500);
        let img = image(8192);
        let err = burn(
            &mut dev,
            &layout(),
            &BurnPlan {
                image_base: BASE,
                image: &img,
                seed: None,
                verify: true,
                leave: true,
            },
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, DfuError::VerifyMismatch { .. }), "{err}");
        assert_eq!(dev.left_to, None, "never starts a bad image");
    }

    #[test]
    fn device_errors_surface_with_their_name() {
        let mut dev = FakeDfu::new();
        let mut dfu = DfuSe::new(&mut dev);
        dfu.reset_to_idle().unwrap();
        // Writing without an erase hits non-erased flash → errPROG.
        let err = dfu.write(BASE, &[0u8; 64], |_| {}).unwrap_err();
        assert!(err.to_string().contains("errPROG"), "{err}");
        // …and the error was cleared for the next attempt.
        dfu.reset_to_idle().unwrap();
    }

    #[test]
    fn refuses_images_outside_the_flash() {
        let mut dev = FakeDfu::new();
        let err = burn(
            &mut dev,
            &layout(),
            &BurnPlan {
                image_base: 0x2000_0000,
                image: &image(64),
                seed: None,
                verify: false,
                leave: false,
            },
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, DfuError::OutOfRange { .. }));
        assert!(!dev.erased, "refused before touching the chip");
    }

    #[test]
    fn recovers_a_device_left_in_error() {
        let mut dev = FakeDfu::new();
        dev.state = DfuState::Error;
        dev.status = 0x03;
        DfuSe::new(&mut dev).reset_to_idle().unwrap();
        assert_eq!(dev.state, DfuState::Idle);
    }
}
