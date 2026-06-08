use crate::watch::Operand;

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
/// Addresses specified as arguments to memory fetch functions that are larger than the address
/// space of the machine architecture will "wrap around"; i.e. the address argument will be
/// evaluated modulo the size of the address space.
///
pub trait WatchContext {

    /// Fetches the contents of a machine register, returning an unsigned value, extended to the
    /// width of [super::expr::Operand].
    ///
    /// # Arguments
    /// `register_id` - ID of the register to fetch
    ///
    fn fetch_register(&self, register_id: Operand) -> Operand;

    /// Fetches the contents of a machine register, returning a signed value, sign-extended to the
    /// width of [super::expr::Operand].
    ///
    /// # Arguments
    /// `register_id` - ID of the register to fetch
    ///
    fn fetch_register_signed(&self, register_id: Operand) -> Operand;

    /// Fetches the state of a machine flag. Returns 1 if the flag is set, else 0.
    ///
    /// # Arguments
    /// `flag_id` - ID of the flag to fetch
    ///
    fn fetch_flag(&self, flag_id: Operand) -> Operand;

    /// Performs an idempotent fetch at the given memory address, returning an unsigned byte
    /// extended to the width of [super::expr::Operand].
    ///
    /// # Arguments
    /// `address` - the subject address
    ///
    fn fetch_byte(&self, address: Operand) -> Operand;

    /// Performs an idempotent fetch at the given memory address, returning a signed byte,
    /// sign-extended to the width of [super::expr::Operand].
    ///
    /// # Arguments
    /// `address` - the subject address
    ///
    fn fetch_byte_signed(&self, address: Operand) -> Operand;

    /// Performs an idempotent fetch at the given memory address, returning an unsigned word
    /// (two consecutive bytes in the architecture's byte order) extended to the width of
    /// [super::expr::Operand].
    ///
    /// # Arguments
    /// `address` - the subject address
    ///
    fn fetch_word(&self, address: Operand) -> Operand;

    /// Performs an idempotent fetch at the given memory address, returning a signed word
    /// (two consecutive bytes in the architecture's byte order) sign-extended to the width of
    /// [super::expr::Operand].
    ///
    /// # Arguments
    /// `address` - the subject address
    ///
    fn fetch_word_signed(&self, address: Operand) -> Operand;

    /// Performs an idempotent fetch at the given memory address, returning an unsigned double word
    /// (four consecutive bytes in the architecture's byte order) extended to the width of
    /// [super::expr::Operand].
    ///
    /// # Arguments
    /// `address` - the subject address
    ///
    fn fetch_dword(&self, address: Operand) -> Operand;

    /// Performs an idempotent fetch at the given memory address, returning a signed double word
    /// (four consecutive bytes in the architecture's byte order) sign-extended to the width of
    /// [super::expr::Operand].
    ///
    /// # Arguments
    /// `address` - the subject address
    ///
    fn fetch_dword_signed(&self, address: Operand) -> Operand;

}