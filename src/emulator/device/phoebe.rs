//! A bank-switched memory module (designed for the Phoebe SBC).
//!
//! This device provides 56K RAM and 32K ROM. The ROM is designed to be bank-switched into a 16K
//! region in the address space (at `0xC000`). It divides the switchable region in half, and maps a
//! fixed 8K ROM bank into the upper half of the region. Using a single memory-mapped register, the
//! lower half of the region can be mapped to any one of three 8K banks. The lower half of the
//! region can also be unmapped, allowing the 8K RAM that shares the same region to be visible.
//!
//! The 8K ROM banks are numbered from 0 to 3, corresponding to their offsets in the 32K address
//! space of the physical ROM; Bank 0 = offset `0x0000`, Bank 1 = offset `0x2000`, Bank 2 = offset
//! `0x4000`, Bank 3 = offset `0x6000`.
//!
//! Bank 3 mapped to the upper half of the 16K switchable region, and cannot be switched out.
//! The program image in Bank 3 must include appropriate target addresses for the 6502 machine
//! vectors; NMI vector at `0x7FFA`, Reset vector at `0x7FFC`, IRQ vector at `0x7FFE`.
//!
//! ## Control Register
//! An 8-bit control register is used to select the 8K ROM bank that is mapped into the
//! address space at `0xC000..0xDFFF`. On the Phoebe SBC, this register is mapped by the
//! hardware to address `0xFFF7`, but the emulation allows it to be mapped into any address.
//!
//! Only the two low-order bits of the register are significant -- the remaining bits are ignored
//! on write and always read as zero. The register is initialized to zero at system reset. When
//! no bank is selected (by setting both selection bits high), the 8K RAM that resides in the same
//! region becomes visible.
//!
//! | Bit 1 | Bit 0 | Selection       |
//! |-------|-------|-----------------|
//! |   0   |   0   | ROM Bank 0      |
//! |   0   |   1   | ROM Bank 1      |
//! |   1   |   0   | ROM Bank 2      |
//! |   1   |   1   | RAM             |
//!
//!
use crate::emulator::AddressRange;
use crate::emulator::bus::RomWritePolicy;
use crate::emulator::device::{DeviceId, ErrorSender, IoDevice};
use log::debug;

const NUM_BANKS: u8 = 4;
const BANK_SIZE: usize = 8192;
const SELECTION_MASK: u8 = 0b00000011;

const ROM_START: u16 = 0xC000;
const ROM_END: u16 = 0xFFFF;

const WINDOW_START: u16 = ROM_START;
const WINDOW_END: u16 = WINDOW_START + (BANK_SIZE - 1) as u16;

/// Total size of ROM in the module
pub const ROM_SIZE: usize = NUM_BANKS as usize * BANK_SIZE;
/// Total size of RAM in the module
pub const RAM_SIZE: usize = 64 * 1024 - BANK_SIZE;

/// A bank-switched ROM (designed for the Phoebe SBC).
pub struct Phoebe {
    /// Name of the device as it appears in configuration and CLI.
    name: &'static str,
    /// Address to which the bank selection register is mapped.
    control_register_address: u16,
    /// Write policy to apply for attempted write operations
    write_policy: Option<RomWritePolicy>,
    /// Destination for error events.
    error_sender: Option<ErrorSender>,
    /// Device identity for error events.
    device_id: Option<DeviceId>,
    /// Address region that corresponds to ROM
    rom_range: AddressRange,
    /// Address region that corresponds to the bank-switching window
    window_range: AddressRange,
    /// Contents of the bank selection register (only bits 1..0 are significant)
    selected_bank: u8,
    /// ROM storage
    rom_data: Vec<u8>,
    /// RAM storage
    ram_data: Vec<u8>,
}

impl Phoebe {

    /// Constructs a new `Phoebe` device mapped to the given region and register address.
    pub fn new(name: &'static str,
               control_register_address: u16) -> Self {
        Self {
            name,
            control_register_address,
            write_policy: None,
            error_sender: None,
            device_id: None,
            rom_range: AddressRange::new(ROM_START, ROM_END),
            window_range: AddressRange::new(WINDOW_START, WINDOW_END),
            selected_bank: 0,
            rom_data: Vec::new(),
            ram_data: Vec::new(),
        }
    }

    /// Constructs a new `Vireo` device whose memory is loaded with the given data.
    ///
    /// ## Arguments
    /// - `control_register_address` - address for the control register
    /// - `rom_data` - data to load into ROM; panics if the length of `rom_data` is not 32K
    /// - `ram_data` - data to load into RAM; panics if the length of `ram_data` is not 128K
    ///
    pub fn with_data(
        name: &'static str,
        control_register_address: u16,
        rom_data: Vec<u8>,
        ram_data: Vec<u8>) -> Self {
        assert_eq!(rom_data.len(), ROM_SIZE,
                   "ROM data size {} does not match ROM size {}", rom_data.len(), ROM_SIZE);
        assert_eq!(ram_data.len(), RAM_SIZE,
                   "RAM data size {} does not match RAM size {}", ram_data.len(), RAM_SIZE);
        let mut device = Self::new(name, control_register_address);
        device.rom_data = rom_data;
        device.ram_data = ram_data;
        device
    }

    /// Sets the write policy.
    pub fn set_write_policy(&mut self, write_policy: RomWritePolicy) {
        self.write_policy = Some(write_policy);
    }

    /// Sets the error sender for event reporting.
    pub fn set_error_sender(&mut self, sender: ErrorSender, id: DeviceId) {
        self.error_sender = Some(sender);
        self.device_id = Some(id);
    }

    fn report_rejected_write(&self, address: u16) {
        if let (Some(sender), Some(id)) = (&self.error_sender, self.device_id) {
            use crate::emulator::device::DeviceEvent;
            let _ = sender.send(DeviceEvent::RejectedWrite { device: id, address });
        }
    }

    fn control_register(&self) -> u8 {
        self.selected_bank
    }

    fn set_control_register(&mut self, value: u8) {
        self.selected_bank = value & SELECTION_MASK;
    }

    fn effective_address(&self, address: u16) -> (bool, usize) {
        if self.rom_range.contains(address) {
            if self.window_range.contains(address) {
                if self.selected_bank < NUM_BANKS - 1 {
                    let offset = BANK_SIZE * self.selected_bank as usize;
                    let effective_address = offset + (address - WINDOW_START) as usize;
                    (true, effective_address)
                } else {
                    (false, address as usize)
                }
            } else {
                let offset = (NUM_BANKS - 1) as usize * BANK_SIZE;
                let effective_address = offset + (address as usize - (ROM_START as usize + BANK_SIZE));
                (true, effective_address)
            }
        } else {
            (false, address as usize)
        }
    }

}

impl IoDevice for Phoebe {

    fn read(&mut self, address: u16) -> u8 {
        self.peek(address)
    }

    fn write(&mut self, address: u16, value: u8) {
        if address == self.control_register_address {
            self.set_control_register(value);
        } else {
            let (is_rom, effective_address) = self.effective_address(address);
            if !is_rom {
                self.ram_data[effective_address] = value;
            } else if let Some(write_policy) = self.write_policy {
                match write_policy {
                    RomWritePolicy::Ignore => (),
                    RomWritePolicy::Error => self.report_rejected_write(address),
                }
            }
        }
    }

    fn peek(&self, address: u16) -> u8 {
        if address == self.control_register_address {
            self.control_register()
        } else {
            let (is_rom, effective_address) = self.effective_address(address);
            if is_rom {
                self.rom_data[effective_address]
            } else {
                self.ram_data[effective_address]
            }
        }
    }

    fn claims(&self, _address: u16) -> bool {
        true
    }

    fn reset(&mut self) {
        self.set_control_register(0);
        debug!("{} @0x{:04x} reset", self.name(), self.control_register_address);
    }

    fn name(&self) -> &str { self.name }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::DeviceEvent;
    use tokio::sync::mpsc;

    const DEVICE_NAME: &str = "phoebe";
    const CTRL_REGISTER_ADDRESS: u16 = 0xFFF7;

    fn device() -> Phoebe {
        let rom_data: Vec<u8> = vec![0xFF; ROM_SIZE];
        let ram_data: Vec<u8> = vec![0; RAM_SIZE];
        Phoebe::with_data(DEVICE_NAME, CTRL_REGISTER_ADDRESS, rom_data, ram_data)
    }

    #[test]
    fn effective_address_ram() {
        let device = device();
        assert_eq!(device.effective_address(0x0000), (false, 0x0000));
        assert_eq!(device.effective_address(0xBFFF), (false, 0xBFFF));
    }

    #[test]
    fn effective_address_window() {
        let mut device = device();
        device.selected_bank = 0;
        assert_eq!(device.effective_address(0xC000), (true, 0x0000));
        assert_eq!(device.effective_address(0xDFFF), (true, 0x1FFF));
        device.selected_bank = 1;
        assert_eq!(device.effective_address(0xC000), (true, 0x2000));
        assert_eq!(device.effective_address(0xDFFF), (true, 0x3FFF));
        device.selected_bank = 2;
        assert_eq!(device.effective_address(0xC000), (true, 0x4000));
        assert_eq!(device.effective_address(0xDFFF), (true, 0x5FFF));
        device.selected_bank = 3;
        assert_eq!(device.effective_address(0xC000), (false, 0xC000));
        assert_eq!(device.effective_address(0xDFFF), (false, 0xDFFF));
    }

    #[test]
    fn effective_address_rom() {
        let device = device();
        assert_eq!(device.effective_address(0xE000), (true, 0x6000));
        assert_eq!(device.effective_address(0xFFFF), (true, 0x7FFF));
    }

    #[test]
    fn peek_control_register() {
        let mut device = device();
        device.set_control_register(SELECTION_MASK);
        assert_eq!(device.peek(CTRL_REGISTER_ADDRESS), SELECTION_MASK);
    }

    #[test]
    fn peek_ram() {
        let mut device = device();
        device.ram_data[0] = 0x55;
        assert_eq!(device.peek(0), 0x55);
    }

    #[test]
    fn peek_window() {
        let mut device = device();
        device.selected_bank = 1;
        device.rom_data[0x2000] = 0x55;
        assert_eq!(device.peek(0xC000), 0x55);
    }

    #[test]
    fn peek_rom() {
        let mut device = device();
        device.rom_data[0x6000] = 0x55;
        assert_eq!(device.peek(0xE000), 0x55);
    }

    #[test]
    fn write_ram() {
        let mut device = device();
        device.selected_bank = SELECTION_MASK;
        device.write(0x0000, 0xFF);
        assert_eq!(device.ram_data[0x0000], 0xFF);
        device.write(0xBFFF, 0xFF);
        assert_eq!(device.ram_data[0xBFFF], 0xFF);
    }

    #[test]
    fn write_rom_ignored_when_policy_is_none() {
        let mut device = device();
        device.write(0xFFFF, 0);
        assert_eq!(device.rom_data[0x7FFF], 0xFF);
    }

    #[test]
    fn write_rom_ignored_when_policy_is_ignore() {
        let mut device = device();
        device.set_write_policy(RomWritePolicy::Ignore);
        device.write(0xFFFF, 0);
        assert_eq!(device.rom_data[0x7FFF], 0xFF);
    }

    #[tokio::test]
    async fn write_rom_reports_rejected_write_when_policy_is_error() {
        let device_id = DeviceId(0);
        let mut device = device();
        let (tx, mut rx) = mpsc::unbounded_channel::<DeviceEvent>();
        device.set_error_sender(tx, device_id);
        device.set_write_policy(RomWritePolicy::Error);

        device.write(0xFFFF, 0);

        match rx.try_recv() {
            Ok(event) => {
                assert!(matches!(event, DeviceEvent::RejectedWrite{ address: 0xFFFF, .. }));
            }
            Err(e) => panic!("Expected a DeviceEvent, but channel was empty: {:?}", e),
        }
    }

    #[test]
    fn reset_restores_default_bank() {
        let mut device = device();
        device.set_control_register(SELECTION_MASK);
        device.reset();
        assert_eq!(device.control_register(), 0);
    }

}
