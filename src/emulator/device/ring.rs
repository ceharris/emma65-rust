
/// Capacity of the ring (number of elements); must be a power of two.
pub const RING_CAPACITY: usize = 128;      // any smallish power of two

/// A ring buffer of fixed capacity.
pub struct Ring<T> {
    buf: [T; RING_CAPACITY],
    head: usize,
    tail: usize,
}

impl<T: Copy> Ring<T> {

    /// Initializes a new empty ring.
    pub fn new(init_value: T) -> Self {
        Self {
            buf: [init_value; RING_CAPACITY],
            head: 0,
            tail: 0,
        }
    }

    /// Puts `value` at the tail of the ring if space is available.
    /// Returns `true` if and only if `value` was placed in the ring.
    pub fn put(&mut self, value: T) -> bool {
        let next = (self.tail + 1) & (RING_CAPACITY - 1);
        if next != self.head {
            self.buf[self.tail] = value;
            self.tail = next;
            true
        } else {
            false
        }
    }

    /// Gets the value at the head of the ring (if any), removing it from the ring.
    pub fn get(&mut self) -> Option<T> {
        if !self.is_empty() {
            let b = self.buf[self.head];
            self.head = (self.head + 1) & (RING_CAPACITY - 1);
            Some(b)
        } else {
            None
        }
    }

    /// Peeks at the value at the head of the ring (if any), without removing it.
    pub fn peek(&self) -> Option<T> {
        if !self.is_empty() {
            Some(self.buf[self.head])
        } else {
            None
        }
    }

    /// Tests whether the ring is empty.
    pub fn is_empty(&self) -> bool {
        self.head == self.tail
    }

    /// Clears all values from the ring.
    pub fn clear(&mut self) {
        self.head = self.tail;
    }

}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn new_ring_is_empty() {
        let ring = Ring::new(0);
        assert!(ring.is_empty(), "expected new ring to be empty");
    }

    #[test]
    fn not_empty_after_put() {
        let mut ring = Ring::new(0);
        assert!(ring.put(42), "expected ring to accept offered value");
        assert!(!ring.is_empty(), "expected non-empty ring after put");
    }

    #[test]
    fn get_put_value() {
        let mut ring = Ring::new(0);
        ring.put(42);
        assert!(matches!(ring.get(), Some(42)), "expected to get the value put");
    }

    #[test]
    fn get_in_order_of_put() {
        let mut ring = Ring::new(0);
        assert!(ring.put(42), "expected ring to accept offered value");
        assert!(ring.put(43), "expected ring to accept offered value");
        assert!(matches!(ring.get(), Some(42)), "expected to get the first value put");
        assert!(matches!(ring.get(), Some(43)), "expected to get the next value put");
    }

    #[test]
    fn peek_does_not_remove_value() {
        let mut ring = Ring::new(0);
        assert!(ring.put(42), "expected ring to accept offered value");
        assert!(ring.put(43), "expected ring to accept offered value");
        assert!(matches!(ring.peek(), Some(42)), "expected to get the first value put");
        assert!(matches!(ring.peek(), Some(42)), "expected same value");
    }

    #[test]
    fn empty_after_clear() {
        let mut ring = Ring::new(0);
        assert!(ring.put(42), "expected ring to accept offered value");
        ring.clear();
        assert!(ring.is_empty(), "expected empty ring");
    }

}
