//! VPB (vector-pull) resolution: lets external hardware substitute a
//! different vector address during a RESET/NMI/IRQ/BRK vector fetch.

/// Resolves the effective bus address for a vector fetch (RESET @ 0xFFFC,
/// NMI @ 0xFFFA, or IRQ/BRK @ 0xFFFE) — models the WDC65C02's VPB pin, which
/// external hardware (e.g. a priority interrupt controller) can use to
/// intercept the fetch and substitute a different vector address.
pub trait VectorResolver: Send {
    /// Returns the effective vector address to read from, given the nominal
    /// `vector_addr`. [`IdentityVectorResolver`] returns it unchanged.
    fn resolve(&self, vector_addr: u16) -> u16;
}

/// Default [`VectorResolver`]: returns the nominal vector address unchanged,
/// modeling a system with no external vector-pull hardware.
#[derive(Debug, Default, Clone, Copy)]
pub struct IdentityVectorResolver;

impl VectorResolver for IdentityVectorResolver {
    fn resolve(&self, vector_addr: u16) -> u16 {
        vector_addr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_resolver_returns_input_unchanged() {
        let resolver = IdentityVectorResolver;
        assert_eq!(resolver.resolve(0xFFFC), 0xFFFC);
        assert_eq!(resolver.resolve(0xFFFA), 0xFFFA);
        assert_eq!(resolver.resolve(0xFFFE), 0xFFFE);
    }
}
