//! Shared input-buffering support for memory-mapped devices that receive a byte stream from a
//! connected transport (e.g. [`Console`](super::console::Console)'s input half, and
//! [`CharDisplay`](super::display::CharDisplay)'s optional keyboard sub-range).
//!
//! This is a direct extraction of the ring/latch/break-key logic those devices need, factored out
//! so it can be shared without either device depending on the other. It has no `Transport`/
//! `TransportRelay` field of its own — the owning device drains its own relay and calls
//! [`push`](InputBuffer::push) once per received byte; this type only decides what happens to a
//! byte once it has one.
//!
//! An `InputBuffer` holds an integral ring buffer of received bytes, plus a single-byte latch
//! register that allows a one-byte lookahead and a way to drain the ring on demand. An optional
//! break key code (e.g. ASCII Ctrl+C) may be configured; when that byte is pushed, the ring is
//! drained, the break key code is latched, and the interrupt flag is set.

use super::ring::Ring;

/// Shared ring/latch/break-key input state for a memory-mapped input device.
pub(crate) struct InputBuffer {
    ring: Ring<u8>,
    latch: u8,
    break_key: Option<u8>,
    interrupt_flag: bool,
}

impl InputBuffer {

    /// Creates a new, empty `InputBuffer` with no break key configured.
    pub fn new() -> Self {
        Self {
            ring: Ring::new(0u8),
            latch: 0,
            break_key: None,
            interrupt_flag: false,
        }
    }

    /// Sets the break key to recognize when a byte is [`push`](InputBuffer::push)ed.
    pub fn set_break_key(&mut self, break_key: u8) {
        self.break_key = Some(break_key);
    }

    /// Data-register read semantics: returns the latch value (if non-zero, clearing it), or the
    /// next buffered byte (if any), or zero. Clears the interrupt flag.
    pub fn read_data(&mut self) -> u8 {
        self.interrupt_flag = false;
        if self.latch != 0 {
            let b = self.latch;
            self.latch = 0;
            b
        } else {
            self.ring.get().unwrap_or(0)
        }
    }

    /// Latch-register read semantics: latches the next buffered byte if nothing is already
    /// latched, then returns the latch value. Clears the interrupt flag.
    pub fn read_latch(&mut self) -> u8 {
        self.interrupt_flag = false;
        if self.latch == 0 {
            self.latch = self.ring.get().unwrap_or(0);
        }
        self.latch
    }

    /// Latch-register write semantics: overwrites the latch, drains the ring, and sets the
    /// interrupt flag if `value` matches the configured break key (clearing it otherwise).
    pub fn write_latch(&mut self, value: u8) {
        self.latch = value;
        self.ring.clear();
        if let Some(break_key) = self.break_key {
            self.interrupt_flag = break_key == value;
        } else {
            self.interrupt_flag = false;
        }
    }

    /// Side-effect-free read of data-register semantics.
    pub fn peek_data(&self) -> u8 {
        if self.latch != 0 {
            self.latch
        } else {
            self.ring.peek().unwrap_or(0)
        }
    }

    /// Side-effect-free read of latch-register semantics.
    pub fn peek_latch(&self) -> u8 {
        self.latch
    }

    /// Accepts one byte drained from the owning device's transport relay. If it matches the
    /// configured break key, the latch is set to it, the ring is drained, and the interrupt flag
    /// is set; otherwise the byte is appended to the ring.
    pub fn push(&mut self, byte: u8) {
        if let Some(break_key) = self.break_key && byte == break_key {
            self.latch = byte;
            self.ring.clear();
            self.interrupt_flag = true;
        } else {
            self.ring.put(byte);
        }
    }

    /// Clears the ring, latch, and interrupt flag. The owning device is responsible for logging
    /// this under its own identity.
    pub fn reset(&mut self) {
        self.ring.clear();
        self.latch = 0;
        self.interrupt_flag = false;
    }

    /// Returns `true` if the break-key interrupt condition is currently asserted.
    pub fn irq_active(&self) -> bool {
        self.interrupt_flag
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::ring::RING_CAPACITY;

    #[test]
    fn read_data_resets_interrupt_flag() {
        let mut buf = InputBuffer::new();
        buf.interrupt_flag = true;
        buf.read_data();
        assert!(!buf.interrupt_flag, "expected interrupt flag reset");
    }

    #[test]
    fn read_data_zero_when_nothing_latched_or_buffered() {
        let mut buf = InputBuffer::new();
        assert_eq!(buf.read_data(), 0);
    }

    #[test]
    fn read_data_prefers_latched_value() {
        let mut buf = InputBuffer::new();
        buf.latch = 0x42;
        assert_eq!(buf.read_data(), 0x42);
        assert_eq!(buf.latch, 0);
    }

    #[test]
    fn read_data_falls_back_to_buffered_value() {
        let mut buf = InputBuffer::new();
        buf.ring.put(0x42);
        assert_eq!(buf.read_data(), 0x42);
        assert_eq!(buf.latch, 0);
    }

    #[test]
    fn read_latch_resets_interrupt_flag() {
        let mut buf = InputBuffer::new();
        buf.interrupt_flag = true;
        buf.read_latch();
        assert!(!buf.interrupt_flag, "expected interrupt flag reset");
    }

    #[test]
    fn read_latch_returns_existing_latch_without_draining_ring() {
        let mut buf = InputBuffer::new();
        buf.latch = 0x42;
        buf.ring.put(0x43);
        assert_eq!(buf.read_latch(), 0x42);
        assert_eq!(buf.latch, 0x42);
        assert!(!buf.ring.is_empty());
    }

    #[test]
    fn read_latch_latches_buffered_value_when_nothing_latched() {
        let mut buf = InputBuffer::new();
        buf.ring.put(0x42);
        assert_eq!(buf.read_latch(), 0x42);
        assert_eq!(buf.latch, 0x42);
    }

    #[test]
    fn read_latch_zero_when_nothing_latched_or_buffered() {
        let mut buf = InputBuffer::new();
        assert_eq!(buf.read_latch(), 0);
    }

    #[test]
    fn write_latch_sets_latch() {
        let mut buf = InputBuffer::new();
        buf.write_latch(0x42);
        assert_eq!(buf.latch, 0x42);
        buf.write_latch(0);
        assert_eq!(buf.latch, 0);
    }

    #[test]
    fn write_latch_clears_ring() {
        let mut buf = InputBuffer::new();
        buf.ring.put(0x42);
        buf.write_latch(0);
        assert!(buf.ring.is_empty(), "expected empty ring");
    }

    #[test]
    fn write_latch_break_key_sets_interrupt_flag() {
        let mut buf = InputBuffer::new();
        buf.set_break_key(0x3);
        buf.write_latch(0x3);
        assert_eq!(buf.latch, 0x3);
        assert!(buf.interrupt_flag, "expected interrupt flag set");
    }

    #[test]
    fn write_latch_non_break_key_clears_interrupt_flag() {
        let mut buf = InputBuffer::new();
        buf.interrupt_flag = true;
        buf.write_latch(0x42);
        assert!(!buf.interrupt_flag, "expected interrupt flag reset");
    }

    #[test]
    fn push_buffers_byte() {
        let mut buf = InputBuffer::new();
        buf.push(0x42);
        assert_eq!(buf.ring.peek(), Some(0x42));
    }

    #[test]
    fn push_break_key_latches_and_sets_interrupt_flag() {
        let mut buf = InputBuffer::new();
        buf.set_break_key(0x3);
        buf.push(0x3);
        assert_eq!(buf.latch, 0x3);
        assert!(buf.interrupt_flag, "expected interrupt flag set");
    }

    #[test]
    fn push_break_key_clears_ring() {
        let mut buf = InputBuffer::new();
        buf.set_break_key(0x3);
        buf.ring.put(0x42);
        buf.ring.put(0x43);
        buf.push(0x3);
        assert!(buf.ring.is_empty(), "expected empty ring");
    }

    #[test]
    fn push_tail_drop_when_ring_full() {
        let mut buf = InputBuffer::new();
        for i in 0..RING_CAPACITY {
            buf.push(i as u8);
        }
        for i in 0..(RING_CAPACITY - 1) {
            assert_eq!(buf.ring.get(), Some(i as u8));
        }
        assert!(buf.ring.is_empty(), "expected empty ring");
    }

    #[test]
    fn peek_data_prefers_latched_value() {
        let mut buf = InputBuffer::new();
        buf.latch = 0x42;
        buf.ring.put(0x43);
        assert_eq!(buf.peek_data(), 0x42);
        assert_eq!(buf.latch, 0x42, "peek must not consume the latch");
    }

    #[test]
    fn peek_data_falls_back_to_buffered_value_without_consuming() {
        let mut buf = InputBuffer::new();
        buf.ring.put(0x42);
        assert_eq!(buf.peek_data(), 0x42);
        assert_eq!(buf.peek_data(), 0x42, "peek must not drain the ring");
    }

    #[test]
    fn peek_latch_returns_latch_without_side_effects() {
        let mut buf = InputBuffer::new();
        buf.latch = 0x42;
        assert_eq!(buf.peek_latch(), 0x42);
        assert_eq!(buf.latch, 0x42);
    }

    #[test]
    fn reset_clears_ring_latch_and_interrupt_flag() {
        let mut buf = InputBuffer::new();
        buf.latch = 0xff;
        buf.ring.put(0x42);
        buf.interrupt_flag = true;
        buf.reset();
        assert_eq!(buf.latch, 0, "reset must clear the latch");
        assert!(buf.ring.is_empty(), "reset must clear the ring");
        assert!(!buf.irq_active(), "reset must clear the interrupt flag");
    }

}
