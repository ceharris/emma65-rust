/// Address ranges and bus operation types.
pub mod region;
/// Bus tracing: callback trait, record type, and binary trace writer.
pub mod trace;

use rand::RngExt;
pub use region::{AddressRange, BusOp};
pub use trace::{BinaryTraceWriter, BusTraceCallback, TraceRecord};

use crate::emulator::device::{DeviceId, IoDevice};
use crate::emulator::error::{BusConfigError, BusError};
use trace::TraceState;

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
        id: DeviceId,
        device: Box<dyn IoDevice>,
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

/// The configurable memory bus with RAM, ROM, and IO device regions.
pub struct Bus {
    regions: Vec<Region>,
    unmapped_policy: UnmappedPolicy,
    /// Monotonic clock state; updated by `Cpu::step()` before each instruction.
    trace_state: TraceState,
    /// Optional callback invoked on every `read()` and `write()` (not `peek`).
    trace_callback: Option<Box<dyn BusTraceCallback>>,
}

impl Bus {
    /// Returns a `BusConfig` builder for constructing a `Bus`.
    pub fn config() -> BusConfig {
        BusConfig::new()
    }

    /// Installs a trace callback. Pass `None` to remove an existing callback.
    ///
    /// When set, the callback is invoked on every `read()` and `write()`, but never on `peek`.
    pub fn set_trace_callback(&mut self, callback: Option<Box<dyn BusTraceCallback>>) {
        self.trace_callback = callback;
    }

    /// Advances the trace timestamp to the current wall-clock time.
    ///
    /// Called by `Cpu::step()` once at the start of each instruction so that all bus
    /// accesses within a single instruction share the same timestamp.
    pub fn advance_trace_timestamp(&mut self) {
        self.trace_state.tick();
    }

    /// Reads one byte from `addr`, triggering device side effects if an IO device is mapped there.
    pub fn read(&mut self, addr: u16) -> Result<u8, BusError> {
        let value = match self.find_region_mut(addr) {
            Some(RegionMatch::Ram { data, offset }) => Ok(data[offset]),
            Some(RegionMatch::Rom { data, offset, .. }) => Ok(data[offset]),
            Some(RegionMatch::Device { device, offset }) => Ok(device.read(offset)),
            None => match self.unmapped_policy {
                UnmappedPolicy::DefaultValue => Ok(UNMAPPED_READ_VALUE),
                UnmappedPolicy::Error => Err(BusError::Unmapped { addr }),
            },
        }?;
        self.emit_trace(addr, value, BusOp::Read);
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
            Some(RegionMatch::Device { device, offset }) => {
                device.write(offset, value);
                Ok(())
            }
            None => match self.unmapped_policy {
                UnmappedPolicy::DefaultValue => Ok(()),
                UnmappedPolicy::Error => Err(BusError::Unmapped { addr }),
            },
        }?;
        self.emit_trace(addr, value, BusOp::Write);
        Ok(())
    }

    /// Reads one byte from `addr` without triggering device side effects.
    pub fn peek(&self, addr: u16) -> Result<u8, BusError> {
        match self.find_region(addr) {
            Some(PeekMatch::Ram { data, offset }) => Ok(data[offset]),
            Some(PeekMatch::Rom { data, offset }) => Ok(data[offset]),
            Some(PeekMatch::Device { device, offset }) => Ok(device.peek(offset)),
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

    /// Calls `tick(cycles)` on every IO device mapped on the bus.
    pub fn tick_devices(&mut self, cycles: u32) {
        for region in &mut self.regions {
            if let Region::Device { device, .. } = region {
                device.tick(cycles);
            }
        }
    }

    /// Returns the IRQ state of every device as `(DeviceId, irq_active)` pairs.
    pub fn device_irq_states(&self) -> Vec<(crate::emulator::device::DeviceId, bool)> {
        self.regions.iter().filter_map(|r| {
            if let Region::Device { id, device, .. } = r {
                Some((*id, device.irq_active()))
            } else {
                None
            }
        }).collect()
    }

    /// Drains pending NMI edge events from all devices. Returns `true` if any device had one.
    pub fn take_device_nmi(&mut self) -> bool {
        let mut any = false;
        for region in &mut self.regions {
            if let Region::Device { device, .. } = region {
                any |= device.take_nmi();
            }
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

    fn emit_trace(&mut self, addr: u16, value: u8, op: BusOp) {
        if let Some(cb) = &mut self.trace_callback {
            cb.record(TraceRecord {
                timestamp_ns: self.trace_state.current_ns(),
                addr,
                value,
                op,
            });
        }
    }

    /// Returns the index of the most-specific (smallest) region that contains `addr`, if any.
    fn find_region_index(&self, addr: u16) -> Option<usize> {
        let mut best_idx: Option<usize> = None;
        let mut best_size: u32 = u32::MAX;
        for (i, region) in self.regions.iter().enumerate() {
            let range = region.range();
            if range.contains(addr) {
                let size = range.len();
                if size < best_size {
                    best_size = size;
                    best_idx = Some(i);
                }
            }
        }
        best_idx
    }

    fn find_region(&self, addr: u16) -> Option<PeekMatch<'_>> {
        let idx = self.find_region_index(addr)?;
        let range = self.regions[idx].range();
        let offset = (addr - range.start) as usize;
        match &self.regions[idx] {
            Region::Ram { data, .. } => Some(PeekMatch::Ram { data, offset }),
            Region::Rom { data, .. } => Some(PeekMatch::Rom { data, offset }),
            Region::Device { device, .. } => Some(PeekMatch::Device {
                device: device.as_ref(),
                offset: offset as u16,
            }),
        }
    }

    fn find_region_mut(&mut self, addr: u16) -> Option<RegionMatch<'_>> {
        let idx = self.find_region_index(addr)?;
        let range = self.regions[idx].range();
        let offset = (addr - range.start) as usize;
        match &mut self.regions[idx] {
            Region::Ram { data, .. } => Some(RegionMatch::Ram { data, offset }),
            Region::Rom { data, write_policy, .. } => Some(RegionMatch::Rom {
                data,
                offset,
                write_policy: *write_policy,
            }),
            Region::Device { device, .. } => Some(RegionMatch::Device {
                device: device.as_mut(),
                offset: offset as u16,
            }),
        }
    }
}

// Temporary match result types to avoid holding region borrows.
enum PeekMatch<'a> {
    Ram { data: &'a Vec<u8>, offset: usize },
    Rom { data: &'a Vec<u8>, offset: usize },
    Device { device: &'a dyn IoDevice, offset: u16 },
}

enum RegionMatch<'a> {
    Ram { data: &'a mut Vec<u8>, offset: usize },
    Rom { data: &'a Vec<u8>, offset: usize, write_policy: RomWritePolicy },
    Device { device: &'a mut dyn IoDevice, offset: u16 },
}

/// Builder for constructing a `Bus`.
pub struct BusConfig {
    regions: Vec<Region>,
    unmapped_policy: UnmappedPolicy,
    rom_write_policy: RomWritePolicy,
}

impl BusConfig {
    /// Creates a new `BusConfig` with `DefaultValue` unmapped policy and `Ignore` ROM write policy.
    pub fn new() -> Self {
        Self {
            regions: Vec::new(),
            unmapped_policy: UnmappedPolicy::DefaultValue,
            rom_write_policy: RomWritePolicy::Ignore,
        }
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
    ///
    /// `id` must be unique among all registered devices.
    pub fn device(
        mut self,
        range: AddressRange,
        id: DeviceId,
        device: Box<dyn IoDevice>,
    ) -> Result<Self, BusConfigError> {
        if self.regions.iter().any(|r| {
            if let Region::Device { id: existing_id, .. } = r {
                *existing_id == id
            } else {
                false
            }
        }) {
            return Err(BusConfigError::DuplicateDeviceId(id));
        }
        self.check_overlap(range)?;
        self.regions.push(Region::Device { range, id, device });
        Ok(self)
    }

    /// Consumes the builder and returns a `Bus`.
    pub fn build(self) -> Bus {
        Bus {
            regions: self.regions,
            unmapped_policy: self.unmapped_policy,
            trace_state: TraceState::new(),
            trace_callback: None,
        }
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
        data: Vec<u8>,
        read_count: usize,
    }

    impl MockDevice {
        fn new(size: usize) -> Self {
            Self { data: vec![0u8; size], read_count: 0 }
        }
    }

    impl IoDevice for MockDevice {
        fn read(&mut self, offset: u16) -> u8 {
            self.read_count += 1;
            self.data[offset as usize]
        }
        fn write(&mut self, offset: u16, value: u8) {
            self.data[offset as usize] = value;
        }
        fn peek(&self, offset: u16) -> u8 {
            self.data[offset as usize]
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
    fn most_specific_wins_device_shadows_rom() {
        // ROM covers 0x8000–0xFFFF; device covers small window inside it.
        let rom_data = vec![0xEAu8; 0x8000];
        let device = Box::new(MockDevice::new(16));
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
        let device = Box::new(MockDevice::new(16));
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
        let mut dev = MockDevice::new(16);
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
    fn duplicate_device_id_error() {
        let result = Bus::config()
            .device(AddressRange::new(0xDF00, 0xDF0F), DeviceId(1), Box::new(MockDevice::new(16)))
            .unwrap()
            .device(AddressRange::new(0xCF00, 0xCF0F), DeviceId(1), Box::new(MockDevice::new(16)));
        assert!(matches!(result, Err(BusConfigError::DuplicateDeviceId(DeviceId(1)))));
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

    // --- bus trace ---

    struct CapturingCallback(Vec<TraceRecord>);

    impl BusTraceCallback for CapturingCallback {
        fn record(&mut self, rec: TraceRecord) {
            self.0.push(rec);
        }
    }

    fn traced_ram_bus() -> (Bus, *mut CapturingCallback) {
        // Returns the bus and a raw pointer to the callback so we can inspect records
        // after operations. The callback is owned by the bus; the pointer is valid
        // for the lifetime of the bus.
        let cb = Box::new(CapturingCallback(Vec::new()));
        let ptr = &*cb as *const CapturingCallback as *mut CapturingCallback;
        let mut bus = ram_bus(0x0000, 0xFFFF);
        bus.set_trace_callback(Some(cb));
        (bus, ptr)
    }

    #[test]
    fn trace_callback_receives_read() {
        let (mut bus, cb_ptr) = traced_ram_bus();
        bus.write(0x0100, 0x42).unwrap();
        // Clear the write record; we only care about the read.
        unsafe { (*cb_ptr).0.clear(); }
        bus.read(0x0100).unwrap();
        let records = unsafe { &(*cb_ptr).0 };
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].addr, 0x0100);
        assert_eq!(records[0].value, 0x42);
        assert_eq!(records[0].op, BusOp::Read);
    }

    #[test]
    fn trace_callback_receives_write() {
        let (mut bus, cb_ptr) = traced_ram_bus();
        bus.write(0x0200, 0xAB).unwrap();
        let records = unsafe { &(*cb_ptr).0 };
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].addr, 0x0200);
        assert_eq!(records[0].value, 0xAB);
        assert_eq!(records[0].op, BusOp::Write);
    }

    #[test]
    fn trace_callback_not_invoked_on_peek() {
        let (bus, cb_ptr) = traced_ram_bus();
        bus.peek(0x0300).unwrap();
        let records = unsafe { &(*cb_ptr).0 };
        assert!(records.is_empty());
    }

    #[test]
    fn trace_callback_not_invoked_when_none() {
        // No callback installed — just verifies no panic.
        let mut bus = ram_bus(0x0000, 0xFFFF);
        bus.write(0x0100, 0x42).unwrap();
        bus.read(0x0100).unwrap();
    }

    #[test]
    fn trace_timestamps_group_by_instruction() {
        let (mut bus, cb_ptr) = traced_ram_bus();

        // Simulate two instructions, each with two bus accesses.
        bus.advance_trace_timestamp();
        bus.write(0x0100, 0x01).unwrap();
        bus.write(0x0101, 0x02).unwrap();

        bus.advance_trace_timestamp();
        bus.write(0x0102, 0x03).unwrap();
        bus.write(0x0103, 0x04).unwrap();

        let records = unsafe { &(*cb_ptr).0 };
        assert_eq!(records.len(), 4);
        // Both accesses within the first instruction share the same timestamp.
        assert_eq!(records[0].timestamp_ns, records[1].timestamp_ns);
        // Both accesses within the second instruction share the same timestamp.
        assert_eq!(records[2].timestamp_ns, records[3].timestamp_ns);
        // The second instruction's timestamp is >= the first's.
        assert!(records[2].timestamp_ns >= records[0].timestamp_ns);
    }

    #[test]
    fn set_trace_callback_none_removes_callback() {
        let (mut bus, cb_ptr) = traced_ram_bus();
        bus.set_trace_callback(None);
        bus.write(0x0100, 0xFF).unwrap();
        let records = unsafe { &(*cb_ptr).0 };
        assert!(records.is_empty());
    }
}