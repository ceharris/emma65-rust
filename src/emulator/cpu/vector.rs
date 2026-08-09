//! VPB (vector-pull) resolution: lets external hardware substitute a
//! different vector address during a RESET/NMI/IRQ/BRK vector fetch.

use crate::emulator::bus::InterruptController;

/// Resolves the effective bus address for a vector fetch (RESET @ 0xFFFC,
/// NMI @ 0xFFFA, or IRQ/BRK @ 0xFFFE) — models the WDC65C02's VPB pin, which
/// external hardware (e.g. a priority interrupt controller) can use to
/// intercept the fetch and substitute a different vector address.
pub trait VectorResolver: Send {
    /// Returns the effective vector address to read from, given the nominal
    /// `vector_addr` and a read-only view of the CPU's `InterruptController`
    /// (e.g. so a priority interrupt controller can rank currently-active
    /// sources via [`InterruptController::active_sources_mask`]).
    /// [`IdentityVectorResolver`] ignores `interrupts` and returns `vector_addr` unchanged.
    fn resolve(&self, vector_addr: u16, interrupts: &InterruptController) -> u16;

    /// Returns a bitmask of IRQ sources (bit `n` corresponds to `IrqSource(n)`) that the
    /// CPU should consider when deciding whether an IRQ is pending. The CPU ANDs this
    /// against [`InterruptController::active_sources_mask`] instead of using the raw mask
    /// directly, so a source with its bit clear here can assert the physical IRQ line
    /// (visible via [`InterruptController::irq_active`]) without the CPU ever recognizing
    /// or servicing it — e.g. a priority interrupt controller masking a source out of its
    /// own vector table.
    ///
    /// Defaults to `u64::MAX` (every source recognized), preserving today's "any active
    /// source is recognized" behavior for resolvers — like [`IdentityVectorResolver`] —
    /// that don't override it. Only gates IRQ recognition; RESET and NMI are unaffected.
    fn irq_mask(&self) -> u64 {
        u64::MAX
    }
}

/// Default [`VectorResolver`]: returns the nominal vector address unchanged,
/// modeling a system with no external vector-pull hardware.
#[derive(Debug, Default, Clone, Copy)]
pub struct IdentityVectorResolver;

impl VectorResolver for IdentityVectorResolver {
    fn resolve(&self, vector_addr: u16, _interrupts: &InterruptController) -> u16 {
        vector_addr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_resolver_returns_input_unchanged() {
        let resolver = IdentityVectorResolver;
        let interrupts = InterruptController::new();
        assert_eq!(resolver.resolve(0xFFFC, &interrupts), 0xFFFC);
        assert_eq!(resolver.resolve(0xFFFA, &interrupts), 0xFFFA);
        assert_eq!(resolver.resolve(0xFFFE, &interrupts), 0xFFFE);
    }

    #[test]
    fn identity_resolver_masks_no_irq_sources() {
        assert_eq!(IdentityVectorResolver.irq_mask(), u64::MAX);
    }
}
