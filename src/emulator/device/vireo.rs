//! A bank-switched memory module (designed for the Vireo SBC).
//!
//! This device combines 128K RAM and 32K ROM with an elegant bank switching mechanism to
//! allow sophisticated program loading strategies and/or program use of RAM beyond the limits
//! of the 64K address space.
//!
//! The module supports four different configurations of the available memory.
//!
//! - **Mode 0**
//!   - `0x0000..0x7FFF` mapped to RAM `0x00000..0x07FFF`
//!   - `0x8000..0xFFFF` mapped to ROM
//! - **Mode 1**
//!   - `0x0000..0x7FFF` mapped to RAM `0x10000..0x17FFF`
//!   - `0x8000..0xFFFF` mapped to ROM
//! - **Mode 2**
//!   - `0x0000..0xFFFF` mapped to RAM `0x00000..0x0FFFF`
//! - **Mode 3**
//!   - `0x0000..0xFFFF` mapped to RAM `0x10000..0x1FFFF`
//!
//! In all modes, the 8K region of the address space at `0xC000..0xDFFF` can be mapped to any
//! 8K segment of the unmapped region of RAM, with 4K alignment. This allows the unmapped region
//! of RAM to be read or written under program control, by manipulating the appropriate control
//! register to address the desired 8K segment.
//!
//! ## Control Register
//! An 8-bit control register is used to set the mode, enable or inhibit the 8K window at `0xC000`,
//! and to select the 8K segment of unmapped RAM to be mapped into the window. Writing to the
//! register sets the configuration, while the current configuration may be obtained by reading
//! the register. On the Vireo SBC, this register is mapped by the hardware to address `0xFFF4`.
//!
//! The fields of the control register are as follows. Note that bit 7 is ignored on write and
//! always reads as zero.
//!
//! ```ignore
//!     ┌────┬────┬────────┬────────────────┐
//!     │ -- │ WI │ M1  M0 │ S3  S2  S1  S0 │
//!     └────┴────┴────────┴────────────────┘
//! ```
//!
//! - *WI* - Window Inhibit - setting this bit to 1 disables the window at 0xC000, such that the
//!   memory it shadows becomes visible/addressable.
//! - *Mx* - Mode - two bits that select a mode 0..3
//! - *Sx* - Segment - four bits that specify the segment of unmapped RAM to map into the window;
//!   see discussion below.
//!
//! ## Segment Selection
//! The control register's Segment field is a 4-bit field that specifies a segment number in the
//! range `0x0..0xF`. The complement of mode bit 0 (`M0`) and the 4-bit Segment field are
//! concatenated as the five most significant bits of the base address for the mapped segment
//! within the RAM. For example, in Modes 0 or 2, setting the Segment field to `0x9` maps the 8K
//! segment of RAM at physical address 0x19000 into the window at `0xC000`. Similarly, in Modes 1
//! or 3, setting the Segment field to `0xE` maps the 8K segment of RAM at physical address 0x0E000`
//! into the window.
//!
//! If the Segment field is set to `0xF`, the upper half of the 8K segment wraps around within
//! the unmapped region of the physical RAM. For example Modes 0 or 2, setting Segment to 0xF,
//! effectively maps 4K at `0x1F000` into the window at `0xC000`, and 4K at `0x10000` into the
//! window at `0xD000`.
//!
use crate::emulator::AddressRange;
use crate::emulator::bus::RomWritePolicy;
use crate::emulator::device::{DeviceId, ErrorSender, IoDevice};
use log::debug;

pub const RAM_SIZE: usize = 128*1024;

pub const ROM_START: u16 = 0x8000;
const ROM_END: u16 = 0xFFFF;
pub const ROM_SIZE: usize = ROM_END as usize + 1 - ROM_START as usize;

const WINDOW_START: u16 = 0xC000;
const WINDOW_END: u16 = 0xDFFF;

const CTRL_WINDOW_INHIBIT: u8 = 0b01000000;
const CTRL_RAM_ONLY: u8 = 0b00100000;
const CTRL_MAP_UPPER: u8 = 0b00010000;
const CTRL_SEGMENT_MASK: u8 = 0b00001111;


/// A bank-switched memory module with a simple MMU (designed for the Finch SBC).
pub struct Vireo {
    /// Name of the device as it appears in configuration and CLI.
    name: &'static str,
    control_register_address: u16,
    /// Write policy to apply for attempted write operations on ROM
    write_policy: Option<RomWritePolicy>,
    /// Destination for error events.
    error_sender: Option<ErrorSender>,
    /// Device identity for error events.
    device_id: Option<DeviceId>,
    /// Range of addresses that optionally map to ROM
    rom_range: AddressRange,
    /// Range of addresses that optionally map to a segment in unmapped RAM
    window_range: AddressRange,
    /// Window inhibit flag
    window_inhibit: bool,
    /// M1 - when true, 64K RAM is mapped; otherwise, 32K RAM and 32K ROM are mapped
    ram_only: bool,
    /// M0 - when true, the upper half of the 128K RAM is mapped; otherwise the lower half is mapped
    map_upper: bool,
    /// Segment to map (0..15)
    segment: u8,
    // ROM storage,
    rom_data: Vec<u8>,
    /// RAM storage
    ram_data: Vec<u8>,
}

impl Vireo {

    /// Constructs a new `Vireo` device.
    ///
    /// ## Arguments
    /// - `control_register_address` - address for the control register
    ///
    pub fn new(name: &'static str, control_register_address: u16) -> Self {
        Self {
            name,
            control_register_address,
            write_policy: None,
            error_sender: None,
            device_id: None,
            rom_range: AddressRange::new(ROM_START, ROM_END),
            window_range: AddressRange::new(WINDOW_START, WINDOW_END),
            window_inhibit: true,
            ram_only: false,
            map_upper: false,
            segment: 0,
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
        let mut device = Vireo::new(name, control_register_address);
        device.rom_data = rom_data;
        device.ram_data = ram_data;
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

    fn control_register(&self) -> u8 {
        (if self.window_inhibit { CTRL_WINDOW_INHIBIT } else { 0 })
            | (if self.ram_only { CTRL_RAM_ONLY } else { 0 })
            | (if self.map_upper { CTRL_MAP_UPPER } else { 0 })
            | (self.segment & CTRL_SEGMENT_MASK)
    }

    fn set_control_register(&mut self, value: u8) {
        self.segment = value & CTRL_SEGMENT_MASK;
        self.ram_only = value & CTRL_RAM_ONLY != 0;
        self.map_upper = value & CTRL_MAP_UPPER != 0;
        self.window_inhibit = value & CTRL_WINDOW_INHIBIT != 0;
    }

    fn effective_address(&self, address: u16) -> (bool, usize) {
        if !self.window_inhibit && self.window_range.contains(address) {
            let high_bit = if self.map_upper { 0 } else { 0x10000 };
            let segment_addr = (self.segment as u16) << 12;
            let offset = address - WINDOW_START;
            let addr = high_bit | segment_addr.wrapping_add(offset) as usize;
            (false, addr)
        } else if !self.ram_only && self.rom_range.contains(address) {
            (true, (address - ROM_START) as usize)
        } else {
            let high_bit = if self.map_upper { 0x10000 } else { 0 };
            let addr = high_bit | address as usize;
            (false, addr)
        }
    }

}

impl IoDevice for Vireo {

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
        self.set_control_register(CTRL_WINDOW_INHIBIT);
        debug!("{} @0x{:04x} reset", self.name(), self.control_register_address);
    }

    fn name(&self) -> &str { self.name }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::DeviceEvent;
    use tokio::sync::mpsc;

    const DEVICE_NAME: &str = "vireo";
    const CTRL_REGISTER_ADDRESS: u16 = 0xFFD8;

    fn device() -> Vireo {
        let rom_data: Vec<u8> = vec![0xFF; ROM_SIZE];
        let ram_data: Vec<u8> = vec![0; RAM_SIZE];
        Vireo::with_data(DEVICE_NAME, CTRL_REGISTER_ADDRESS, rom_data, ram_data)
    }

    #[test]
    fn effective_address_lower_half_when_lower_mapped() {
        let device = device();
        assert_eq!(device.effective_address(0x0000), (false, 0x00000));
        assert_eq!(device.effective_address(0x7FFF), (false, 0x07FFF));
    }

    #[test]
    fn effective_address_lower_half_when_upper_mapped() {
        let mut device = device();
        device.map_upper = true;
        assert_eq!(device.effective_address(0x0000), (false, 0x10000));
        assert_eq!(device.effective_address(0x7FFF), (false, 0x17FFF));
    }

    #[test]
    fn effective_address_upper_half_when_ram_only_and_lower_mapped() {
        let mut device = device();
        device.ram_only = true;
        assert_eq!(device.effective_address(0x8000), (false, 0x08000));
        assert_eq!(device.effective_address(0xFFFF), (false, 0x0FFFF));
    }

    #[test]
    fn effective_address_lower_half_when_ram_only_and_upper_mapped() {
        let mut device = device();
        device.map_upper = true;
        device.ram_only = true;
        assert_eq!(device.effective_address(0x8000), (false, 0x18000));
        assert_eq!(device.effective_address(0xFFFF), (false, 0x1FFFF));
    }

    #[test]
    fn effective_address_upper_half_when_rom_mapped() {
        let device = device();
        assert_eq!(device.effective_address(0x8000), (true, 0x0000));
        assert_eq!(device.effective_address(0xFFFF), (true, 0x7FFF));
    }

    #[test]
    fn effective_address_in_window_when_window_enabled_and_lower_mapped() {
        let mut device = device();
        device.segment = 0x9;
        device.window_inhibit = false;
        assert_eq!(device.effective_address(0xC000), (false, 0x19000));
        assert_eq!(device.effective_address(0xDFFF), (false, 0x1AFFF));
    }

    #[test]
    fn effective_address_in_window_when_window_enabled_and_upper_mapped() {
        let mut device = device();
        device.map_upper = true;
        device.segment = 0x9;
        device.window_inhibit = false;
        assert_eq!(device.effective_address(0xC000), (false, 0x09000));
        assert_eq!(device.effective_address(0xDFFF), (false, 0x0AFFF));
    }

    #[test]
    fn effective_address_in_window_when_window_inhibited_and_rom_mapped() {
        let mut device = device();
        device.window_inhibit = true;
        device.segment = 0x9;
        assert_eq!(device.effective_address(0xC000), (true, 0x04000));
        assert_eq!(device.effective_address(0xDFFF), (true, 0x05FFF));
    }

    #[test]
    fn effective_address_in_window_when_window_inhibited_ram_only_and_lower_mapped() {
        let mut device = device();
        device.window_inhibit = true;
        device.ram_only = true;
        device.segment = 0x9;
        assert_eq!(device.effective_address(0xC000), (false, 0x0C000));
        assert_eq!(device.effective_address(0xDFFF), (false, 0x0DFFF));
    }

    #[test]
    fn effective_address_in_window_when_window_inhibited_ram_only_and_upper_mapped() {
        let mut device = device();
        device.window_inhibit = true;
        device.ram_only = true;
        device.map_upper = true;
        device.segment = 0x9;
        assert_eq!(device.effective_address(0xC000), (false, 0x1C000));
        assert_eq!(device.effective_address(0xDFFF), (false, 0x1DFFF));
    }

    #[test]
    fn control_register_set_segment() {
        let mut device = device();
        for i in 0..16 {
            device.set_control_register(i);
            assert_eq!(device.segment, i);
        }
    }

    #[test]
    fn control_register_set_mode() {
        let mut device = device();
        device.set_control_register(0);
        assert!(!device.map_upper);
        assert!(!device.ram_only);
        device.set_control_register(1 << 4);
        assert!(device.map_upper);
        assert!(!device.ram_only);
        device.set_control_register(2 << 4);
        assert!(!device.map_upper);
        assert!(device.ram_only);
        device.set_control_register(3 << 4);
        assert!(device.map_upper);
        assert!(device.ram_only);
    }

    #[test]
    fn control_register_set_window_inhibit() {
        let mut device = device();
        device.set_control_register(CTRL_WINDOW_INHIBIT);
        assert!(device.window_inhibit);
    }

    #[test]
    fn control_register_set_all_fields() {
        let mut device = device();
        device.set_control_register(0xFF);
        assert!(device.window_inhibit);
        assert!(device.ram_only);
        assert!(device.map_upper);
        assert_eq!(device.segment, 0xF);
    }

    #[test]
    fn control_register_round_trip() {
        let mut device = device();
        device.set_control_register(0x55);
        assert_eq!(device.control_register(), 0x55);
    }

    #[test]
    fn peek_control_register() {
        let mut device = device();
        device.set_control_register(0x55);
        assert_eq!(device.peek(CTRL_REGISTER_ADDRESS), 0x55);
    }

    #[test]
    fn peek_window() {
        let mut device = device();
        device.window_inhibit = false;
        device.map_upper = false;
        device.segment = 0x9;
        device.ram_data[0x19000] = 0x99;
        assert_eq!(device.peek(WINDOW_START), 0x99);
    }

    #[test]
    fn peek_rom() {
        let mut device = device();
        device.map_upper = false;
        device.ram_only = false;
        device.rom_data[0] = 0x88;
        assert_eq!(device.peek(ROM_START), 0x88);
    }

    #[test]
    fn peek_rom_window_inhibited() {
        let mut device = device();
        device.map_upper = false;
        device.ram_only = false;
        device.window_inhibit = true;
        device.rom_data[(WINDOW_START - ROM_START) as usize] = 0xCC;
        assert_eq!(device.peek(WINDOW_START), 0xCC);
    }

    #[test]
    fn peek_ram_lower_half_when_lower_mapped() {
        let mut device = device();
        device.ram_data[0x00000] = 0xFF;
        assert_eq!(device.peek(0x0000), 0xFF);
    }

    #[test]
    fn peek_ram_lower_half_when_upper_mapped() {
        let mut device = device();
        device.map_upper = true;
        device.ram_data[0x10000] = 0xFF;
        assert_eq!(device.peek(0x0000), 0xFF);
    }

    #[test]
    fn peek_ram_lower_half_when_ram_only_and_lower_mapped() {
        let mut device = device();
        device.ram_only = true;
        device.ram_data[0x0FFFF] = 0xFF;
        assert_eq!(device.peek(0xFFFF), 0xFF);
    }

    #[test]
    fn peek_ram_lower_half_when_ram_only_and_upper_mapped() {
        let mut device = device();
        device.map_upper = true;
        device.ram_only = true;
        device.ram_data[0x1FFFF] = 0xFF;
        assert_eq!(device.peek(0xFFFF), 0xFF);
    }

    #[test]
    fn write_ram_lower_half_when_lower_mapped() {
        let mut device = device();
        device.write(0, 0xFF);
        assert_eq!(device.ram_data[0x00000], 0xFF);
    }

    #[test]
    fn write_ram_lower_half_when_upper_mapped() {
        let mut device = device();
        device.map_upper = true;
        device.write(0, 0xFF);
        assert_eq!(device.ram_data[0x10000], 0xFF);
    }

    #[test]
    fn write_ram_upper_half_when_ram_only_and_lower_mapped() {
        let mut device = device();
        device.ram_only = true;
        device.write(0x8000, 0xFF);
        assert_eq!(device.ram_data[0x08000], 0xFF);
    }

    #[test]
    fn write_ram_upper_half_when_ram_only_and_upper_mapped() {
        let mut device = device();
        device.map_upper = true;
        device.ram_only = true;
        device.write(0x8000, 0xFF);
        assert_eq!(device.ram_data[0x18000], 0xFF);
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
    fn reset_restores_default_config() {
        let mut device = device();
        device.window_inhibit = false;
        device.ram_only = true;
        device.map_upper = true;
        device.segment = 0xF;
        device.reset();
        assert!(device.window_inhibit);
        assert!(!device.ram_only);
        assert!(!device.map_upper);
        assert_eq!(device.segment, 0);
    }

}
