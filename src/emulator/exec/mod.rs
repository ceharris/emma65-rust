use crate::emulator::cpu::opcodes::DecodedOp;
use crate::emulator::error::ExecError;
use crate::watch::WatchError;

const UNLIMITED_SENTINEL: u64 = 0;

/// Target clock frequency for the emulated CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockSpeed {
    hz: u64,
}

impl ClockSpeed {
    pub fn mhz(mhz: f64) -> Self {
        Self { hz: (mhz * 1_000_000.0).round() as u64 }
    }

    pub fn hz(hz: u64) -> Self {
        assert!(hz > 0, "hz must be non-zero; use ClockSpeed::unlimited() for no throttling");
        Self { hz }
    }

    pub fn unlimited() -> Self {
        Self { hz: UNLIMITED_SENTINEL }
    }

    pub fn is_unlimited(&self) -> bool {
        self.hz == UNLIMITED_SENTINEL
    }

    pub fn hz_value(&self) -> Option<u64> {
        if self.is_unlimited() { None } else { Some(self.hz) }
    }
}

/// Result returned by `Cpu::step()`.
pub enum StepResult {
    /// Instruction executed normally.
    Executed(DecodedOp),
    /// PC matched a breakpoint; instruction was NOT executed.
    Breakpoint(u16),
    /// A watch expression triggered; instruction was NOT executed.
    WatchTriggered { watch_index: usize, pc: u16 },
    /// A watch expression evaluation failed; instruction was NOT executed.
    WatchError { watch_index: usize, pc: u16, error: WatchError },
    /// CPU is in WAI state, waiting for an interrupt.
    Waiting,
    /// CPU is in STP state; only reset() clears it.
    Stopped,
    /// A fatal execution error occurred.
    Error(ExecError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mhz_converts_to_hz() {
        assert_eq!(ClockSpeed::mhz(1.0).hz_value(), Some(1_000_000));
        assert_eq!(ClockSpeed::mhz(1.8432).hz_value(), Some(1_843_200));
        assert_eq!(ClockSpeed::mhz(2.0).hz_value(), Some(2_000_000));
    }

    #[test]
    fn hz_constructor() {
        assert_eq!(ClockSpeed::hz(1_843_200).hz_value(), Some(1_843_200));
    }

    #[test]
    fn unlimited_sentinel() {
        let s = ClockSpeed::unlimited();
        assert!(s.is_unlimited());
        assert_eq!(s.hz_value(), None);
    }

    #[test]
    fn non_unlimited_is_not_unlimited() {
        assert!(!ClockSpeed::mhz(1.0).is_unlimited());
        assert!(!ClockSpeed::hz(1).is_unlimited());
    }
}
