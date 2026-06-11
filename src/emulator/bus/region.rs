/// A contiguous range of addresses in the 16-bit address space, inclusive on both ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressRange {
    /// First address in the range (inclusive).
    pub start: u16,
    /// Last address in the range (inclusive).
    pub end: u16,
}

impl AddressRange {
    /// Creates a new address range spanning `start..=end` (both inclusive).
    pub fn new(start: u16, end: u16) -> Self {
        Self { start, end }
    }

    /// Returns `true` if `addr` falls within this range.
    pub fn contains(&self, addr: u16) -> bool {
        addr >= self.start && addr <= self.end
    }

    /// Returns the number of addresses in the range.
    pub fn len(&self) -> u32 {
        (self.end as u32) - (self.start as u32) + 1
    }

    /// Always returns `false`; an `AddressRange` always spans at least one address.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Returns `true` if this range shares at least one address with `other`.
    pub fn overlaps(&self, other: &AddressRange) -> bool {
        self.start <= other.end && other.start <= self.end
    }
}

/// Identifies a bus operation for error reporting and tracing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusOp {
    /// A read from the bus.
    Read,
    /// A write to the bus.
    Write,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_start_and_end() {
        let r = AddressRange::new(0x0200, 0x02FF);
        assert!(r.contains(0x0200));
        assert!(r.contains(0x02FF));
        assert!(r.contains(0x0250));
    }

    #[test]
    fn does_not_contain_outside() {
        let r = AddressRange::new(0x0200, 0x02FF);
        assert!(!r.contains(0x01FF));
        assert!(!r.contains(0x0300));
    }

    #[test]
    fn len_is_correct() {
        assert_eq!(AddressRange::new(0x0000, 0x00FF).len(), 256);
        assert_eq!(AddressRange::new(0x0000, 0xFFFF).len(), 65536);
        assert_eq!(AddressRange::new(0xFF00, 0xFF0F).len(), 16);
    }

    #[test]
    fn overlaps_partial() {
        let a = AddressRange::new(0x0000, 0x00FF);
        let b = AddressRange::new(0x0080, 0x017F);
        assert!(a.overlaps(&b));
        assert!(b.overlaps(&a));
    }

    #[test]
    fn overlaps_contained() {
        let outer = AddressRange::new(0x8000, 0xFFFF);
        let inner = AddressRange::new(0xFF00, 0xFF0F);
        assert!(outer.overlaps(&inner));
    }

    #[test]
    fn no_overlap_adjacent() {
        let a = AddressRange::new(0x0000, 0x00FF);
        let b = AddressRange::new(0x0100, 0x01FF);
        assert!(!a.overlaps(&b));
    }
}
