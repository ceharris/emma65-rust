use log::debug;
use crate::emulator::AddressRange;
use crate::emulator::bus::RomWritePolicy;
use crate::emulator::device::{DeviceId, ErrorSender, IoDevice};

const NUM_BANKS: u8 = 4;
const BANK_SIZE: u16 = 8192;
const SELECTION_MASK: u8 = 0b00000011;

/// Size of the address space region occupied by the ROM
pub const REGION_SIZE: u16 = 2 * BANK_SIZE;
/// Total size of ROM in the device
pub const MEMORY_SIZE: u16 = NUM_BANKS as u16 * BANK_SIZE;

/// A bank-switched ROM (designed for the Phoebe SBC).
/// This device provides a 32K ROM designed to be bank-switched into a 16K region in the address
/// space (typically at 0xC000). It divides the region in half and maps a fixed 8K bank into the
/// upper half of the region. Using a single memory-mapped register, the lower half of the
/// region can be mapped to any one of three 8K banks. The lower half of the region can also be
/// unmapped, allowing additional RAM or RAM in the same region to be selectively shadowed by the
/// bank-switched ROM.
///
/// The 8K banks are numbered from 0 to 3, corresponding to their offsets in the 32K space of the
/// ROM; Bank 0 = offset 0, Bank 1 = offset 0x2000, Bank 2 = offset 0x4000, Bank 3 = offset 0x6000.
///
/// Bank 3 is always mapped to the upper half of the region. Assuming the typical use in which the
/// target address region for the device is 0xC000..0xFFFF, the image loaded into bank 3 must
/// include appropriate target addresses for the 6502 machine vectors; offset 0x7FFA = NMI vector,
/// 0x7FFC = reset vector, 0x7FFE = IRQ vector.
///
/// ## Bank Selection Register
/// The bank selection register can be mapped into the region of the address space that is used
/// for I/O devices. Only the two low-order bits of the register are significant -- the remaining
/// bits are ignored. When none of banks 0 to 2 is selected, the lower half of the region is
/// unmapped. The selection register is initialized to zero at system reset.
///
/// | Bit 1 | Bit 0 | Bank selected |
/// |-------|-------|---------------|
/// |   0   |   0   | Bank 0        |
/// |   0   |   1   | Bank 1        |
/// |   1   |   0   | Bank 2        |
/// |   1   |   1   | none          |
///
pub struct Phoebe {
    /// Device name used in configuration
    name: &'static str,
    /// 16K address region mapped to the ROM; typically 0xC000..0xFFFF.
    rom_region: AddressRange,
    /// Address to which the bank selection register is mapped.
    register_address: u16,
    /// Write policy to apply for attempted write operations
    write_policy: Option<RomWritePolicy>,
    /// Destination for error events.
    error_sender: Option<ErrorSender>,
    /// Device identity for error events.
    device_id: Option<DeviceId>,
    /// Contents of the bank selection register (only bits 1..0 are significant)
    selected_bank: u8,
    /// Memory storage
    data: Vec<u8>,
}

impl Phoebe {

    /// Constructs a new `RomPhoebe` device mapped to the given region and register address.
    pub fn new(name: &'static str, rom_region: AddressRange, register_address: u16) -> Self {
        Self {
            name,
            rom_region,
            register_address,
            write_policy: None,
            error_sender: None,
            device_id: None,
            selected_bank: 0,
            data: Vec::new(),
        }
    }

    /// Constructs a new `RomPhoebe` device mapped to the given region and loaded with the
    /// specified data. Panics if the length of `data` is not equal to the fixed size of the ROM.
    pub fn with_data(name: &'static str, rom_region: AddressRange, register_address: u16, data: Vec<u8>) -> Self {
        assert_eq!(data.len(), (NUM_BANKS as u16 * BANK_SIZE) as usize,
                   "data size {} does not match ROM size {}", data.len(), (NUM_BANKS as u16 * BANK_SIZE) as usize);
        let mut device = Phoebe::new(name, rom_region, register_address);
        device.data = data;
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

}

impl IoDevice for Phoebe {

    #[allow(dead_code)]
    fn base_address(&self) -> u16 {
       self.rom_region.start
    }

    fn read_relative(&mut self, _offset: u16) -> u8 {
        unreachable!("all reads are handled in `read_absolute()`")
    }

    fn write_relative(&mut self, _offset: u16, _value: u8) {
        unreachable!("all writes are handled in `write_absolute()`")
    }

    fn peek_relative(&self, offset: u16) -> u8 {
        if offset < BANK_SIZE {
            // NOTE: If `selected_bank == NUM_BANKS - 1`, `claims()` will return false for any
            // address in the lower half of the region, in which case the bus won't call on this
            // device, so this code isn't executed.
            let effective_addr = (BANK_SIZE * (self.selected_bank as u16) + offset) as usize;
            self.data[effective_addr]
        } else {
            let effective_addr = offset.wrapping_add((NUM_BANKS - 2) as u16 * BANK_SIZE) as usize;
            self.data[effective_addr]
        }
    }

    fn read_absolute(&mut self, address: u16) -> u8 {
        self.peek_absolute(address)
    }

    fn write_absolute(&mut self, address: u16, value: u8) {
        if address == self.register_address {
            self.selected_bank = value & SELECTION_MASK;
        } else if let Some(write_policy) = self.write_policy {
            match write_policy {
                RomWritePolicy::Ignore => (),
                RomWritePolicy::Error => self.report_rejected_write(address),
            }
        }
    }

    fn peek_absolute(&self, address: u16) -> u8 {
        if address == self.register_address {
            self.selected_bank
        } else {
            self.peek_relative(address - self.rom_region.start)
        }
    }

    fn claims(&self, address: u16) -> bool {
        address == self.register_address
            || (address - self.rom_region.start) >= BANK_SIZE
            || self.selected_bank as u16 != (NUM_BANKS - 1) as u16
    }

    fn reset(&mut self) {
        self.selected_bank = 0;
        debug!("{} {} reset", self.name(), self.device_id.unwrap())
    }

    fn name(&self) -> &str { self.name }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::{AddressRange, IoDevice, RomWritePolicy};
    use crate::emulator::device::phoebe::Phoebe;
    use crate::emulator::device::DeviceEvent;
    use tokio::sync::mpsc;

    const START_ADDR: u16 = 0xC000;
    const END_ADDR: u16 = 0xFFFF;
    const REGISTER_ADDR: u16 = 0xFFF7;

    fn rom_device() -> Phoebe {
        Phoebe::new("phoebe", AddressRange::new(START_ADDR, END_ADDR), REGISTER_ADDR)
    }

    #[test]
    #[should_panic]
    fn read_absolute_panics_when_no_data() {
        let mut device = rom_device();
        device.read_absolute(START_ADDR);
    }

    #[test]
    #[should_panic]
    fn peek_absolute_panics_when_no_data() {
        let device = rom_device();
        device.peek_relative(0);
    }

    #[test]
    fn write_absolute_ignored_when_policy_is_none() {
        let mut device = rom_device();
        device.write_absolute(START_ADDR, 0);
    }

    #[test]
    fn write_absolute_ignored_when_policy_is_ignore() {
        let mut device = rom_device();
        device.set_write_policy(RomWritePolicy::Ignore);
        device.write_absolute(START_ADDR, 0);
    }

    #[tokio::test]
    async fn write_absolute_reports_rejected_write_when_policy_is_error() {
        let device_id = DeviceId(0);
        let mut device = rom_device();
        let (tx, mut rx) = mpsc::unbounded_channel::<DeviceEvent>();
        device.set_error_sender(tx, device_id);
        device.set_write_policy(RomWritePolicy::Error);

        device.write_absolute(START_ADDR, 0);

        match rx.try_recv() {
            Ok(event) => {
                assert!(matches!(event, DeviceEvent::RejectedWrite{ address: START_ADDR, .. }));
            }
            Err(e) => panic!("Expected a DeviceEvent, but channel was empty: {:?}", e),
        }
    }

    #[test]
    fn claims_upper_half_always() {
        let device = rom_device();
        assert!(device.claims(START_ADDR + BANK_SIZE), "expected to claim start of upper half");
        assert!(device.claims(START_ADDR - 1 + 2*BANK_SIZE), "expected to claim end of upper half");
    }

    #[test]
    fn claims_lower_half_when_top_bank_selected_never() {
        let mut device = rom_device();
        device.selected_bank = NUM_BANKS - 1;
        assert!(!device.claims(START_ADDR), "expected not to claim start of lower half");
        assert!(!device.claims(START_ADDR -1 + BANK_SIZE), "expected not to claim end of lower half");
    }

    #[test]
    fn claims_lower_half_when_other_bank_selected_always() {
        let mut device = rom_device();
        device.selected_bank = 2;
        assert!(device.claims(START_ADDR), "expected to claim start of lower half");
        assert!(device.claims(START_ADDR -1 + BANK_SIZE), "expected to claim end of lower half");
    }

    #[test]
    fn claims_bank_select_register_always() {
        let device = rom_device();
        assert!(device.claims(REGISTER_ADDR), "expected to claim register address");
    }

    #[test]
    fn peek_absolute_retrieves_expected_data() {
        let mut data: Vec<u8> = vec![0; (NUM_BANKS as u16 * BANK_SIZE) as usize];
        for bank in 1..=3 {
            let start_addr = bank * (BANK_SIZE as usize);
            let end_addr = (bank + 1) * (BANK_SIZE as usize) - 1;
            data[start_addr] = bank as u8;
            data[end_addr] = bank as u8;
        }
        let mut device = Phoebe::with_data("phoebe", AddressRange::new(START_ADDR, END_ADDR), REGISTER_ADDR, data);
        for bank in 0..=3 {
            device.selected_bank = bank;
            assert_eq!(device.peek_absolute(START_ADDR), bank);
            assert_eq!(device.peek_absolute(START_ADDR + BANK_SIZE - 1), bank);
        }
        assert_eq!(device.peek_absolute(START_ADDR.wrapping_add(BANK_SIZE)), NUM_BANKS - 1);
        assert_eq!(device.peek_absolute(START_ADDR.wrapping_add(2*BANK_SIZE).wrapping_sub(1)), NUM_BANKS - 1);
    }

    #[test]
    fn reset_selected_bank() {
        let mut device = rom_device();
        device.selected_bank = 1;
        device.reset();
        assert_eq!(device.selected_bank, 0, "expected selected bank reset");
    }

}
