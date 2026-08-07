//! `setInstructionBreakpoints` request handling.
//!
//! Mirrors the Tauri debugger's `disassembly.rs` breakpoint operations
//! (`toggle_breakpoint`/`set_breakpoint`/`remove_breakpoint`/`disable_breakpoint`/
//! `enable_breakpoint`), translated to DAP's replace-the-whole-set contract:
//! `setInstructionBreakpoints` is called with the complete desired set of instruction
//! breakpoints every time (VS Code re-sends the full list on each gutter click, not an
//! incremental add/remove), so there is no separate enable/disable state to track here —
//! an address either is or isn't in the set `Cpu` should halt at.

use std::collections::HashSet;

use dap::requests::SetInstructionBreakpointsArguments;
use dap::types::Breakpoint;
use emma65::emulator::Cpu;

use crate::disasm::parse_memory_reference;

/// Resolves a single `InstructionBreakpoint`'s `instructionReference`/`offset` pair to an
/// address, the same way `disasm::disassemble` resolves `memoryReference`/`offset`.
fn resolve_addr(instruction_reference: &str, offset: Option<i64>) -> Result<u16, String> {
    let addr = parse_memory_reference(instruction_reference)?.wrapping_add(offset.unwrap_or(0));
    Ok(addr as u16)
}

/// Replaces `cpu`'s breakpoint set with the addresses in `args`, adding new ones and removing
/// any previously-set address that is no longer present, then returns a `Breakpoint` per input
/// entry (in the same order) confirming it as verified.
pub fn set_instruction_breakpoints(cpu: &mut Cpu, args: &SetInstructionBreakpointsArguments) -> Result<Vec<Breakpoint>, String> {
    let mut addrs = Vec::with_capacity(args.breakpoints.len());
    for bp in &args.breakpoints {
        addrs.push(resolve_addr(&bp.instruction_reference, bp.offset)?);
    }

    let desired: HashSet<u16> = addrs.iter().copied().collect();
    let current: Vec<u16> = cpu.breakpoints().iter().copied().collect();
    for addr in current {
        if !desired.contains(&addr) {
            cpu.remove_breakpoint(addr);
        }
    }
    for &addr in &addrs {
        cpu.add_breakpoint(addr);
    }

    // `Breakpoint::instruction_reference` is deliberately left `None`: the `dap` crate's
    // `Breakpoint` type is missing `#[serde(rename_all = "camelCase")]` (unlike
    // `InstructionBreakpoint`), so setting it would serialize as `instruction_reference`
    // instead of spec's `instructionReference`. `verified` (a bare boolean, unaffected by
    // casing) is all VS Code's Disassembly View needs to render the breakpoint dot.
    Ok(addrs.into_iter().map(|_| Breakpoint { verified: true, ..Default::default() }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dap::types::InstructionBreakpoint;
    use emma65::emulator::{AddressRange, Bus, CpuVariant};

    fn make_cpu() -> Cpu {
        let bus = Bus::config().ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0).unwrap().build();
        Cpu::builder(CpuVariant::Wdc65C02).bus(bus).build().unwrap()
    }

    fn ibp(instruction_reference: &str) -> InstructionBreakpoint {
        InstructionBreakpoint { instruction_reference: instruction_reference.to_string(), ..Default::default() }
    }

    fn args(breakpoints: Vec<InstructionBreakpoint>) -> SetInstructionBreakpointsArguments {
        SetInstructionBreakpointsArguments { breakpoints }
    }

    #[test]
    fn sets_breakpoints_on_the_cpu_and_reports_them_verified() {
        let mut cpu = make_cpu();
        let result = set_instruction_breakpoints(&mut cpu, &args(vec![ibp("0x0200"), ibp("0x0300")])).unwrap();

        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|bp| bp.verified));
        assert_eq!(cpu.breakpoints(), &HashSet::from([0x0200, 0x0300]));
    }

    #[test]
    fn removes_breakpoints_no_longer_in_the_set() {
        let mut cpu = make_cpu();
        set_instruction_breakpoints(&mut cpu, &args(vec![ibp("0x0200"), ibp("0x0300")])).unwrap();

        set_instruction_breakpoints(&mut cpu, &args(vec![ibp("0x0300")])).unwrap();

        assert_eq!(cpu.breakpoints(), &HashSet::from([0x0300]));
    }

    #[test]
    fn empty_list_clears_all_breakpoints() {
        let mut cpu = make_cpu();
        set_instruction_breakpoints(&mut cpu, &args(vec![ibp("0x0200")])).unwrap();

        let result = set_instruction_breakpoints(&mut cpu, &args(vec![])).unwrap();

        assert!(result.is_empty());
        assert!(cpu.breakpoints().is_empty());
    }

    #[test]
    fn resolves_offset_relative_to_the_instruction_reference() {
        let mut cpu = make_cpu();
        let mut bp = ibp("0x0200");
        bp.offset = Some(4);

        set_instruction_breakpoints(&mut cpu, &args(vec![bp])).unwrap();

        assert_eq!(cpu.breakpoints(), &HashSet::from([0x0204]));
    }

    #[test]
    fn setting_the_same_address_twice_is_idempotent() {
        let mut cpu = make_cpu();
        set_instruction_breakpoints(&mut cpu, &args(vec![ibp("0x0200"), ibp("0x0200")])).unwrap();

        assert_eq!(cpu.breakpoints(), &HashSet::from([0x0200]));
    }
}
