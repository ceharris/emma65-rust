//! A bank-switched ROM (designed for the Finch SBC).
//!
//! This device combines 512K RAM and 512K ROM and a very basic memory management unit (MMU) that
//! allows the entire 64K address space for the 6502 to be mapped to any combination of 4K segments
//! of the module's available memory.
//!
//! The module's 1024K memory space requires a 20-bit address. The 6502 bus has a 16-bit
//! address. The full address needed by the memory module is supplied by using the four most
//! significant bits from the 6502's address (`A12..A15`) as an index into an array of sixteen
//! 8-bit bank registers in the MMU. The full effective address used to access memory consists
//! of the 12 least significant bits from the 6502 address bus (bits `A0..A11`) concatenated with
//! the 8-bit bank address (`B0..B7`) stored in the register addressed by `A12..A15`.
//!
//! ```ignore
//!                                     6 5 0 2   A d d r e s s   B u s
//!                     A15 A14 A13 A12 A11 A10  A9  A8  A7  A6  A5  A4  A3  A2  A1  A0
//!                       │   │   │   │   │   │   │   │   │   │   │   │   │   │   │   │
//!   ┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓  │   │   │   │   │   │   │   │   │   │   │   │
//!   ┃   MMU Bank Registers (0..15)   ┃  │   │   │   │   │   │   │   │   │   │   │   │
//!   ┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛  │   │   │   │   │   │   │   │   │   │   │   │
//!       │   │   │   │   │   │   │   │   │   │   │   │   │   │   │   │   │   │   │   │
//!      B7  B6  B5  B4  B3  B2  B1  B0   │   │   │   │   │   │   │   │   │   │   │   │
//!       │   │   │   │   │   │   │   │   │   │   │   │   │   │   │   │   │   │   │   │
//!     M19 M18 M17 M16 M15 M14 M13 M12 M11 M10  M9  M8  M7  M6  M5  M4  M3  M2  M1  M0
//!                       E f f e c t i v e   M e m o r y   A d d r e s s
//! ```
//!
//! The 64K address space of the 6502 can be viewed as consisting of 16 4K bank slots, at `0x0000`,
//! `0x1000`, `0x2000`, ..., `0xF000`. The MMU's bank registers determine which of the 256 4K banks
//! of physical memory will be mapped into each of those slots. Any memory bank (RAM or ROM) can be
//! mapped into any slot under program control, by simply writing the bank number (`0x00..0xFF`)
//! into the corresponding bank register (`0x0..0xF`).
//!
//! The module's memory is evenly divided with banks `0x00` through `0x7F` composed of RAM, while
//! banks `0x80` through `0xFF` are composed of EEPROM. The Finch SBC uses a ZIF socket for the
//! 32-pin EEPROM package to facilitate programming.
//!
//! ## MMU Bank Registers
//! The sixteen 8-bit bank registers are mapped into a contiguous region of the 6502's address space
//! at an address that is paragraph aligned (i.e. the address is evenly divisible by 16). On the
//! Finch SBC, the bank registers are hard-mapped into the address space at `0xFC00`. This emulation
//! allows them to be mapped at an arbitrary address. The bank registers support both read and
//! write, so the 6502 program is not required to keep a shadow copy in RAM.
//!
//! At reset, the MMU bank registers are ignored and a simple fixed memory model is selected. In
//! this mode, the MMU maps the lowest 32K of physical RAM into the lower half of the 6502 address
//! space, while mapping the lowest 32K of physical ROM into the upper half of the address space.
//! This fixed memory model maps slots `0x0..0x7` to banks `0x00..0x07` (RAM), while slots
//! `0x8..0xF` are mapped to banks `0x80..0x87`. This provides a reasonably organized complement of
//! RAM and ROM by default, and ensures that at system startup the 6502 RESET vector (at `0xFFFC`)
//! will be fetched from physical ROM (at `0x87FFC`) for system initialization.
//!
//! After the system is operating under program control, the MMU bank registers can be configured as
//! desired and subsequently enabled using the MMU control register, as described below.
//!
//! ## MMU Control Register
//! In addition to the bank registers, the Finch memory module provides an 8-bit control register
//! that is used to enable or disable the MMU. When disabled, the memory module operates in the
//! fixed mode described above under [MMU Bank Registers](#mmu-bank-registers). When enabled, the
//! MMU bank registers are used to form the full 20-bit address needed to access the module's
//! memory space.
//!
//! The high-order bit (`D7`) is assigned as the MMU enable (`MMUE`) signal. When register bit
//! `D7` is low, the MMU is disabled and the system's memory uses the fixed mapping. When `D7`
//! is high, the MMU bank registers determine the mapping of the system address space. Obviously,
//! it is important to carefully configure the bank registers before enabling the MMU.
//!
//! On the Finch SBC, the MMU Control Register is mapped into the address space at `0xFFD8`. In
//! this emulation, the control register may be mapped at an arbitrary address.
//!
//! Also note that on the Finch SBC, the MMU Control Register is a more general purpose
//! configuration register, with several bit positions assigned to other system configuration
//! functions. As such, when modifying the MMUE bit, the programmer should first read the register,
//! modify the MMUE bit in the accumulator, then write the result back to the register. For
//! example, to set the MMUE bit (assuming that the register is mapped at `0xFFD8`):
//!
//! ```ignore
//!         LDA $FFD8       ; fetch the config register state
//!         ORA #$80        ; set the high order bit (MMUE)
//!         STA $FFD8       ; store the new config register state
//! ```
//!
use crate::emulator::AddressRange;
use crate::emulator::bus::RomWritePolicy;
use crate::emulator::device::{DeviceId, ErrorSender, IoDevice};
use log::debug;

const NUM_SLOTS: usize = 16;
const SLOT_BITS: usize = 4;
const NUM_BANKS: usize = 256;

const ADDRESS_MASK: usize = 0xfff;

const BANK_SIZE: usize = 4096;

pub const MEMORY_SIZE: usize = BANK_SIZE * NUM_BANKS;
pub const ROM_START: usize = MEMORY_SIZE / 2;

const MMUE_MASK: u8 = 0b10000000;


/// A bank-switched memory module with a simple MMU (designed for the Finch SBC).
pub struct Finch {
    /// Name of the device as it appears in configuration and CLI.
    name: &'static str,
    /// Base address of the bank registers
    bank_register_range: AddressRange,
    /// Address for the control register
    control_register_address: u16,
    /// Write policy to apply for attempted write operations on ROM
    write_policy: Option<RomWritePolicy>,
    /// Destination for error events.
    error_sender: Option<ErrorSender>,
    /// Device identity for error events.
    device_id: Option<DeviceId>,
    /// Bank selection registers
    bank_registers: [u8; NUM_SLOTS],
    /// Control register image
    control_register: u8,
    /// Memory storage
    data: Vec<u8>,
}

impl Finch {

    /// Constructs a new `Finch` device.
    ///
    /// ## Arguments
    /// - `bank_register_address` - base address for the sixteen bank registers
    /// - `control_register_address` - address for the control register
    ///
    pub fn new(name: &'static str, bank_register_address: u16, control_register_address: u16) -> Self {
        Self {
            name,
            bank_register_range: AddressRange::new(
                bank_register_address,
                bank_register_address + (NUM_SLOTS - 1) as u16),
            control_register_address,
            write_policy: None,
            error_sender: None,
            device_id: None,
            bank_registers: [0; NUM_SLOTS],
            control_register: 0,
            data: Vec::new(),
        }
    }

    /// Constructs a new `Finch` device whose memory is loaded with the given data.
    ///
    /// ## Arguments
    /// - `bank_register_address` - base address for the sixteen bank registers
    /// - `control_register_address` - address for the control register
    /// - `data` - data to load into memory; panics if the length of `data` is not equal
    ///   to the size of memory (1024K)
    ///
    pub fn with_data(
            name: &'static str,
            bank_register_address: u16,
            control_register_address: u16,
            data: Vec<u8>) -> Self {
        assert_eq!(data.len(), BANK_SIZE * NUM_BANKS,
                   "data size {} does not match ROM size {}", data.len(), BANK_SIZE * NUM_BANKS);
        let mut device = Finch::new(name, bank_register_address, control_register_address);
        device.data = data;
        device
    }

    /// Sets the ROM write policy.
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

    fn mmu_enabled(&self) -> bool {
        self.control_register & MMUE_MASK != 0
    }

    fn effective_address(&self, address: u16) -> usize {
        let slot = (address >> (16 - SLOT_BITS)) as usize;
        let bank = if self.mmu_enabled() {
            self.bank_registers[slot] as usize
        } else if slot < NUM_SLOTS / 2 {
            slot
        } else {
            (slot & 0x7) | 0x80
        };
        (address as usize & ADDRESS_MASK) | (bank << (16 - SLOT_BITS))
    }

}

impl IoDevice for Finch {

    fn read(&mut self, address: u16) -> u8 {
        self.peek(address)
    }

    fn write(&mut self, address: u16, value: u8) {
        if address == self.control_register_address {
            self.control_register = value
        } else if self.bank_register_range.contains(address) {
            self.bank_registers[(address - self.bank_register_range.start) as usize] = value;
        } else {
            let effective_address = self.effective_address(address);
            if effective_address < ROM_START {
                self.data[effective_address] = value;
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
            self.control_register
        } else if self.bank_register_range.contains(address) {
            self.bank_registers[(address - self.bank_register_range.start) as usize]
        } else {
            self.data[self.effective_address(address)]
        }
    }

    fn claims(&self, _address: u16) -> bool {
        true
    }

    fn reset(&mut self) {
        self.control_register &= !MMUE_MASK;
        debug!("{} @0x{:04x} reset", self.name(), self.control_register);
    }

    fn name(&self) -> &str { self.name }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::DeviceEvent;
    use tokio::sync::mpsc;

    const DEVICE_NAME: &str = "finch";
    const BANK_REGISTER_BASE: u16 = 0xFFC0;
    const CTRL_REGISTER_ADDRESS: u16 = 0xFFD8;

    fn device() -> Finch {
        let data: Vec<u8> = vec![0; BANK_SIZE*NUM_BANKS];
        Finch::with_data(DEVICE_NAME, BANK_REGISTER_BASE, CTRL_REGISTER_ADDRESS, data)
    }

    fn device_with_mmu_configured() -> Finch {
        let mut device = device();
        for (i, bank_register) in device.bank_registers.iter_mut().enumerate() {
            // mapping: 0x0 = 0x78, 0x1 = 0x79, ..., 0x7 = 0x7F, 0x8 = 0x80, ..., 0xF = 0x87
            *bank_register = (i + 128 - 8) as u8;
        }
        device.control_register = MMUE_MASK;
        device
    }

    #[test]
    fn effective_address_mmu_disabled() {
        let device = device();
        assert_eq!(device.effective_address(0), 0);
        assert_eq!(device.effective_address(0x1000), 0x1000);
        assert_eq!(device.effective_address(0x7FFF), 0x7FFF);
        assert_eq!(device.effective_address(0x8000), 0x80000);
        assert_eq!(device.effective_address(0x9000), 0x81000);
        assert_eq!(device.effective_address(0xFFFF), 0x87FFF);
    }

    #[test]
    fn effective_address_mmu_enabled() {
        let device = device_with_mmu_configured();
        assert_eq!(device.effective_address(0x0000), 0x78000);
        assert_eq!(device.effective_address(0x1000), 0x79000);
        assert_eq!(device.effective_address(0x7FFF), 0x7FFFF);
        assert_eq!(device.effective_address(0x8000), 0x80000);
        assert_eq!(device.effective_address(0x9000), 0x81000);
        assert_eq!(device.effective_address(0xFFFF), 0x87FFF);
    }

    #[test]
    fn peek_control_register() {
        let device = device_with_mmu_configured();
        assert_eq!(device.peek(CTRL_REGISTER_ADDRESS), MMUE_MASK);
    }

    #[test]
    fn peek_bank_registers() {
        let device = device_with_mmu_configured();
        for i in 0..NUM_SLOTS {
            let address = BANK_REGISTER_BASE + i as u16;
            assert_eq!(device.peek(address), (i + 128 - 8) as u8);
        }
    }

    #[test]
    fn peek_memory() {
        let mut device = device_with_mmu_configured();
        for i in 0..NUM_SLOTS {
            let bank =  i + 128 - 8;
            let address = bank << (16 - SLOT_BITS);
            device.data[address] = i as u8;
        }
        for i in 0..NUM_SLOTS {
            let address = (i << (16 - SLOT_BITS)) as u16;
            assert_eq!(device.peek(address), i as u8);
        }
    }

    #[test]
    fn reset_disables_mmu() {
        let mut device = device_with_mmu_configured();
        assert_ne!(device.control_register & MMUE_MASK, 0);
        device.reset();
        assert_eq!(device.control_register & MMUE_MASK, 0);
    }

    #[test]
    fn write_control_register() {
        let mut device = device();
        assert_eq!(device.peek(CTRL_REGISTER_ADDRESS), 0);
        device.write(CTRL_REGISTER_ADDRESS, 0xFF);
        assert_eq!(device.peek(CTRL_REGISTER_ADDRESS), 0xFF);
    }

    #[test]
    fn write_bank_registers() {
        let mut device = device();
        for i in 0..NUM_SLOTS {
            let address = BANK_REGISTER_BASE + i as u16;
            assert_eq!(device.peek(address), 0);
            device.write(address, i as u8);
            assert_eq!(device.peek(address), i as u8);
        }
    }

    #[test]
    fn write_ram() {
        let mut device = device();
        device.write(0x01FF, 0xFF);
        assert_eq!(device.data[0x01FF], 0xFF);
    }

    #[test]
    fn write_rom_ignored_when_policy_is_none() {
        let mut device = device();
        device.write(0xFFFF, 0);
    }

    #[test]
    fn write_rom_ignored_when_policy_is_ignore() {
        let mut device = device();
        device.set_write_policy(RomWritePolicy::Ignore);
        device.write(0xFFFF, 0);
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

}
