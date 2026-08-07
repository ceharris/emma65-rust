//! `disassemble` request handling, backed by [`emma65::disasm::Disassembler`].
//!
//! Mirrors the Tauri debugger's `disassembly.rs::get_disassembly`, translated from a Tauri
//! command taking `(addr, count)` to DAP's `disassemble` request, which additionally resolves
//! a `memoryReference`/`offset`/`instructionOffset` triple into a start address — VS Code's
//! Disassembly View uses a negative `instructionOffset` to fetch instructions preceding the
//! current PC when it first opens.

use dap::requests::DisassembleArguments;
use dap::types::DisassembledInstruction;
use emma65::disasm::{DisassembledLine, Disassembler};
use emma65::emulator::Cpu;

/// Parses a DAP `memoryReference` (hex if `0x`/`0X`-prefixed, decimal otherwise).
pub(crate) fn parse_memory_reference(memory_reference: &str) -> Result<i64, String> {
    match memory_reference.strip_prefix("0x").or_else(|| memory_reference.strip_prefix("0X")) {
        Some(hex) => i64::from_str_radix(hex, 16).map_err(|e| format!("invalid memoryReference: {e}")),
        None => memory_reference.parse::<i64>().map_err(|e| format!("invalid memoryReference: {e}")),
    }
}

/// Steps `address` back `n` instructions by resynchronizing the instruction stream: tries each
/// candidate start position in `address - n*3 .. address`, nearest first, disassembling forward
/// until it either lands exactly on `address` (a valid alignment, kept if it covers at least `n`
/// instructions) or overshoots. 6502 instructions are 1-3 bytes, so `n*3` bytes back always
/// contains room for `n` real instructions; like any disassembler reading backwards over a
/// variable-length instruction set, an ambiguous byte stream can still resync on the wrong
/// boundary.
fn step_back_instructions(disasm: &Disassembler, cpu: &Cpu, address: u16, n: usize) -> u16 {
    if n == 0 {
        return address;
    }
    let earliest = address.wrapping_sub((n as u16).saturating_mul(3));
    let mut candidate = address.wrapping_sub(1);
    loop {
        let mut addr = candidate;
        let mut starts = Vec::new();
        while addr != address {
            starts.push(addr);
            let len = disasm.disassemble_one(cpu.bus(), addr).raw_bytes.len() as u16;
            let next = addr.wrapping_add(len);
            if next == address && starts.len() >= n {
                return starts[starts.len() - n];
            }
            if next >= address || next == addr {
                break; // resynced with too few instructions, overshot, or stalled
            }
            addr = next;
        }
        if candidate == earliest {
            return earliest;
        }
        candidate = candidate.wrapping_sub(1);
    }
}

/// Converts a decoded instruction into the DAP wire format.
fn to_dap_instruction(line: &DisassembledLine) -> DisassembledInstruction {
    let instruction_bytes = line.raw_bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ");
    let mut instruction =
        if line.operand_text.is_empty() { line.mnemonic.to_string() } else { format!("{} {}", line.mnemonic, line.operand_text) };
    if let Some(comment) = &line.comment_text {
        instruction.push_str("  ");
        instruction.push_str(comment);
    }
    DisassembledInstruction {
        address: format!("0x{:04X}", line.addr),
        instruction_bytes: Some(instruction_bytes),
        instruction,
        symbol: line.labels.first().cloned(),
        ..Default::default()
    }
}

/// Builds the DAP `disassemble` response for `args`, using side-effect-free `peek` reads off
/// `cpu`'s bus, matching `get_disassembly`'s read semantics.
pub fn disassemble(cpu: &Cpu, args: &DisassembleArguments) -> Result<Vec<DisassembledInstruction>, String> {
    let base = parse_memory_reference(&args.memory_reference)?.wrapping_add(args.offset.unwrap_or(0));
    let address = base as u16;

    let disasm = Disassembler::new(cpu.variant());
    let instruction_offset = args.instruction_offset.unwrap_or(0);
    let start = if instruction_offset < 0 {
        step_back_instructions(&disasm, cpu, address, (-instruction_offset) as usize)
    } else {
        let mut addr = address;
        for _ in 0..instruction_offset {
            let len = disasm.disassemble_one(cpu.bus(), addr).raw_bytes.len() as u16;
            addr = addr.wrapping_add(len);
        }
        addr
    };

    // `end == 0` disables `disassemble_range`'s end-address check, and every opcode decodes to
    // at least 1 byte, so this always returns exactly `count` lines (wrapping the 16-bit address
    // space rather than stopping), satisfying DAP's "must return exactly this many" contract with
    // no separate padding step.
    let count = args.instruction_count.max(0) as usize;
    let lines = disasm.disassemble_range(cpu.bus(), start, 0, count);
    Ok(lines.iter().map(to_dap_instruction).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use emma65::emulator::{AddressRange, Bus, CpuVariant};

    /// Builds a CPU with 64KB RAM, writing `program` starting at `start`.
    fn make_cpu(start: u16, program: &[u8]) -> Cpu {
        let mut bus = Bus::config().ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0).unwrap().build();
        for (i, &byte) in program.iter().enumerate() {
            bus.write(start.wrapping_add(i as u16), byte).unwrap();
        }
        Cpu::builder(CpuVariant::Wdc65C02).bus(bus).build().unwrap()
    }

    fn args(memory_reference: &str, offset: Option<i64>, instruction_offset: Option<i64>, instruction_count: i64) -> DisassembleArguments {
        DisassembleArguments {
            memory_reference: memory_reference.to_string(),
            offset,
            instruction_offset,
            instruction_count,
            resolve_symbols: None,
        }
    }

    #[test]
    fn disassembles_forward_from_a_hex_memory_reference() {
        // NOP, LDX #$FF
        let cpu = make_cpu(0x0200, &[0xEA, 0xA2, 0xFF]);
        let instructions = disassemble(&cpu, &args("0x0200", None, None, 2)).unwrap();

        assert_eq!(instructions[0].address, "0x0200");
        assert_eq!(instructions[0].instruction, "NOP");
        assert_eq!(instructions[1].address, "0x0201");
        assert!(instructions[1].instruction.starts_with("LDX #$FF"));
    }

    #[test]
    fn decimal_memory_reference_and_byte_offset_resolve_the_same_address_as_hex() {
        let cpu = make_cpu(0x0200, &[0xEA]);
        let instructions = disassemble(&cpu, &args("500", Some(12), None, 1)).unwrap(); // 500 + 12 = 0x0200
        assert_eq!(instructions[0].address, "0x0200");
    }

    #[test]
    fn positive_instruction_offset_skips_whole_instructions() {
        // NOP, LDX #$FF, NOP
        let cpu = make_cpu(0x0200, &[0xEA, 0xA2, 0xFF, 0xEA]);
        let instructions = disassemble(&cpu, &args("0x0200", None, Some(1), 1)).unwrap();
        assert_eq!(instructions[0].address, "0x0201");
        assert!(instructions[0].instruction.starts_with("LDX #$FF"));
    }

    #[test]
    fn negative_instruction_offset_resyncs_backward_over_variable_length_instructions() {
        // NOP ($EA, 1 byte) at 0x0200, LDX #$FF ($A2 $FF, 2 bytes) at 0x0201, NOP at 0x0203.
        let cpu = make_cpu(0x0200, &[0xEA, 0xA2, 0xFF, 0xEA]);
        let instructions = disassemble(&cpu, &args("0x0203", None, Some(-2), 3)).unwrap();

        assert_eq!(instructions.len(), 3);
        assert_eq!(instructions[0].address, "0x0200");
        assert_eq!(instructions[0].instruction, "NOP");
        assert_eq!(instructions[1].address, "0x0201");
        assert!(instructions[1].instruction.starts_with("LDX #$FF"));
        assert_eq!(instructions[2].address, "0x0203");
        assert_eq!(instructions[2].instruction, "NOP");
    }

    #[test]
    fn disassembling_past_the_top_of_the_address_space_wraps_instead_of_short_returning() {
        // DAP requires exactly `instruction_count` entries back; confirm the response is
        // full-length even when the requested window runs off the end of the 16-bit space.
        let cpu = make_cpu(0xFFFE, &[0xEA, 0xEA, 0xEA]); // NOP at 0xFFFE, 0xFFFF, then wraps to 0x0000
        let instructions = disassemble(&cpu, &args("0xFFFE", None, None, 3)).unwrap();

        assert_eq!(instructions.len(), 3);
        assert_eq!(instructions[0].address, "0xFFFE");
        assert_eq!(instructions[1].address, "0xFFFF");
        assert_eq!(instructions[2].address, "0x0000");
    }
}
