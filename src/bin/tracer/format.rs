//! Fixed-column text formatting for decoded trace output.
//!
//! Column positions are reverse-engineered from the worked examples in
//! issue #268 and are not derivable from any single width constant, so
//! each field is placed at its own absolute column via [`Row::put`].

use emma65::emulator::{DisassembledLine, Disassembler, Registers, StatusRegister};

/// Header line labeling each column.
pub const HEADER: &str =
    "   Seq#    Cyc A  X  Y  S  P  NV-BDIZC Addr   Code    Instruction / Operation";
/// Separator line printed immediately below [`HEADER`].
pub const SEPARATOR: &str =
    "---------- --- -- -- -- -- -- -------- ---- -------- -------------------------";

const CYC_COL: usize = 11;
const A_COL: usize = 15;
const X_COL: usize = 18;
const Y_COL: usize = 21;
const S_COL: usize = 24;
const P_COL: usize = 27;
const FLAGS_COL: usize = 30;
const ADDR_COL: usize = 39;
const CODE_COL: usize = 44;
const MNEMONIC_COL: usize = 54;
const OPERAND_COL: usize = 58;
const COMMENT_COL: usize = 66;

/// A single output line under construction, addressed by absolute byte column.
struct Row(Vec<u8>);

impl Row {
    fn new() -> Self {
        Row(Vec::new())
    }

    /// Writes `text` starting at `col`, padding with spaces as needed to reach it.
    /// Never shortens the row: writing an empty string is a no-op past the pad.
    fn put(&mut self, col: usize, text: &str) {
        while self.0.len() < col {
            self.0.push(b' ');
        }
        for (i, b) in text.bytes().enumerate() {
            let idx = col + i;
            if idx < self.0.len() {
                self.0[idx] = b;
            } else {
                self.0.push(b);
            }
        }
    }

    fn into_string(self) -> String {
        String::from_utf8(self.0).expect("row contents are always ASCII")
    }
}

/// Renders the `NV-BDIZC` flags field: each header character if its bit is
/// set, otherwise a space. The `-` position (the UNUSED bit, always set on
/// real hardware) is never printed — it carries no information.
fn flags_field(p: StatusRegister) -> String {
    const FLAGS: [(char, StatusRegister); 7] = [
        ('N', StatusRegister::N),
        ('V', StatusRegister::V),
        ('B', StatusRegister::B),
        ('D', StatusRegister::D),
        ('I', StatusRegister::I),
        ('Z', StatusRegister::Z),
        ('C', StatusRegister::C),
    ];
    let mut s: String = FLAGS.iter().map(|&(c, bit)| if p.contains(bit) { c } else { ' ' }).collect();
    s.insert(2, ' ');
    s
}

/// Formats a label line: the label left-aligned at the address column, followed by `:`.
pub fn label_line(label: &str) -> String {
    let mut row = Row::new();
    row.put(ADDR_COL, label);
    let mut s = row.into_string();
    s.push(':');
    s
}

/// Formats one instruction row: sequence number, cycle count, registers,
/// flags, and the disassembled instruction's address/code/mnemonic/operand/comment.
pub fn instruction_row(seq: u64, cycles: Option<u8>, regs: &Registers, line: &DisassembledLine) -> String {
    let mut row = Row::new();
    row.put(0, &format!("{seq:>10}"));
    if let Some(c) = cycles {
        row.put(CYC_COL, &format!("{c:>3}"));
    }
    row.put(A_COL, &format!("{:02X}", regs.a));
    row.put(X_COL, &format!("{:02X}", regs.x));
    row.put(Y_COL, &format!("{:02X}", regs.y));
    row.put(S_COL, &format!("{:02X}", regs.s));
    row.put(P_COL, &format!("{:02X}", regs.p.to_byte()));
    row.put(FLAGS_COL, &flags_field(regs.p));
    row.put(ADDR_COL, &format!("{:04X}", line.addr));
    let code = line.raw_bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ");
    row.put(CODE_COL, &code);
    // Left unpadded: `Row::put`'s own gap-fill positions the operand at
    // `OPERAND_COL` regardless of the mnemonic's length (3 or 4 letters).
    // Padding it here would leave a trailing space after operand-less
    // (implied-mode) mnemonics like `NOP`.
    row.put(MNEMONIC_COL, &line.mnemonic.to_string());
    match &line.comment_text {
        Some(comment) => {
            row.put(OPERAND_COL, &format!("{:<8}", line.operand_text));
            row.put(COMMENT_COL, comment);
        }
        None if !line.operand_text.is_empty() => row.put(OPERAND_COL, &line.operand_text),
        None => {}
    }
    row.into_string()
}

/// Formats one verbose bus-op sub-line: address, `RD`/`WR`, hex value, and
/// the same alternate-representation comment used for immediate operands.
pub fn bus_op_row(addr: u16, op: &str, value: u8) -> String {
    // `op` ("RD"/"WR") lands in the Code field's first byte slot; `value`
    // lands in its third byte slot ("XX XX XX" -> byte 3 starts at +6).
    const VALUE_COL: usize = CODE_COL + 6;
    let mut row = Row::new();
    row.put(ADDR_COL, &format!("{addr:04X}"));
    row.put(CODE_COL, op);
    row.put(VALUE_COL, &format!("{value:02X}"));
    row.put(COMMENT_COL, &Disassembler::immediate_mode_comment(value));
    row.into_string()
}
