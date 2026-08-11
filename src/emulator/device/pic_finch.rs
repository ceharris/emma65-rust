//! Priority interrupt controller (PIC) device.
//!
//! The PIC ranks up to 8 IRQ priority slots and tells the CPU which per-slot vector to
//! jump to, via [`VectorResolver`]. It occupies a single byte of address space (its
//! Interrupt Enable Register, or IER) — it does **not** claim the 16-byte vector table
//! itself, which is expected to be backed by ROM:
//!
//! | Address           | Contents                                           |
//! |-------------------|-----------------------------------------------------|
//! | `0xFFE0`–`0xFFE1` | Vector for slot 0 (highest priority: `IrqSource(0)`) |
//! | `0xFFE2`–`0xFFE3` | Vector for slot 1 (`IrqSource(1)`)                   |
//! | ...               | ...                                                   |
//! | `0xFFEC`–`0xFFED` | Vector for slot 6 (`IrqSource(6)`)                   |
//! | `0xFFEE`–`0xFFEF` | Vector for slot 7 (fold: `IrqSource(7)`..`IrqSource(63)`, wired-OR) |
//!
//! `IrqSource`'s priority ordering (lower value = higher priority) puts sources 0..6 in
//! their own slots; everything from source 7 up to the crate's maximum of 63 shares the
//! lowest-priority slot 7. The RESET (`0xFFFC`) and NMI (`0xFFFA`) vectors are untouched.
//!
//! The IER's 7 low bits individually enable/disable slots 0..6 for *vector routing
//! purposes*: since [`VectorResolver::irq_mask`] also gates which sources the CPU
//! recognizes as a pending IRQ at all (see that trait method's docs), a disabled slot's
//! source cannot wake the CPU or be routed to its own vector — it is simply not
//! recognized until re-enabled. Sources 7..63 are always recognized and always routed to
//! slot 7; they can only be inhibited by setting the CPU's I flag, matching real PIC
//! hardware where an unprioritized/"wired-OR" tier has no per-source mask.
//!
//! Because the 6502's `0xFFFE` IRQ/BRK vector low byte is never fetched when a PIC is
//! installed (the CPU fetches the PIC-resolved address instead), the IER is typically
//! mapped at `0xFFFF`, the otherwise-unused high byte of that vector.
//!
//! ## IER encoding
//!
//! Reading the IER returns the enable state of slots 0..6 in bits 0..6; bit 7 always
//! reads as 1 (sources 7..63 are always "enabled").
//!
//! Writing the IER treats bit 7 as a set/clear indicator and bits 0..6 as a selection
//! mask: bits set in the written value select which of slots 0..6 to modify, and bit 7
//! determines whether the selected slots are enabled (`1`) or disabled (`0`). Unselected
//! bits are left unchanged. This is the same encoding used by the VIA's IER
//! ([`Via6522`](super::Via6522)).

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::emulator::VectorResolver;
use crate::emulator::bus::InterruptController;
use crate::emulator::cpu::IRQ_VECTOR;
use crate::emulator::{LogCategory, LogLevel, LogSender, log_msg};

/// Base address of the PIC's 8-slot, 16-byte IRQ vector table.
const VECTOR_TABLE_BASE: u16 = 0xFFE0;

/// Slot that IRQ sources 7..63 fold onto (lowest priority, always recognized).
const FOLD_SLOT: u16 = 7;

/// Mask selecting the 7 individually-enableable low bits of the IER (slots 0..6).
const IER_ENABLE_MASK: u8 = 0x7F;

/// Bit 7 of an IER write selects enable (`1`) vs. disable (`0`) for the selected slots.
const IER_SET_FLAG: u8 = 0x80;

/// Default state of the IER after reset
const DEFAULT_IER_STATE: u8 = IER_ENABLE_MASK;

/// A priority interrupt controller: an [`IoDevice`](super::IoDevice) exposing a single
/// Interrupt Enable Register, paired with a [`VectorResolver`] implementation that ranks
/// active IRQ sources into an 8-slot vector table. See the module docs for the register
/// and vector table layout.
///
/// The IER is held behind an `Arc`, so [`PicFinch::vector_resolver`] can hand out a
/// [`PicFinchVectorResolver`] that shares this `PicFinch`'s enable state — needed because
/// `BusConfig::device` and `BusConfig::vector_resolver` each take ownership of their own
/// boxed trait object, and both ultimately end up owned by different fields of the `Cpu`
/// (the bus's device list and the CPU's own resolver slot, respectively).
pub struct PicFinch {
    name: &'static str,
    /// Bus address of the IER register.
    address: u16,
    /// Enable state of slots 0..6, one bit per slot (bit 7 unused; always reads as 1).
    /// Shared with any [`PicFinchVectorResolver`] handed out via [`PicFinch::vector_resolver`].
    ier: Arc<AtomicU8>,
    /// Sender for structured diagnostic messages (e.g. `reset()`).
    log_sender: LogSender,
}

impl PicFinch {
    /// Creates a new `PicFinch` with all slots disabled and no IER address assigned.
    pub fn new(name: &'static str) -> Self {
        Self { name, address: 0, ier: Arc::new(AtomicU8::new(DEFAULT_IER_STATE)), log_sender: LogSender::default() }
    }

    /// Sets the bus address of the IER register.
    pub fn with_address(mut self, address: u16) -> Self {
        self.address = address;
        self
    }

    /// Installs a log sender for diagnostic messages (e.g. `reset()`).
    pub fn set_log_sender(&mut self, sender: LogSender) {
        self.log_sender = sender;
    }

    /// Returns a [`VectorResolver`] that shares this `PicFinch`'s IER state, suitable for
    /// installing on the `Cpu` (e.g. via `BusConfig::vector_resolver`) alongside
    /// registering this `PicFinch` itself as a bus device (e.g. via `BusConfig::device`).
    pub fn vector_resolver(&self) -> PicFinchVectorResolver {
        PicFinchVectorResolver { ier: Arc::clone(&self.ier) }
    }
}

impl super::IoDevice for PicFinch {
    fn read(&mut self, address: u16) -> u8 {
        self.peek(address)
    }

    fn write(&mut self, _address: u16, value: u8) {
        write_ier(&self.ier, value);
    }

    fn peek(&self, _address: u16) -> u8 {
        read_ier(&self.ier)
    }

    fn reset(&mut self) {
        self.ier.store(DEFAULT_IER_STATE, Ordering::Relaxed);
        log_msg!(self.log_sender, LogLevel::Info, LogCategory::Device, "{} @0x{:04x} reset", self.name(), self.address);
    }

    fn name(&self) -> &str {
        self.name
    }
}

impl VectorResolver for PicFinch {
    fn resolve(&self, vector_addr: u16, interrupts: &InterruptController) -> u16 {
        resolve_vector(self.ier.load(Ordering::Relaxed), vector_addr, interrupts)
    }

    fn irq_mask(&self) -> u64 {
        irq_mask_for(self.ier.load(Ordering::Relaxed))
    }
}

/// A [`VectorResolver`] sharing a [`PicFinch`]'s IER state, obtained via [`PicFinch::vector_resolver`].
/// See that method's docs for why this exists as a separate type from `PicFinch` itself.
pub struct PicFinchVectorResolver {
    ier: Arc<AtomicU8>,
}

impl VectorResolver for PicFinchVectorResolver {
    fn resolve(&self, vector_addr: u16, interrupts: &InterruptController) -> u16 {
        resolve_vector(self.ier.load(Ordering::Relaxed), vector_addr, interrupts)
    }

    fn irq_mask(&self) -> u64 {
        irq_mask_for(self.ier.load(Ordering::Relaxed))
    }
}

/// Applies an IER write (see the module docs' "IER encoding" section) to `ier`.
fn write_ier(ier: &AtomicU8, value: u8) {
    let selected = value & IER_ENABLE_MASK;
    if value & IER_SET_FLAG != 0 {
        ier.fetch_or(selected, Ordering::Relaxed);
    } else {
        ier.fetch_and(!selected, Ordering::Relaxed);
    }
}

/// Reads `ier` with bit 7 forced to 1, as documented for IER reads.
fn read_ier(ier: &AtomicU8) -> u8 {
    ier.load(Ordering::Relaxed) | IER_SET_FLAG
}

/// Shared routing logic for [`PicFinch`] and [`PicFinchVectorResolver`]: resolves `vector_addr`
/// against the given IER byte and interrupt controller state.
fn resolve_vector(ier: u8, vector_addr: u16, interrupts: &InterruptController) -> u16 {
    if vector_addr != IRQ_VECTOR {
        return vector_addr;
    }
    let recognized = interrupts.active_sources_mask() & irq_mask_for(ier);
    let low_slots = recognized & IER_ENABLE_MASK as u64;
    let slot = if low_slots != 0 { low_slots.trailing_zeros() as u16 } else { FOLD_SLOT };
    VECTOR_TABLE_BASE + slot * 2
}

/// Shared mask logic for [`PicFinch`] and [`PicFinchVectorResolver`]: bits 0..6 reflect `ier`'s
/// enable state, bits 7..63 (sources with no per-source mask) are always set.
fn irq_mask_for(ier: u8) -> u64 {
    (ier as u64 & IER_ENABLE_MASK as u64) | !(IER_ENABLE_MASK as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::bus::IrqSource;
    use crate::emulator::device::IoDevice;

    const IER_ADDR: u16 = 0xFFFF;

    fn pic_finch() -> PicFinch {
        PicFinch::new("pic").with_address(IER_ADDR)
    }

    fn with_active(sources: &[u32]) -> InterruptController {
        let mut ctrl = InterruptController::new();
        for &s in sources {
            ctrl.assert_irq(IrqSource(s));
        }
        ctrl
    }

    // --- IER register ---

    #[test]
    fn new_ier_reads_with_bit7_set_and_all_slots_enabled() {
        assert_eq!(pic_finch().peek(IER_ADDR), 0xFF);
    }

    #[test]
    fn write_with_bit7_set_enables_selected_slots() {
        let mut dev = pic_finch();
        dev.write(IER_ADDR, 0x7F); // disable all
        dev.write(IER_ADDR, 0x85); // select slots 0, 2; enable
        assert_eq!(dev.peek(IER_ADDR), 0x85);
    }

    #[test]
    fn write_with_bit7_clear_disables_selected_slots() {
        let mut dev = pic_finch();
        dev.write(IER_ADDR, 0x05); // select slots 0, 2; disable
        assert_eq!(dev.peek(IER_ADDR), 0x80 | (0x7F & !0x05));
    }

    #[test]
    fn write_leaves_unselected_slots_unchanged() {
        let mut dev = pic_finch();
        dev.write(IER_ADDR, 0x7E); // disable all but slot 0
        dev.write(IER_ADDR, 0x84); // enable slot 2, slot 0 untouched
        assert_eq!(dev.peek(IER_ADDR), 0x80 | 0x05);
    }

    #[test]
    fn reset_enables_all_slots() {
        let mut dev = pic_finch();
        dev.write(IER_ADDR, 0x7F);
        dev.reset();
        assert_eq!(dev.peek(IER_ADDR), 0xFF);
    }

    #[test]
    fn reset_logs_device_message() {
        let (sender, rx) = crate::emulator::logging::test_channel_sender(4);
        let mut dev = pic_finch();
        dev.set_log_sender(sender);
        dev.reset();
        let received = rx.recv().unwrap();
        assert_eq!(received.category, LogCategory::Device);
        assert_eq!(received.message, format!("pic @0x{IER_ADDR:04x} reset"));
    }

    // --- irq_mask ---

    #[test]
    fn irq_mask_reflects_enabled_low_slots() {
        let mut dev = pic_finch();
        dev.write(IER_ADDR, 0x7A); // disable all but slots 0, 2
        assert_eq!(dev.irq_mask() & 0x7F, 0x05);
    }

    #[test]
    fn irq_mask_always_recognizes_high_sources() {
        let dev = pic_finch(); // all low slots disabled
        assert_eq!(dev.irq_mask() & !0x7Fu64, !0x7Fu64);
    }

    // --- resolve ---

    #[test]
    fn resolve_passes_through_reset_and_nmi_vectors() {
        let dev = pic_finch();
        let ctrl = InterruptController::new();
        assert_eq!(dev.resolve(0xFFFC, &ctrl), 0xFFFC);
        assert_eq!(dev.resolve(0xFFFA, &ctrl), 0xFFFA);
    }

    #[test]
    fn resolve_routes_enabled_low_source_to_its_own_slot() {
        let mut dev = pic_finch();
        dev.write(IER_ADDR, 0x80 | (1 << 3)); // enable slot 3
        let ctrl = with_active(&[3]);
        assert_eq!(dev.resolve(IRQ_VECTOR, &ctrl), VECTOR_TABLE_BASE + 3 * 2);
    }

    #[test]
    fn resolve_picks_highest_priority_among_enabled_active_low_sources() {
        let mut dev = pic_finch();
        dev.write(IER_ADDR, 0xFF); // enable all low slots
        let ctrl = with_active(&[5, 1, 3]);
        assert_eq!(dev.resolve(IRQ_VECTOR, &ctrl), VECTOR_TABLE_BASE + 2);
    }

    #[test]
    fn resolve_routes_high_source_to_fold_slot() {
        let dev = pic_finch();
        let ctrl = with_active(&[40]);
        assert_eq!(dev.resolve(IRQ_VECTOR, &ctrl), VECTOR_TABLE_BASE + FOLD_SLOT * 2);
    }

    #[test]
    fn resolve_prefers_enabled_low_source_over_high_source() {
        let mut dev = pic_finch();
        dev.write(IER_ADDR, 0x80 | (1 << 4)); // enable slot 4
        let ctrl = with_active(&[40, 4]);
        assert_eq!(dev.resolve(IRQ_VECTOR, &ctrl), VECTOR_TABLE_BASE + 4 * 2);
    }

    #[test]
    fn resolve_ignores_disabled_low_source_even_if_active() {
        let mut dev = pic_finch();
        dev.write(IER_ADDR, 0x04);
        // slot 2 disabled; only a high source is active alongside it
        let ctrl = with_active(&[2, 40]);
        assert_eq!(dev.resolve(IRQ_VECTOR, &ctrl), VECTOR_TABLE_BASE + FOLD_SLOT * 2);
    }

    // --- vector_resolver ---

    #[test]
    fn vector_resolver_sees_ier_writes_made_through_the_device() {
        let mut dev = pic_finch();
        let resolver = dev.vector_resolver();
        dev.write(IER_ADDR, 0x80 | (1 << 5)); // enable slot 5
        let ctrl = with_active(&[5]);
        assert_eq!(resolver.resolve(IRQ_VECTOR, &ctrl), VECTOR_TABLE_BASE + 5 * 2);
    }

    #[test]
    fn vector_resolver_irq_mask_tracks_device_state() {
        let mut dev = pic_finch();
        let resolver = dev.vector_resolver();
        dev.write(IER_ADDR, !(1 << 6) & 0x7F); // disable all but slot 6
        assert_eq!(resolver.irq_mask() & 0x7F, 1 << 6);
    }
}
