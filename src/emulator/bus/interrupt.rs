//! Interrupt controller.
//!
//! The interrupt controller handles signaling and recognition of the three interrupt
//! sources defined for the 6502: RESET, NMI, and IRQ.
//! 
//! Tne interrupt controller plays a crucial role in identifying interrupt sources
//! during CPU step execution. As such the [`poll_devices`](InterruptController::poll_devices)
//! method, which is called for each instruction executed, must be especially
//! efficient. This implementation uses a bit set (in a `u64`) to track devices that
//! are currently asserting IRQ.
//!
//! To emulate the level-triggered nature of the CPU's IRQ signal, devices assert or
//! release IRQ (based on their internal state) using [`assert_irq`](InterruptController::assert_irq)
//! and [`release_irq`](InterruptController::release_irq), respectively. These methods simply
//! set or clear a bit in the bit set representing active IRQ sources. The CPU's IRQ signal
//! reads as asserted whenever the bit set is non-empty (i.e. the underlying `u64` is not zero).
//!
//! A source that wants "trigger exactly one IRQ" semantics (rather than a sustained
//! level requiring an explicit release) can use
//! [`assert_irq_pulse`](InterruptController::assert_irq_pulse) instead of `assert_irq`.
//! The CPU calls [`consume_irq_pulses`](InterruptController::consume_irq_pulses) once an
//! IRQ has been serviced, which releases any sources currently asserted in pulse mode.
//!
use crate::emulator::device::DeviceId;

/// Identifies a source of IRQ, mapped from the `DeviceId` of the asserting device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IrqSource(pub u32);


impl From<DeviceId> for IrqSource {
    fn from(id: DeviceId) -> Self {
        IrqSource(id.0)
    }
}

/// Maximum number of IRQ sources
pub const MAX_IRQ_SOURCES: u32 = 64;

/// Tracks the state of the IRQ line (level-triggered, multi-source) and NMI (edge-triggered).
pub struct InterruptController {
    irq_sources: u64,
    /// Subset of `irq_sources` asserted via [`assert_irq_pulse`](InterruptController::assert_irq_pulse);
    /// cleared from `irq_sources` the next time an IRQ is serviced.
    irq_pulse_sources: u64,
    nmi_pending: bool,
    reset_pending: bool,
}

impl InterruptController {
    /// Creates a new controller with no active interrupts.
    pub fn new() -> Self {
        Self {
            irq_sources: 0,
            irq_pulse_sources: 0,
            nmi_pending: false,
            reset_pending: false,
        }
    }

    /// Asserts the IRQ line from `source`. The line remains active until all sources release it.
    pub fn assert_irq(&mut self, source: IrqSource) {
        self.irq_sources |= 1u64 << source.0;
    }

    /// Asserts the IRQ line from `source` in one-shot ("pulse") mode: `source` is
    /// automatically released the next time an IRQ is serviced, rather than requiring
    /// an explicit [`release_irq`](InterruptController::release_irq) call.
    ///
    /// Used to give a level-triggered source "trigger exactly one IRQ" semantics —
    /// e.g. a UI control asserting IRQ while the CPU is free-running, where a sustained
    /// level would otherwise re-service the same interrupt on every instruction after
    /// `RTI` until manually released.
    pub fn assert_irq_pulse(&mut self, source: IrqSource) {
        let bit = 1u64 << source.0;
        self.irq_sources |= bit;
        self.irq_pulse_sources |= bit;
    }

    /// Releases `source` from the IRQ line. If no other sources are asserting, the line goes low.
    /// Also clears any pending one-shot state for `source`.
    pub fn release_irq(&mut self, source: IrqSource) {
        let mask = !(1u64 << source.0);
        self.irq_sources &= mask;
        self.irq_pulse_sources &= mask;
    }

    /// Returns `true` if any source is currently asserting the IRQ line.
    pub fn irq_active(&self) -> bool {
        self.irq_sources != 0
    }

    /// Releases all sources currently asserted in pulse mode. Called once an IRQ has
    /// been serviced (i.e. the CPU has entered the ISR), so each pulsed source produces
    /// exactly one interrupt regardless of how long the line would otherwise remain
    /// asserted.
    pub fn consume_irq_pulses(&mut self) {
        self.irq_sources &= !self.irq_pulse_sources;
        self.irq_pulse_sources = 0;
    }

    /// Latches a pending NMI. Called on the falling edge of the NMI line.
    pub fn signal_nmi(&mut self) {
        self.nmi_pending = true;
    }

    /// Consumes and clears the pending NMI flag. Returns `true` if an NMI was pending.
    pub fn take_nmi(&mut self) -> bool {
        let pending = self.nmi_pending;
        self.nmi_pending = false;
        pending
    }

    /// Returns `true` if an NMI is pending and has not yet been serviced.
    pub fn nmi_pending(&self) -> bool {
        self.nmi_pending
    }

    /// Latches a pending RESET. Called on the falling edge of the RESET line.
    pub fn signal_reset(&mut self) {
        self.reset_pending = true;
    }

    /// Consumes and clears the pending RESET flag. Returns `true` if RESET was pending.
    pub fn take_reset(&mut self) -> bool {
        let pending = self.reset_pending;
        self.reset_pending = false;
        pending
    }

    /// Returns `true` if RESET is pending and has not yet been serviced.
    pub fn reset_pending(&self) -> bool {
        self.reset_pending
    }

    /// Syncs device IRQ states into the source bitmask. Called by the CPU after each instruction.
    pub fn poll_devices(&mut self, states: impl Iterator<Item = (DeviceId, bool)>) {
        for (id, active) in states {
            if id.0 < MAX_IRQ_SOURCES {
                let bit = 1u64 << Self::bit_index(IrqSource::from(id));
                if active {
                    self.irq_sources |= bit;
                } else {
                    self.irq_sources &= !bit;
                }
            }
        }
    }

    /// Validates and extracts the bit index for `source`.
    fn bit_index(source: IrqSource) -> u32 {
        assert!(
            source.0 < 64,
            "IrqSource({}) is out of range: InterruptController supports IrqSource values 0..64",
            source.0
        );
        source.0
    }

}

impl Default for InterruptController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irq_inactive_by_default() {
        assert!(!InterruptController::new().irq_active());
    }

    #[test]
    fn assert_makes_irq_active() {
        let mut ctrl = InterruptController::new();
        ctrl.assert_irq(IrqSource(1));
        assert!(ctrl.irq_active());
    }

    #[test]
    fn release_all_sources_clears_irq() {
        let mut ctrl = InterruptController::new();
        ctrl.assert_irq(IrqSource(1));
        ctrl.assert_irq(IrqSource(2));
        ctrl.release_irq(IrqSource(1));
        assert!(ctrl.irq_active());
        ctrl.release_irq(IrqSource(2));
        assert!(!ctrl.irq_active());
    }

    #[test]
    fn release_nonexistent_source_is_harmless() {
        let mut ctrl = InterruptController::new();
        ctrl.release_irq(IrqSource(63));
        assert!(!ctrl.irq_active());
    }

    #[test]
    fn nmi_inactive_by_default() {
        assert!(!InterruptController::new().nmi_pending());
    }

    #[test]
    fn signal_nmi_latches_pending() {
        let mut ctrl = InterruptController::new();
        ctrl.signal_nmi();
        assert!(ctrl.nmi_pending());
    }

    #[test]
    fn take_nmi_returns_true_and_clears() {
        let mut ctrl = InterruptController::new();
        ctrl.signal_nmi();
        assert!(ctrl.take_nmi());
        assert!(!ctrl.nmi_pending());
    }

    #[test]
    fn take_nmi_returns_false_when_not_pending() {
        let mut ctrl = InterruptController::new();
        assert!(!ctrl.take_nmi());
    }

    #[test]
    fn reset_inactive_by_default() {
        assert!(!InterruptController::new().reset_pending());
    }

    #[test]
    fn signal_reset_latches_pending() {
        let mut ctrl = InterruptController::new();
        ctrl.signal_reset();
        assert!(ctrl.reset_pending());
    }

    #[test]
    fn take_reset_returns_true_and_clears() {
        let mut ctrl = InterruptController::new();
        ctrl.signal_reset();
        assert!(ctrl.take_reset());
        assert!(!ctrl.reset_pending());
    }

    #[test]
    fn take_reset_returns_false_when_not_pending() {
        let mut ctrl = InterruptController::new();
        assert!(!ctrl.take_reset());
    }

    #[test]
    fn poll_devices_asserts_active_sources() {
        let mut ctrl = InterruptController::new();
        ctrl.poll_devices([(DeviceId(1), true), (DeviceId(2), false)].into_iter());
        assert!(ctrl.irq_active());
    }

    #[test]
    fn poll_devices_releases_inactive_sources() {
        let mut ctrl = InterruptController::new();
        ctrl.assert_irq(IrqSource(1));
        ctrl.poll_devices([(DeviceId(1), false)].into_iter());
        assert!(!ctrl.irq_active());
    }

    #[test]
    fn poll_devices_syncs_multiple_sources() {
        let mut ctrl = InterruptController::new();
        ctrl.poll_devices([(DeviceId(1), true), (DeviceId(2), true)].into_iter());
        assert!(ctrl.irq_active());
        ctrl.poll_devices([(DeviceId(1), false), (DeviceId(2), false)].into_iter());
        assert!(!ctrl.irq_active());
    }

    #[test]
    fn assert_irq_pulse_makes_irq_active() {
        let mut ctrl = InterruptController::new();
        ctrl.assert_irq_pulse(IrqSource(1));
        assert!(ctrl.irq_active());
    }

    #[test]
    fn consume_irq_pulses_releases_pulsed_source() {
        let mut ctrl = InterruptController::new();
        ctrl.assert_irq_pulse(IrqSource(1));
        ctrl.consume_irq_pulses();
        assert!(!ctrl.irq_active());
    }

    #[test]
    fn consume_irq_pulses_does_not_release_manually_asserted_source() {
        let mut ctrl = InterruptController::new();
        ctrl.assert_irq(IrqSource(1));
        ctrl.consume_irq_pulses();
        assert!(ctrl.irq_active());
    }

    #[test]
    fn consume_irq_pulses_leaves_other_manual_sources_asserted() {
        let mut ctrl = InterruptController::new();
        ctrl.assert_irq(IrqSource(1));
        ctrl.assert_irq_pulse(IrqSource(2));
        ctrl.consume_irq_pulses();
        assert!(ctrl.irq_active());
        ctrl.release_irq(IrqSource(1));
        assert!(!ctrl.irq_active());
    }

    #[test]
    fn release_irq_clears_pulse_state() {
        let mut ctrl = InterruptController::new();
        ctrl.assert_irq_pulse(IrqSource(1));
        ctrl.release_irq(IrqSource(1));
        // If pulse state were left dangling, a later manual assert of a
        // different source would be wrongly consumed as a pulse.
        ctrl.assert_irq(IrqSource(1));
        ctrl.consume_irq_pulses();
        assert!(ctrl.irq_active());
    }

    #[test]
    fn irq_source_from_device_id() {
        let src = IrqSource::from(DeviceId(42));
        assert_eq!(src, IrqSource(42));
    }

    #[test]
    fn poll_devices_does_not_panic_on_non_irq_source() {
        let mut ctrl = InterruptController::new();
        ctrl.poll_devices([(DeviceId(MAX_IRQ_SOURCES), true)].into_iter());
        assert!(!ctrl.irq_active());
    }

}
