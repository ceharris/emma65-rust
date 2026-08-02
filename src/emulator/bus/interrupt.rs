//! IRQ and NMI interrupt controller.
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
    nmi_pending: bool,
    next_irq_source: u32,
}

impl InterruptController {
    /// Creates a new controller with no active interrupts.
    pub fn new() -> Self {
        Self {
            irq_sources: 0,
            nmi_pending: false,
            next_irq_source: 0,
        }
    }

    /// Allocates the next IRQ source
    pub fn alloc_irq_source(&mut self) -> IrqSource {
        assert_ne!(self.next_irq_source, MAX_IRQ_SOURCES, "too many interrupt sources");
        let source = IrqSource(self.next_irq_source);
        self.next_irq_source += 1;
        source
    }

    /// Asserts the IRQ line from `source`. The line remains active until all sources release it.
    pub fn assert_irq(&mut self, source: IrqSource) {
        self.irq_sources |= 1u64 << source.0;
    }

    /// Releases `source` from the IRQ line. If no other sources are asserting, the line goes low.
    pub fn release_irq(&mut self, source: IrqSource) {
        self.irq_sources &= !(1u64 << source.0);
    }

    /// Returns `true` if any source is currently asserting the IRQ line.
    pub fn irq_active(&self) -> bool {
        self.irq_sources != 0
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

    /// Syncs device IRQ states into the source bitmask. Called by the CPU after each instruction.
    ///
    /// Each `(id, active)` pair asserts or releases the corresponding `IrqSource`'s bit.
    ///
    /// Panics if any `id.0 >= 64`; see [`assert_irq`](Self::assert_irq).
    pub fn poll_devices(&mut self, states: impl Iterator<Item = (DeviceId, bool)>) {
        for (id, active) in states {
            let bit = 1u64 << Self::bit_index(IrqSource::from(id));
            if active {
                self.irq_sources |= bit;
            } else {
                self.irq_sources &= !bit;
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
    fn irq_source_from_device_id() {
        let src = IrqSource::from(DeviceId(42));
        assert_eq!(src, IrqSource(42));
    }
}
