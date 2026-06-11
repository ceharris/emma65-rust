use super::expr::Operand;

/// A context for evaluating a watch expression.
///
/// This trait is implemented by the emulator to provide safe access to the current state of
/// the machine's registers and flags, as well as idempotent access to system memory and mapped
/// I/O devices.
///
/// The mapping of register names and flag names to the [`super::expr::Operand`] type used to
/// identify registers and flags in the compiler's byte code is handled by a
/// [`super::parser::Mapper`] that is passed to the expression parser. These mappings are
/// architecture-specific.
///
/// Addresses specified as arguments to memory read functions that are larger than the address
/// space of the machine architecture will "wrap around"; i.e. the address argument will be
/// evaluated modulo the size of the address space.
///
pub trait WatchContext {

    /// Reads the contents of a machine register, returning an unsigned value zero-extended
    /// to the width of [`Operand`].
    fn read_register_u32(&self, register_id: Operand) -> Operand;

    /// Reads the contents of a machine register, returning a signed value sign-extended
    /// to the width of [`Operand`].
    fn read_register_i32(&self, register_id: Operand) -> Operand;

    /// Reads the state of a machine flag. Returns 1 if the flag is set, else 0.
    fn read_flag(&self, flag_id: Operand) -> Operand;

    /// Performs an idempotent memory read at the given address, returning an unsigned value
    /// zero-extended to the width of [`Operand`].
    ///
    /// `width` is the number of bytes to read (1, 2, or 4).
    fn read_mem_u32(&self, addr: u16, width: u8) -> u32;

    /// Performs an idempotent memory read at the given address, returning a signed value
    /// sign-extended to the width of [`Operand`].
    ///
    /// `width` is the number of bytes to read (1, 2, or 4).
    fn read_mem_i32(&self, addr: u16, width: u8) -> u32;
}