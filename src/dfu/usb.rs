//! The real USB side of DFU, on `nusb`: find STM32 ROM bootloaders in DFU
//! mode, read their flash layout, and carry the class requests.

use std::num::NonZeroU8;
use std::time::Duration;

use nusb::descriptors::ConfigurationDescriptor;
use nusb::transfer::{ControlIn, ControlOut, ControlType, Recipient};
use nusb::MaybeFuture;

use super::layout::{self, MemoryLayout};
use super::protocol::DfuTransport;
use super::{DfuError, STM32_DFU_PID, STM32_DFU_VID};

const USB_CLASS_APP_SPECIFIC: u8 = 0xFE;
const DFU_SUBCLASS: u8 = 0x01;
const DFU_FUNCTIONAL_DESCRIPTOR: u8 = 0x21;
const LANG_EN_US: u16 = 0x0409;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
/// What the ST ROM bootloader reports; used if the descriptor is missing.
const FALLBACK_TRANSFER_SIZE: u16 = 1024;

/// One STM32 found in DFU mode.
#[derive(Debug, Clone)]
pub struct DfuDeviceInfo {
    pub serial: Option<String>,
    pub product: Option<String>,
    /// ROM bootloader version (`bcdDevice`).
    pub rom_version: u16,
    /// The "Internal Flash" region, when the device could be opened and its
    /// alt-setting names read. `None` with `open_error` set otherwise (e.g.
    /// no WinUSB driver on Windows).
    pub flash: Option<MemoryLayout>,
    pub open_error: Option<String>,
}

/// List every STM32 in DFU mode. Opening a device to read its layout can
/// fail (driver missing, busy); that's reported per device, not fatally.
pub fn list_devices() -> Result<Vec<DfuDeviceInfo>, DfuError> {
    let devices = nusb::list_devices()
        .wait()
        .map_err(|e| DfuError::Usb(format!("list USB devices: {e}")))?;
    let mut out = Vec::new();
    for info in
        devices.filter(|d| d.vendor_id() == STM32_DFU_VID && d.product_id() == STM32_DFU_PID)
    {
        let (flash, open_error) = match info.open().wait() {
            Ok(dev) => match find_dfu(&dev) {
                Ok(found) => (Some(found.layout), None),
                Err(e) => (None, Some(e.to_string())),
            },
            Err(e) => (None, Some(format!("open: {e}"))),
        };
        out.push(DfuDeviceInfo {
            serial: info.serial_number().map(str::to_string),
            product: info.product_string().map(str::to_string),
            rom_version: info.device_version(),
            flash,
            open_error,
        });
    }
    Ok(out)
}

struct FoundDfu {
    interface: u8,
    alt: u8,
    layout: MemoryLayout,
    transfer_size: u16,
}

/// Find the DFU interface, its "Internal Flash" alt setting and the
/// functional descriptor's `wTransferSize`.
fn find_dfu(dev: &nusb::Device) -> Result<FoundDfu, DfuError> {
    let config: ConfigurationDescriptor<'_> = dev
        .active_configuration()
        .map_err(|e| DfuError::Usb(format!("read configuration: {e}")))?;
    let transfer_size = config
        .descriptors()
        .find(|d| d.descriptor_type() == DFU_FUNCTIONAL_DESCRIPTOR && d.len() >= 7)
        .map(|d| u16::from_le_bytes([d[5], d[6]]))
        .filter(|n| *n > 0)
        .unwrap_or(FALLBACK_TRANSFER_SIZE);
    for alt in config.interface_alt_settings() {
        if alt.class() != USB_CLASS_APP_SPECIFIC || alt.subclass() != DFU_SUBCLASS {
            continue;
        }
        let Some(idx) = alt.string_index() else {
            continue;
        };
        let name = read_string(dev, idx)?;
        if let Some(layout) = layout::parse(&name) {
            if layout.name.eq_ignore_ascii_case("Internal Flash") {
                return Ok(FoundDfu {
                    interface: alt.interface_number(),
                    alt: alt.alternate_setting(),
                    layout,
                    transfer_size,
                });
            }
        }
    }
    Err(DfuError::NoFlashLayout)
}

fn read_string(dev: &nusb::Device, idx: NonZeroU8) -> Result<String, DfuError> {
    dev.get_string_descriptor(idx, LANG_EN_US, CONTROL_TIMEOUT)
        .wait()
        .map_err(|e| DfuError::Usb(format!("read string descriptor {idx}: {e}")))
}

/// An opened, claimed DFU interface.
pub struct UsbDfu {
    interface: nusb::Interface,
    number: u8,
    layout: MemoryLayout,
    transfer_size: u16,
    rom_version: u16,
    serial: Option<String>,
}

impl UsbDfu {
    pub fn layout(&self) -> &MemoryLayout {
        &self.layout
    }
    pub fn rom_version(&self) -> u16 {
        self.rom_version
    }
    pub fn serial(&self) -> Option<&str> {
        self.serial.as_deref()
    }
}

/// Open the one STM32 in DFU mode (or the one with `serial`).
pub fn open(serial: Option<&str>) -> Result<UsbDfu, DfuError> {
    let candidates: Vec<_> = nusb::list_devices()
        .wait()
        .map_err(|e| DfuError::Usb(format!("list USB devices: {e}")))?
        .filter(|d| d.vendor_id() == STM32_DFU_VID && d.product_id() == STM32_DFU_PID)
        .collect();
    let describe = |d: &nusb::DeviceInfo| d.serial_number().unwrap_or("(no serial)").to_string();
    let info = match (candidates.len(), serial) {
        (0, _) => return Err(DfuError::NoDevice),
        (_, Some(want)) => candidates
            .iter()
            .find(|d| d.serial_number() == Some(want))
            .cloned()
            .ok_or_else(|| DfuError::SerialNotFound {
                requested: want.to_string(),
                available: candidates
                    .iter()
                    .map(describe)
                    .collect::<Vec<_>>()
                    .join(", "),
            })?,
        (1, None) => candidates[0].clone(),
        (_, None) => {
            return Err(DfuError::AmbiguousDevice(
                candidates
                    .iter()
                    .map(describe)
                    .collect::<Vec<_>>()
                    .join(", "),
            ))
        }
    };
    let dev = info
        .open()
        .wait()
        .map_err(|e| DfuError::Usb(format!("open DFU device: {e}")))?;
    let found = find_dfu(&dev)?;
    let interface = dev
        .detach_and_claim_interface(found.interface)
        .wait()
        .map_err(|e| DfuError::Usb(format!("claim DFU interface: {e}")))?;
    interface
        .set_alt_setting(found.alt)
        .wait()
        .map_err(|e| DfuError::Usb(format!("select the Internal Flash alt setting: {e}")))?;
    Ok(UsbDfu {
        interface,
        number: found.interface,
        layout: found.layout,
        transfer_size: found.transfer_size,
        rom_version: info.device_version(),
        serial: info.serial_number().map(str::to_string),
    })
}

impl DfuTransport for UsbDfu {
    fn control_out(&mut self, request: u8, value: u16, data: &[u8]) -> Result<(), DfuError> {
        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Class,
                    recipient: Recipient::Interface,
                    request,
                    value,
                    index: u16::from(self.number),
                    data,
                },
                CONTROL_TIMEOUT,
            )
            .wait()
            .map_err(|e| DfuError::Usb(format!("DFU request {request}: {e}")))
    }

    fn control_in(&mut self, request: u8, value: u16, length: u16) -> Result<Vec<u8>, DfuError> {
        self.interface
            .control_in(
                ControlIn {
                    control_type: ControlType::Class,
                    recipient: Recipient::Interface,
                    request,
                    value,
                    index: u16::from(self.number),
                    length,
                },
                CONTROL_TIMEOUT,
            )
            .wait()
            .map_err(|e| DfuError::Usb(format!("DFU request {request}: {e}")))
    }

    fn transfer_size(&self) -> u16 {
        self.transfer_size
    }
}
