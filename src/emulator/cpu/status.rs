use bitflags::bitflags;

bitflags! {
    /// The 65C02 processor status register (P).
    ///
    /// Each bit is a named flag: N (negative), V (overflow), UNUSED, B (break), D (decimal),
    /// I (interrupt disable), Z (zero), C (carry). The UNUSED bit is always set on the real
    /// hardware when pushed to the stack; software should treat it as read-only.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct StatusRegister: u8 {
        const C      = 0x01;
        const Z      = 0x02;
        const I      = 0x04;
        const D      = 0x08;
        const B      = 0x10;
        const UNUSED = 0x20;
        const V      = 0x40;
        const N      = 0x80;
    }
}

impl StatusRegister {
    pub fn from_byte(byte: u8) -> Self {
        Self::from_bits_truncate(byte)
    }

    pub fn to_byte(self) -> u8 {
        self.bits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_byte_round_trip() {
        for b in 0u8..=255 {
            assert_eq!(StatusRegister::from_byte(b).to_byte(), b);
        }
    }

    #[test]
    fn individual_flag_set_and_clear() {
        let mut s = StatusRegister::empty();
        s.insert(StatusRegister::C);
        assert!(s.contains(StatusRegister::C));
        s.remove(StatusRegister::C);
        assert!(!s.contains(StatusRegister::C));
    }

    #[test]
    fn flag_bits_are_correct() {
        assert_eq!(StatusRegister::C.bits(), 0x01);
        assert_eq!(StatusRegister::Z.bits(), 0x02);
        assert_eq!(StatusRegister::I.bits(), 0x04);
        assert_eq!(StatusRegister::D.bits(), 0x08);
        assert_eq!(StatusRegister::B.bits(), 0x10);
        assert_eq!(StatusRegister::UNUSED.bits(), 0x20);
        assert_eq!(StatusRegister::V.bits(), 0x40);
        assert_eq!(StatusRegister::N.bits(), 0x80);
    }

    #[test]
    fn multiple_flags() {
        let s = StatusRegister::N | StatusRegister::Z | StatusRegister::C;
        assert_eq!(s.to_byte(), 0x83);
    }
}
