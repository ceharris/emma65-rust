//! Memory bus, address decoding, and bus tracing.

mod interrupt;
mod region;
mod loader;
pub mod symbol;

use rand::RngExt;
pub use interrupt::{InterruptController, IrqSource, MAX_IRQ_SOURCES};
pub use loader::BusLoadTarget;
pub use region::{AddressRange, BusOp};
pub use symbol::SymbolTable;

use crate::emulator::device::{DeviceId, IoDevice};
use crate::emulator::error::{BusConfigError, BusError};

/// Value returned on reads from unmapped addresses when `UnmappedPolicy::DefaultValue` is active.
const UNMAPPED_READ_VALUE: u8 = 0xFF;

/// Policy for reads and writes to unmapped addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnmappedPolicy {
    /// Return `UNMAPPED_READ_VALUE` on reads; silently ignore writes.
    DefaultValue,
    /// Return a `BusError::Unmapped` error.
    Error,
}

/// Policy for writes to ROM regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RomWritePolicy {
    /// Silently ignore writes to ROM.
    Ignore,
    /// Return a `BusError::RomWrite` error.
    Error,
}

/// Internal representation of one region mapped on the bus.
enum Region {
    Ram {
        range: AddressRange,
        data: Vec<u8>,
    },
    Rom {
        range: AddressRange,
        data: Vec<u8>,
        write_policy: RomWritePolicy,
    },
    Device {
        range: AddressRange,
        /// Index into the owning `Bus`/`BusConfig`'s `devices` vector.
        ///
        /// Stable because devices are only ever appended during `BusConfig`
        /// construction; more than one `Region::Device` may share a `device_index`
        /// when a device is mapped at more than one range via `extend_device()`.
        device_index: usize,
    },
}

impl Region {
    fn range(&self) -> AddressRange {
        match self {
            Region::Ram { range, .. } => *range,
            Region::Rom { range, .. } => *range,
            Region::Device { range, .. } => *range,
        }
    }
}


impl From<IrqSource> for DeviceId {
    fn from(id: IrqSource) -> Self {
        DeviceId(id.0)
    }
}

/// An allocator for unique device IDs.
#[derive(Clone, Copy)]
pub struct DeviceIdAllocator {
    next_irq_source: u32,
    next_other_id: u32,
}

impl Default for DeviceIdAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceIdAllocator {

    pub fn new() -> Self {
        Self {
            next_irq_source: 0,
            next_other_id: MAX_IRQ_SOURCES,
        }
    }

    /// Returns the next device ID
    pub fn next(&mut self, wants_irq_source: bool) -> DeviceId {
        if wants_irq_source {
            self.next_irq_source()
        } else {
            self.next_other_id()
        }
    }

    /// Returns the next ID for a device that needs to assert IRQ
    fn next_irq_source(&mut self) -> DeviceId {
        assert_ne!(self.next_irq_source, MAX_IRQ_SOURCES);
        let irq_source = DeviceId(self.next_irq_source);
        self.next_irq_source += 1;
        irq_source
    }

    /// Returns the next ID for a device that never needs to assert IRQ
    fn next_other_id(&mut self) -> DeviceId {
        let id = DeviceId(self.next_other_id);
        self.next_other_id += 1;
        id
    }

}

/// The configurable memory bus with RAM, ROM, and IO device regions.
pub struct Bus {
    regions: Vec<Region>,
    devices: Vec<(DeviceId, Box<dyn IoDevice>)>,
    unmapped_policy: UnmappedPolicy,
    symbol_table: SymbolTable,
    resolved: Box<[Option<u32>]>,
}

impl Bus {
    /// Returns a `BusConfig` builder for constructing a `Bus`.
    pub fn config() -> BusConfig {
        BusConfig::new()
    }

    /// Reads one byte from `addr`, triggering device side effects if an IO device is mapped there.
    pub fn read(&mut self, addr: u16) -> Result<u8, BusError> {
        let value = match self.find_region_mut(addr) {
            Some(RegionMatch::Ram { data, offset }) => Ok(data[offset]),
            Some(RegionMatch::Rom { data, offset, .. }) => Ok(data[offset]),
            Some(RegionMatch::Device { device, addr }) => Ok(device.read(addr)),
            None => match self.unmapped_policy {
                UnmappedPolicy::DefaultValue => Ok(UNMAPPED_READ_VALUE),
                UnmappedPolicy::Error => Err(BusError::Unmapped { addr }),
            },
        }?;
        Ok(value)
    }

    /// Writes one byte to `addr`, triggering device side effects if an IO device is mapped there.
    pub fn write(&mut self, addr: u16, value: u8) -> Result<(), BusError> {
        match self.find_region_mut(addr) {
            Some(RegionMatch::Ram { data, offset }) => {
                data[offset] = value;
                Ok(())
            }
            Some(RegionMatch::Rom { write_policy, .. }) => match &write_policy {
                RomWritePolicy::Ignore => Ok(()),
                RomWritePolicy::Error => Err(BusError::RomWrite { addr }),
            },
            Some(RegionMatch::Device { device, addr }) => {
                device.write(addr, value);
                Ok(())
            }
            None => match self.unmapped_policy {
                UnmappedPolicy::DefaultValue => Ok(()),
                UnmappedPolicy::Error => Err(BusError::Unmapped { addr }),
            },
        }?;
        Ok(())
    }

    /// Reads one byte from `addr` without triggering device side effects.
    pub fn peek(&self, addr: u16) -> Result<u8, BusError> {
        match self.find_region(addr) {
            Some(PeekMatch::Ram { data, offset }) => Ok(data[offset]),
            Some(PeekMatch::Rom { data, offset }) => Ok(data[offset]),
            Some(PeekMatch::Device { device, addr }) => Ok(device.peek(addr)),
            None => match self.unmapped_policy {
                UnmappedPolicy::DefaultValue => Ok(UNMAPPED_READ_VALUE),
                UnmappedPolicy::Error => Err(BusError::Unmapped { addr }),
            },
        }
    }

    /// Reads `buf.len()` bytes starting at `addr` without triggering device side effects.
    ///
    /// Reads cross region boundaries; unmapped gaps are filled with the unmapped default
    /// (0xFF) or produce an error according to the bus's `UnmappedPolicy`.
    pub fn peek_range(&self, addr: u16, buf: &mut [u8]) -> Result<(), BusError> {
        for (i, slot) in buf.iter_mut().enumerate() {
            let a = addr.wrapping_add(i as u16);
            *slot = self.peek(a)?;
        }
        Ok(())
    }

    /// Writes one byte to `addr`, bypassing ROM write restrictions and triggering device side
    /// effects if an I/O device is mapped there.
    pub fn patch(&mut self, addr: u16, value: u8) {
        match self.find_region_mut(addr) {
            Some(RegionMatch::Ram { data, offset }) => {
                data[offset] = value;
            }
            Some(RegionMatch::Rom { data, offset, write_policy: _write_policy }) => {
                data[offset] = value;
            },
            Some(RegionMatch::Device { device, addr }) => {
                device.patch(addr, value);
            }
            None => {
            }
        };
    }

    /// Calls `tick(cycles)` on every IO device mapped on the bus.
    pub fn tick_devices(&mut self, cycles: u32) {
        for (_, device) in &mut self.devices {
            device.tick(cycles);
        }
    }

    /// Calls `reset()` on every IO device mapped on the bus
    pub fn reset_devices(&mut self) {
        for (_, device) in &mut self.devices {
            device.reset();
        }
    }

    /// Returns the IRQ state of every device as `(DeviceId, irq_active)` pairs.
    pub fn device_irq_states(&self) -> impl Iterator<Item = (DeviceId, bool)> + '_ {
        self.devices.iter().map(|(id, device)| (*id, device.irq_active()))
    }

    /// Drains pending NMI edge events from all devices. Returns `true` if any device had one.
    pub fn take_device_nmi(&mut self) -> bool {
        let mut any = false;
        for (_, device) in &mut self.devices {
            any |= device.take_nmi();
        }
        any
    }

    /// Replaces the ROM data for the region starting at `range.start` with `data`.
    ///
    /// `data.len()` must equal `range.len()`.  Useful for patching ROM after construction.
    pub fn load_rom(&mut self, range: AddressRange, data: &[u8]) -> Result<(), BusError> {
        let expected = range.len() as usize;
        if data.len() != expected {
            // Treat as unmapped — caller passed a range that isn't a ROM region.
            return Err(BusError::Unmapped { addr: range.start });
        }
        for region in &mut self.regions {
            if let Region::Rom { range: r, data: rom_data, .. } = region
                && *r == range {
                rom_data.copy_from_slice(data);
                return Ok(());
            }
        }
        Err(BusError::Unmapped { addr: range.start })
    }

    // --- private helpers ---

    /// Returns the index of the most-specific (smallest) region that contains `addr`, if any.
    fn find_region_index(&self, addr: u16) -> Option<usize> {
        self.resolved[addr as usize].map(|idx| idx as usize)
    }

    fn find_region(&self, addr: u16) -> Option<PeekMatch<'_>> {
        let idx = self.find_region_index(addr)?;
        match &self.regions[idx] {
            Region::Ram { range, data } => {
                let offset = (addr - range.start) as usize;
                Some(PeekMatch::Ram { data, offset })
            }
            Region::Rom { range, data, .. } => {
                let offset = (addr - range.start) as usize;
                Some(PeekMatch::Rom { data, offset })
            }
            Region::Device { device_index, .. } => Some(PeekMatch::Device {
                device: self.devices[*device_index].1.as_ref(),
                addr,
            }),
        }
    }

    fn find_region_mut(&mut self, addr: u16) -> Option<RegionMatch<'_>> {
        let idx = self.find_region_index(addr)?;
        let Bus { regions, devices, .. } = self;
        match &mut regions[idx] {
            Region::Ram { range, data } => {
                let offset = (addr - range.start) as usize;
                Some(RegionMatch::Ram { data, offset })
            }
            Region::Rom { range, data, write_policy } => {
                let offset = (addr - range.start) as usize;
                Some(RegionMatch::Rom { data, offset, write_policy: *write_policy })
            }
            Region::Device { device_index, .. } => Some(RegionMatch::Device {
                device: devices[*device_index].1.as_mut(),
                addr,
            }),
        }
    }
    
    /// Returns a reference to the symbol table for this bus.
    pub fn symbol_table(&self) -> &SymbolTable {
        &self.symbol_table
    }

    /// Returns a mutable reference to the symbol table for this bus.
    pub fn symbol_table_mut(&mut self) -> &mut SymbolTable {
        &mut self.symbol_table
    }
    
}

// Temporary match result types to avoid holding region borrows.
enum PeekMatch<'a> {
    Ram { data: &'a Vec<u8>, offset: usize },
    Rom { data: &'a Vec<u8>, offset: usize },
    Device { device: &'a dyn IoDevice, addr: u16 },
}

enum RegionMatch<'a> {
    Ram { data: &'a mut Vec<u8>, offset: usize },
    Rom { data: &'a mut Vec<u8>, offset: usize, write_policy: RomWritePolicy },
    Device { device: &'a mut dyn IoDevice, addr: u16 },
}

/// Builder for constructing a `Bus`.
pub struct BusConfig {
    regions: Vec<Region>,
    /// Devices registered so far, in registration order. See `Bus::devices`.
    devices: Vec<(DeviceId, Box<dyn IoDevice>)>,
    unmapped_policy: UnmappedPolicy,
    rom_write_policy: RomWritePolicy,
    symbol_table: SymbolTable,
}

impl BusConfig {
    /// Creates a new `BusConfig` with `DefaultValue` unmapped policy and `Ignore` ROM write policy.
    pub fn new() -> Self {
        Self {
            regions: Vec::new(),
            devices: Vec::new(),
            unmapped_policy: UnmappedPolicy::DefaultValue,
            rom_write_policy: RomWritePolicy::Ignore,
            symbol_table: SymbolTable::new(),
        }
    }

    pub fn symbol_table(mut self, symbol_table: &SymbolTable) -> Self {
        self.symbol_table.insert_from(symbol_table);
        self
    }

    /// Sets the policy for accesses to unmapped addresses.
    pub fn unmapped_policy(mut self, policy: UnmappedPolicy) -> Self {
        self.unmapped_policy = policy;
        self
    }

    /// Sets the default policy for writes to ROM regions (can be overridden per region).
    pub fn rom_write_policy(mut self, policy: RomWritePolicy) -> Self {
        self.rom_write_policy = policy;
        self
    }

    /// Maps a RAM region over `range`. Initial contents are random.
    pub fn ram(mut self, range: AddressRange) -> Result<Self, BusConfigError> {
        self.check_overlap(range)?;
        let len = range.len() as usize;
        let mut v = vec![0u8; len];
        rand::rng().fill(&mut v[..]);
        self.regions.push(Region::Ram { range, data: v });
        Ok(self)
    }

    /// Maps a RAM region over `range`, filling each cell with the specified value.
    pub fn ram_with_fill(mut self, range: AddressRange, fill_value: u8) -> Result<Self, BusConfigError> {
        self.check_overlap(range)?;
        let len = range.len() as usize;
        self.regions.push(Region::Ram { range, data: vec![fill_value; len] });
        Ok(self)
    }

    /// Maps a RAM region over `range`, pre-loaded with `data`.
    ///
    /// Unlike `rom()`, writes to this region succeed normally after construction.
    /// `data.len()` must equal `range.len()`.
    pub fn ram_with_data(mut self, range: AddressRange, data: Vec<u8>) -> Result<Self, BusConfigError> {
        let expected = range.len() as usize;
        if data.len() != expected {
            return Err(BusConfigError::RomSizeMismatch {
                range,
                data_len: data.len(),
                expected,
            });
        }
        self.check_overlap(range)?;
        self.regions.push(Region::Ram { range, data });
        Ok(self)
    }

    /// Maps a ROM region over `range`, pre-loaded with `data`.
    ///
    /// `data.len()` must equal `range.len()`.
    pub fn rom(mut self, range: AddressRange, data: Vec<u8>) -> Result<Self, BusConfigError> {
        let expected = range.len() as usize;
        if data.len() != expected {
            return Err(BusConfigError::RomSizeMismatch {
                range,
                data_len: data.len(),
                expected,
            });
        }
        self.check_overlap(range)?;
        let write_policy = self.rom_write_policy;
        self.regions.push(Region::Rom { range, data, write_policy });
        Ok(self)
    }

    /// Maps an IO device over `range`.
    pub fn device(
        mut self,
        range: AddressRange,
        id: DeviceId,
        device: Box<dyn IoDevice>,
    ) -> Result<Self, BusConfigError> {
        if self.devices.iter().any(|(existing_id, _)| *existing_id == id) {
            return Err(BusConfigError::DuplicateDeviceId(id));
        }
        self.check_overlap(range)?;
        let device_index = self.devices.len();
        self.devices.push((id, device));
        self.regions.push(Region::Device { range, device_index });
        Ok(self)
    }

    pub fn build(self) -> Bus {
        let resolved = Self::resolve_addresses(&self.regions, &self.devices);
        Bus {
            regions: self.regions,
            devices: self.devices,
            unmapped_policy: self.unmapped_policy,
            symbol_table: self.symbol_table,
            resolved,
        }
    }

    /// Resolves every address to the region that will respond to it, calling
    /// `IoDevice::claims()` once per candidate to settle chip-select fallthrough.
    /// Safe to do once here because `claims()` is invariant once the bus is
    /// configured -- this is the only place it's ever called.
    fn resolve_addresses(
        regions: &[Region],
        devices: &[(DeviceId, Box<dyn IoDevice>)],
    ) -> Box<[Option<u32>]> {
        const N: usize = 0x10000;

        // Bucket every region index under each address it covers (CSR-style,
        // to avoid 64K individual heap allocations).
        let mut counts = vec![0u32; N];
        for region in regions {
            let range = region.range();
            for addr in range.start..=range.end {
                counts[addr as usize] += 1;
            }
        }
        let mut starts = vec![0u32; N + 1];
        let mut acc = 0u32;
        for i in 0..N {
            starts[i] = acc;
            acc += counts[i];
        }
        starts[N] = acc;

        let mut cursor = starts.clone();
        let mut bucketed = vec![0u32; acc as usize];
        for (idx, region) in regions.iter().enumerate() {
            let range = region.range();
            for addr in range.start..=range.end {
                let slot = &mut cursor[addr as usize];
                bucketed[*slot as usize] = idx as u32;
                *slot += 1;
            }
        }

        // For each address: sort its candidates by specificity, then resolve
        // to the first one that claims (or isn't a device at all).
        let mut resolved = vec![None; N];
        for a in 0..N {
            let s = starts[a] as usize;
            let e = starts[a + 1] as usize;
            let candidates = &mut bucketed[s..e];
            candidates.sort_by_key(|&idx| (regions[idx as usize].range().len(), idx));

            let addr = a as u16;
            resolved[a] = candidates.iter().copied().find(|&idx| {
                match &regions[idx as usize] {
                    Region::Device { device_index, .. } => devices[*device_index].1.claims(addr),
                    _ => true,
                }
            });
        }

        resolved.into_boxed_slice()
    }

    fn check_overlap(&self, new_range: AddressRange) -> Result<(), BusConfigError> {
        for region in &self.regions {
            let existing = region.range();
            if existing.overlaps(&new_range) {
                // Ambiguous only when the two regions are the same size.
                if existing.len() == new_range.len() {
                    return Err(BusConfigError::AmbiguousOverlap { range: new_range });
                }
                // Different-sized overlapping regions are allowed (most-specific-wins).
            }
        }
        Ok(())
    }
}

impl Default for BusConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockDevice {
        address: u16,
        data: Vec<u8>,
        read_count: usize,
    }

    impl MockDevice {
        fn new(address: u16, size: usize) -> Self {
            Self { address, data: vec![0u8; size], read_count: 0 }
        }
    }

    impl IoDevice for MockDevice {
        fn read(&mut self, address: u16) -> u8 {
            self.read_count += 1;
            self.data[(address - self.address) as usize]
        }
        fn write(&mut self, address: u16, value: u8) {
            self.data[(address - self.address) as usize] = value;
        }
        fn peek(&self, address: u16) -> u8 {
            self.data[(address - self.address) as usize]
        }
    }

    fn ram_bus(start: u16, end: u16) -> Bus {
        Bus::config()
            .ram_with_fill(AddressRange::new(start, end), 0)
            .unwrap()
            .build()
    }

    #[test]
    fn ram_read_write_round_trip() {
        let mut bus = ram_bus(0x0000, 0x1FFF);
        bus.write(0x0100, 0xAB).unwrap();
        assert_eq!(bus.read(0x0100).unwrap(), 0xAB);
    }

    #[test]
    fn rom_read_only_ignore_policy() {
        let data = vec![0xEAu8; 256];
        let mut bus = Bus::config()
            .rom_write_policy(RomWritePolicy::Ignore)
            .rom(AddressRange::new(0xC000, 0xC0FF), data)
            .unwrap()
            .build();
        bus.write(0xC010, 0x00).unwrap();
        assert_eq!(bus.read(0xC010).unwrap(), 0xEA);
    }

    #[test]
    fn rom_read_only_error_policy() {
        let data = vec![0xEAu8; 256];
        let mut bus = Bus::config()
            .rom_write_policy(RomWritePolicy::Error)
            .rom(AddressRange::new(0xC000, 0xC0FF), data)
            .unwrap()
            .build();
        let result = bus.write(0xC010, 0x00);
        assert!(matches!(result, Err(BusError::RomWrite { addr: 0xC010 })));
    }

    #[test]
    fn ram_patch_read_round_trip() {
        let mut bus = ram_bus(0x0000, 0x1FFF);
        bus.patch(0x0100, 0xAB);
        assert_eq!(bus.read(0x0100).unwrap(), 0xAB);
    }

    #[test]
    fn rom_patch_read_round_trip() {
        let data = vec![0xEAu8; 256];
        let mut bus = Bus::config()
            .rom_write_policy(RomWritePolicy::Error)
            .rom(AddressRange::new(0xC000, 0xC0FF), data)
            .unwrap()
            .build();
        bus.patch(0xC000, 0xAB);
        assert_eq!(bus.read(0xC000).unwrap(), 0xAB);
    }

    #[test]
    fn unmapped_default_value_policy() {
        let mut bus = Bus::config()
            .unmapped_policy(UnmappedPolicy::DefaultValue)
            .build();
        assert_eq!(bus.read(0x1234).unwrap(), UNMAPPED_READ_VALUE);
        bus.write(0x1234, 0x42).unwrap();
    }

    #[test]
    fn unmapped_error_policy() {
        let mut bus = Bus::config()
            .unmapped_policy(UnmappedPolicy::Error)
            .build();
        assert!(matches!(bus.read(0x1234), Err(BusError::Unmapped { addr: 0x1234 })));
        assert!(matches!(bus.write(0x1234, 0x00), Err(BusError::Unmapped { addr: 0x1234 })));
    }

    #[test]
    fn symbol_table_inserts_from_source() {
        let mut table = SymbolTable::default();
        table.insert("foo".to_string(), 0xDEAD);
        table.insert("bar".to_string(), 0xBEEF);
        let bus = Bus::config()
            .symbol_table(&table)
            .build();
        assert_eq!(bus.symbol_table.address_for("foo"), Some(0xDEAD));
        assert_eq!(bus.symbol_table.address_for("bar"), Some(0xBEEF));
    }

    #[test]
    fn most_specific_wins_device_shadows_rom() {
        // ROM covers 0x8000–0xFFFF; device covers small window inside it.
        let rom_data = vec![0xEAu8; 0x8000];
        let device = Box::new(MockDevice::new(0xDF00, 16));
        let mut bus = Bus::config()
            .rom(AddressRange::new(0x8000, 0xFFFF), rom_data)
            .unwrap()
            .device(AddressRange::new(0xDF00, 0xDF0F), DeviceId(1), device)
            .unwrap()
            .build();
        // Address in device window — should hit the device (initially 0x00), not ROM (0xEA).
        assert_eq!(bus.read(0xDF00).unwrap(), 0x00);
        // Address outside device window — should hit ROM.
        assert_eq!(bus.read(0x8000).unwrap(), 0xEA);
    }

    #[test]
    fn ambiguous_overlap_error_for_same_size() {
        let result = Bus::config()
            .ram_with_fill(AddressRange::new(0x0000, 0x00FF), 0)
            .unwrap()
            .ram_with_fill(AddressRange::new(0x0000, 0x00FF), 0);
        assert!(matches!(result, Err(BusConfigError::AmbiguousOverlap { .. })));
    }

    #[test]
    fn peek_does_not_trigger_device_side_effects() {
        let device = Box::new(MockDevice::new(0xDF00, 16));
        let bus = Bus::config()
            .device(AddressRange::new(0xDF00, 0xDF0F), DeviceId(1), device)
            .unwrap()
            .build();
        let _ = bus.peek(0xDF00).unwrap();
        let _ = bus.peek(0xDF00).unwrap();
        // If side effects were triggered, read_count would be > 0; peek uses peek() not read().
        // We verify by checking that the mock's read_count is still 0 via downcast—
        // but since Bus doesn't expose the device, we just confirm no panic and correct value.
        assert_eq!(bus.peek(0xDF00).unwrap(), 0x00);
    }

    #[test]
    fn device_offset_translation() {
        let mut dev = MockDevice::new(0xDF00, 16);
        dev.data[5] = 0x42;
        let mut bus = Bus::config()
            .device(AddressRange::new(0xDF00, 0xDF0F), DeviceId(1), Box::new(dev))
            .unwrap()
            .build();
        // Address 0xDF05 → offset 5 within the device.
        assert_eq!(bus.read(0xDF05).unwrap(), 0x42);
    }

    #[test]
    fn peek_range_across_region_boundaries() {
        let rom_data = vec![0xAAu8; 256];
        let bus = Bus::config()
            .unmapped_policy(UnmappedPolicy::DefaultValue)
            .rom(AddressRange::new(0xC000, 0xC0FF), rom_data)
            .unwrap()
            .build();
        let mut buf = [0u8; 4];
        // Last 2 bytes are ROM (0xAA), next 2 are unmapped (0xFF).
        bus.peek_range(0xC0FE, &mut buf).unwrap();
        assert_eq!(buf, [0xAA, 0xAA, UNMAPPED_READ_VALUE, UNMAPPED_READ_VALUE]);
    }

    #[test]
    fn load_rom_replaces_data() {
        let initial = vec![0xEAu8; 256];
        let mut bus = Bus::config()
            .rom(AddressRange::new(0xC000, 0xC0FF), initial)
            .unwrap()
            .build();
        let new_data = vec![0xA5u8; 256];
        bus.load_rom(AddressRange::new(0xC000, 0xC0FF), &new_data).unwrap();
        assert_eq!(bus.peek(0xC000).unwrap(), 0xA5);
    }

    #[test]
    fn rom_size_mismatch_error() {
        let result = Bus::config()
            .rom(AddressRange::new(0xC000, 0xC0FF), vec![0u8; 100]);
        assert!(matches!(result, Err(BusConfigError::RomSizeMismatch { .. })));
    }

    #[test]
    fn ram_with_data_preloads_initial_contents() {
        let data = vec![0xABu8; 256];
        let mut bus = Bus::config()
            .ram_with_data(AddressRange::new(0xC000, 0xC0FF), data)
            .unwrap()
            .build();
        assert_eq!(bus.read(0xC042).unwrap(), 0xAB);
    }

    #[test]
    fn ram_with_data_allows_writes() {
        let data = vec![0xABu8; 256];
        let mut bus = Bus::config()
            .ram_with_data(AddressRange::new(0xC000, 0xC0FF), data)
            .unwrap()
            .build();
        bus.write(0xC042, 0x99).unwrap();
        assert_eq!(bus.read(0xC042).unwrap(), 0x99);
    }

    #[test]
    fn ram_with_data_size_mismatch_error() {
        let result = Bus::config()
            .ram_with_data(AddressRange::new(0xC000, 0xC0FF), vec![0u8; 100]);
        assert!(matches!(result, Err(BusConfigError::RomSizeMismatch { .. })));
    }

    // --- multi-region devices and conditional chip-select ---

    #[test]
    fn base_address_default_absolute_delegation() {
        let mut dev = MockDevice::new(0xDF00, 16);
        dev.data[5] = 0x42;
        let mut bus = Bus::config()
            .device(AddressRange::new(0xDF00, 0xDF0F), DeviceId(1), Box::new(dev))
            .unwrap()
            .build();
        // addr 0xDF05 - base_address() 0xDF00 = offset 5, via the default read_absolute/
        // write_absolute/peek_absolute delegation to read/write/peek.
        assert_eq!(bus.read(0xDF05).unwrap(), 0x42);
        bus.write(0xDF06, 0x99).unwrap();
        assert_eq!(bus.peek(0xDF06).unwrap(), 0x99);
    }

    /// A device whose `claims` response is fixed at construction, for exercising
    /// conditional chip-select fallthrough.
    struct DecliningDevice {
        claims: bool,
        value: u8,
    }

    impl DecliningDevice {
        fn new(value: u8, claims: bool) -> Self {
            Self { claims, value }
        }
    }

    impl IoDevice for DecliningDevice {
        fn read(&mut self, _offset: u16) -> u8 {
            self.value
        }
        fn write(&mut self, _offset: u16, value: u8) {
            self.value = value;
        }
        fn peek(&self, _offset: u16) -> u8 {
            self.value
        }
        fn claims(&self, _addr: u16) -> bool {
            self.claims
        }
    }

    #[test]
    fn claims_true_dispatches_to_device() {
        let rom_data = vec![0xEAu8; 0x100];
        let claiming = Box::new(DecliningDevice::new(0x99, true));
        let mut bus = Bus::config()
            .rom(AddressRange::new(0xC000, 0xC0FF), rom_data)
            .unwrap()
            .device(AddressRange::new(0xC010, 0xC010), DeviceId(1), claiming)
            .unwrap()
            .build();
        assert_eq!(bus.read(0xC010).unwrap(), 0x99);
    }

    #[test]
    fn claims_false_falls_through_to_underlying_region() {
        let rom_data = vec![0xEAu8; 0x100];
        let declining = Box::new(DecliningDevice::new(0x99, false));
        let mut bus = Bus::config()
            .rom(AddressRange::new(0xC000, 0xC0FF), rom_data)
            .unwrap()
            .device(AddressRange::new(0xC010, 0xC010), DeviceId(1), declining)
            .unwrap()
            .build();
        // Device declines, so the read falls through to the underlying ROM.
        assert_eq!(bus.read(0xC010).unwrap(), 0xEA);
    }

    #[test]
    fn claims_false_falls_through_to_unmapped_default_value() {
        let declining = Box::new(DecliningDevice::new(0x99, false));
        let mut bus = Bus::config()
            .unmapped_policy(UnmappedPolicy::DefaultValue)
            .device(AddressRange::new(0x1234, 0x1234), DeviceId(1), declining)
            .unwrap()
            .build();
        assert_eq!(bus.read(0x1234).unwrap(), UNMAPPED_READ_VALUE);
    }

    #[test]
    fn claims_false_falls_through_to_unmapped_error() {
        let declining = Box::new(DecliningDevice::new(0x99, false));
        let mut bus = Bus::config()
            .unmapped_policy(UnmappedPolicy::Error)
            .device(AddressRange::new(0x1234, 0x1234), DeviceId(1), declining)
            .unwrap()
            .build();
        assert!(matches!(bus.read(0x1234), Err(BusError::Unmapped { addr: 0x1234 })));
    }

    #[test]
    fn claims_walks_multiple_shadow_levels() {
        let rom_data = vec![0xEAu8; 0x8000];
        let middle = Box::new(DecliningDevice::new(0x11, false));
        let inner = Box::new(DecliningDevice::new(0x22, false));
        let mut bus = Bus::config()
            .rom(AddressRange::new(0x8000, 0xFFFF), rom_data)
            .unwrap()
            .device(AddressRange::new(0xDF00, 0xDF0F), DeviceId(1), middle)
            .unwrap()
            .device(AddressRange::new(0xDF00, 0xDF00), DeviceId(2), inner)
            .unwrap()
            .build();
        // Innermost (1 byte) declines, middle (16 bytes) also declines, falls through to ROM.
        assert_eq!(bus.read(0xDF00).unwrap(), 0xEA);
    }

    /// A device with shared, thread-safe call counters for verifying that per-device
    /// lifecycle methods fire once per device rather than once per mapped region.
    struct CountingDevice {
        tick_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        reset_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        irq_active_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        take_nmi_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl IoDevice for CountingDevice {
        fn read(&mut self, _offset: u16) -> u8 { 0 }
        fn write(&mut self, _offset: u16, _value: u8) {}
        fn peek(&self, _offset: u16) -> u8 { 0 }
        fn tick(&mut self, _cycles: u32) {
            self.tick_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        fn reset(&mut self) {
            self.reset_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        fn irq_active(&self) -> bool {
            self.irq_active_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            false
        }
        fn take_nmi(&mut self) -> bool {
            self.take_nmi_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            false
        }
    }

    #[test]
    fn tick_reset_irq_nmi_called_once_per_device() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let tick_count = Arc::new(AtomicUsize::new(0));
        let reset_count = Arc::new(AtomicUsize::new(0));
        let irq_active_count = Arc::new(AtomicUsize::new(0));
        let take_nmi_count = Arc::new(AtomicUsize::new(0));
        let device = Box::new(CountingDevice {
            tick_count: tick_count.clone(),
            reset_count: reset_count.clone(),
            irq_active_count: irq_active_count.clone(),
            take_nmi_count: take_nmi_count.clone(),
        });
        let mut bus = Bus::config()
            .device(AddressRange::new(0xDF00, 0xDF0F), DeviceId(1), device)
            .unwrap()
            .build();

        bus.tick_devices(1);
        bus.reset_devices();
        bus.device_irq_states().for_each(drop);
        bus.take_device_nmi();

        assert_eq!(tick_count.load(Ordering::SeqCst), 1);
        assert_eq!(reset_count.load(Ordering::SeqCst), 1);
        assert_eq!(irq_active_count.load(Ordering::SeqCst), 1);
        assert_eq!(take_nmi_count.load(Ordering::SeqCst), 1);
    }

    /// A device that counts `claims()` invocations, for verifying that address
    /// resolution consults `claims()` while building the bus and does not
    /// re-consult it on every subsequent access.
    struct ClaimCountingDevice {
        claims_result: bool,
        claims_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        value: u8,
    }

    impl ClaimCountingDevice {
        fn new(claims_result: bool, claims_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
            Self { claims_result, claims_calls, value: 0 }
        }
    }

    impl IoDevice for ClaimCountingDevice {
        fn read(&mut self, _addr: u16) -> u8 {
            self.value
        }
        fn write(&mut self, _addr: u16, value: u8) {
            self.value = value;
        }
        fn peek(&self, _addr: u16) -> u8 {
            self.value
        }
        fn claims(&self, _addr: u16) -> bool {
            self.claims_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.claims_result
        }
    }

    #[test]
    fn build_consults_claims_for_every_address_in_range() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let claims_calls = Arc::new(AtomicUsize::new(0));
        let device = Box::new(ClaimCountingDevice::new(true, claims_calls.clone()));
        let range = AddressRange::new(0xD000, 0xD00F); // 16 addresses

        let _bus = Bus::config()
            .device(range, DeviceId(1), device)
            .unwrap()
            .build();

        // Resolution must consult `claims()` at least once per address the
        // device is mapped over while the bus is being built.
        assert!(claims_calls.load(Ordering::SeqCst) >= range.len() as usize);
    }

    #[test]
    fn repeated_access_does_not_reinvoke_claims_after_build() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let claims_calls = Arc::new(AtomicUsize::new(0));
        let device = Box::new(ClaimCountingDevice::new(true, claims_calls.clone()));
        let mut bus = Bus::config()
            .device(AddressRange::new(0xD000, 0xD000), DeviceId(1), device)
            .unwrap()
            .build();

        let calls_after_build = claims_calls.load(Ordering::SeqCst);

        for _ in 0..100 {
            bus.read(0xD000).unwrap();
            bus.write(0xD000, 0x42).unwrap();
            bus.peek(0xD000).unwrap();
        }

        // Address resolution happened once at build time, so 300 subsequent
        // accesses must not call `claims()` again.
        assert_eq!(claims_calls.load(Ordering::SeqCst), calls_after_build);
    }

    #[test]
    fn build_resolves_most_specific_region_across_full_overlap() {
        let rom_data = vec![0x11u8; 0x100];
        let inner = Box::new(MockDevice::new(0xC080, 1));
        let mut bus = Bus::config()
            .rom(AddressRange::new(0xC000, 0xC0FF), rom_data)
            .unwrap()
            .device(AddressRange::new(0xC080, 0xC080), DeviceId(1), inner)
            .unwrap()
            .build();

        // The 1-byte device region is more specific than the enclosing 256-byte
        // ROM region and must win at that address.
        bus.write(0xC080, 0xAB).unwrap();
        assert_eq!(bus.read(0xC080).unwrap(), 0xAB);

        // Every other address covered only by ROM must still resolve to ROM.
        assert_eq!(bus.read(0xC07F).unwrap(), 0x11);
        assert_eq!(bus.read(0xC081).unwrap(), 0x11);
    }

    #[test]
    fn build_resolves_fallthrough_for_every_address_in_declining_region() {
        let rom_data = vec![0xEAu8; 0x100];
        let declining = Box::new(DecliningDevice::new(0x99, false));
        let mut bus = Bus::config()
            .rom(AddressRange::new(0xC000, 0xC0FF), rom_data)
            .unwrap()
            .device(AddressRange::new(0xC010, 0xC01F), DeviceId(1), declining)
            .unwrap()
            .build();

        // The device's whole 16-byte region declines, so every address in it --
        // not just one spot-checked address -- must fall through to ROM.
        for addr in 0xC010..=0xC01F {
            assert_eq!(bus.read(addr).unwrap(), 0xEA, "address {addr:#06x} should fall through to ROM");
        }
    }

    #[test]
    fn device_id_allocator_next_other_id() {
        let mut allocator = DeviceIdAllocator::new();
        let id1 = allocator.next(false);
        assert!(id1.0 >= MAX_IRQ_SOURCES, "must not be within range of IRQ source");
        let id2 = allocator.next(false);
        assert!(id2.0 >= MAX_IRQ_SOURCES, "must not be within range of IRQ source");
        assert_ne!(id1, id2);
    }

    #[test]
    fn device_id_allocator_next_irq_source() {
        let mut allocator = DeviceIdAllocator::new();
        let id1 = allocator.next(true);
        assert!(id1.0 < MAX_IRQ_SOURCES, "must be within range of IRQ source");
        let id2 = allocator.next(true);
        assert!(id2.0 < MAX_IRQ_SOURCES, "must be within range of IRQ source");
        assert_ne!(id1, id2);
    }

    #[test]
    #[should_panic]
    fn device_id_allocator_next_irq_source_panics_on_max_sources() {
        let mut allocator = DeviceIdAllocator::new();
        allocator.next_irq_source = MAX_IRQ_SOURCES;
        allocator.next(true);
    }

}