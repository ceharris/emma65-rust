//! `scopes`/`variables`/`setVariable` for the "CPU Registers" scope.
//!
//! Mirrors the Tauri debugger's `registers.rs::get_registers`/`set_register`, translated
//! from Tauri commands to DAP's Variables view: A, X, Y, PC, S, P as a flat "CPU Registers"
//! scope, with P's value rendered as the same 8-character NV-BDIZC flag string (dash where
//! clear) the Tauri register panel shows. `setVariable` accepts `0b`/`0o`/`0q`/`0x`-prefixed
//! binary/octal/hex or (unprefixed) signed decimal, same as the Tauri panel's editable
//! register fields.

use dap::types::{Scope, ScopePresentationhint, Variable};
use emma65::emulator::{Cpu, StatusRegister};

/// Fixed `variablesReference` for the single "CPU Registers" scope. This adapter always has
/// exactly one thread, one frame, and one scope (see `main.rs`'s `Threads`/`StackTrace`/
/// `Scopes` handling), so a constant stands in for the id VS Code would otherwise expect to
/// be minted per frame.
pub const REGISTERS_VARIABLES_REFERENCE: i64 = 1;

/// `(label, bit)` pairs for the P register, in display order N V - B D I Z C — the same
/// order and dash-for-clear convention the Tauri debugger's `RegisterPanel.tsx` uses. The
/// UNUSED bit's position always displays as `-`, since it carries no information.
const FLAG_CHARS: [(char, StatusRegister); 8] = [
    ('N', StatusRegister::N),
    ('V', StatusRegister::V),
    ('-', StatusRegister::UNUSED),
    ('B', StatusRegister::B),
    ('D', StatusRegister::D),
    ('I', StatusRegister::I),
    ('Z', StatusRegister::Z),
    ('C', StatusRegister::C),
];

/// Formats `p` as an 8-character NV-BDIZC string, letter where set, `-` where clear.
fn flags_string(p: StatusRegister) -> String {
    FLAG_CHARS.iter().map(|&(label, bit)| if label != '-' && p.contains(bit) { label } else { '-' }).collect()
}

/// Parses an NV-BDIZC flag string of the exact shape [`flags_string`] produces back into a
/// `StatusRegister`, so a `setVariable` on `P` that leaves the flag string mostly intact
/// (VS Code prefills the edit box with the current display value) round-trips without
/// forcing the user through a hex byte instead. Returns `None` for anything that isn't
/// exactly 8 characters with a recognized letter or `-` in each position.
fn parse_flags_string(s: &str) -> Option<StatusRegister> {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() != FLAG_CHARS.len() {
        return None;
    }
    let mut result = StatusRegister::UNUSED;
    for (&(label, bit), &ch) in FLAG_CHARS.iter().zip(chars.iter()) {
        if label == '-' {
            continue;
        }
        match ch {
            c if c == label => result |= bit,
            '-' => {}
            _ => return None,
        }
    }
    Some(result)
}

/// Parses a DAP `setVariable` value: `0b`-prefixed binary, `0o`/`0q`-prefixed octal,
/// `0x`/`$`-prefixed hex (all case-insensitive, all unsigned), or decimal otherwise —
/// decimal accepts a leading `+`/`-` sign, since unlike the other radixes there's no
/// prefix to carry it. Mirrors the Tauri register panel's `parseRegisterInput` prefix
/// convention (`RegisterPanel.tsx`), minus its "no prefix falls back to the current
/// display radix" behavior, since a DAP Variables scope has no such display-radix state
/// to fall back to.
fn parse_numeric(value: &str) -> Result<i64, String> {
    let value = value.trim();
    if let Some(bin) = value.strip_prefix("0b").or_else(|| value.strip_prefix("0B")) {
        return i64::from_str_radix(bin, 2).map_err(|e| format!("invalid value: {e}"));
    }
    if let Some(oct) = value
        .strip_prefix("0o")
        .or_else(|| value.strip_prefix("0O"))
        .or_else(|| value.strip_prefix("0q"))
        .or_else(|| value.strip_prefix("0Q"))
    {
        return i64::from_str_radix(oct, 8).map_err(|e| format!("invalid value: {e}"));
    }
    if let Some(hex) = value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")).or_else(|| value.strip_prefix('$')) {
        return i64::from_str_radix(hex, 16).map_err(|e| format!("invalid value: {e}"));
    }
    value.parse::<i64>().map_err(|e| format!("invalid value: {e}"))
}

/// Validates a parsed integer against a register field's bit width and returns its unsigned
/// representation, or an error naming `field` if out of range. Byte/word fields accept the
/// union of the unsigned range (`0..2^width_bits-1`) and the signed two's-complement range
/// (`-2^(width_bits-1)..-1`) when `allow_signed` — so e.g. `-1` for an 8-bit register means
/// 0xFF — mirroring the Tauri panel's `toUnsignedInRange`. PC has no signed display mode, so
/// only the unsigned range applies there.
fn to_unsigned_in_range(field: &str, value: i64, width_bits: u32, allow_signed: bool) -> Result<u32, String> {
    let max = (1i64 << width_bits) - 1;
    if !allow_signed {
        return (0..=max).contains(&value).then_some(value as u32).ok_or_else(|| format!("{field} value out of range: must be 0 to {max}"));
    }
    let min = -(1i64 << (width_bits - 1));
    (min..=max)
        .contains(&value)
        .then_some((value & max) as u32)
        .ok_or_else(|| format!("{field} value out of range: must be {min} to {max}"))
}

/// Builds the single "CPU Registers" scope.
pub fn scopes() -> Vec<Scope> {
    vec![Scope {
        name: "CPU Registers".to_string(),
        presentation_hint: Some(ScopePresentationhint::Registers),
        variables_reference: REGISTERS_VARIABLES_REFERENCE,
        named_variables: Some(6),
        expensive: false,
        ..Default::default()
    }]
}

fn variable(name: &str, value: String) -> Variable {
    Variable { name: name.to_string(), value, variables_reference: 0, ..Default::default() }
}

/// Builds the register variables for the "CPU Registers" scope, in the Tauri panel's
/// A/X/Y/PC/S/P order.
pub fn variables(cpu: &Cpu) -> Vec<Variable> {
    let regs = cpu.registers();
    vec![
        variable("A", format!("0x{:02X}", regs.a)),
        variable("X", format!("0x{:02X}", regs.x)),
        variable("Y", format!("0x{:02X}", regs.y)),
        // `memory_reference` set (only here, not on the other registers): with
        // `supportsReadMemoryRequest` advertised, this is what makes VS Code offer a
        // "View Binary Data" action on PC, the story 7 acceptance criterion's entry point
        // into the built-in Memory Inspector without a custom command.
        Variable { memory_reference: Some(format!("0x{:04X}", regs.pc)), ..variable("PC", format!("0x{:04X}", regs.pc)) },
        variable("S", format!("0x{:02X}", regs.s)),
        variable("P", flags_string(regs.p)),
    ]
}

/// Writes `name`'s new `value` into `cpu`'s registers, returning the value as it will be
/// redisplayed. Only reachable while the CPU is halted (`ExecState::with_cpu_mut` returns
/// `None` during a free-run/step, which `main.rs` reports as "CPU not ready"), matching
/// `set_register`'s existing constraint.
pub fn set_variable(cpu: &mut Cpu, name: &str, value: &str) -> Result<String, String> {
    match name {
        "P" => {
            let p = match parse_flags_string(value) {
                Some(p) => p,
                None => {
                    let byte = to_unsigned_in_range("P", parse_numeric(value)?, 8, true)? as u8;
                    StatusRegister::from_byte(byte) | StatusRegister::UNUSED
                }
            };
            cpu.registers_mut().p = p;
            Ok(flags_string(cpu.registers().p))
        }
        "PC" => {
            let word = to_unsigned_in_range("PC", parse_numeric(value)?, 16, false)? as u16;
            cpu.registers_mut().pc = word;
            Ok(format!("0x{word:04X}"))
        }
        "A" | "X" | "Y" | "S" => {
            let byte = to_unsigned_in_range(name, parse_numeric(value)?, 8, true)? as u8;
            match name {
                "A" => cpu.registers_mut().a = byte,
                "X" => cpu.registers_mut().x = byte,
                "Y" => cpu.registers_mut().y = byte,
                "S" => cpu.registers_mut().s = byte,
                _ => unreachable!(),
            }
            Ok(format!("0x{byte:02X}"))
        }
        _ => Err(format!("unknown register: {name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emma65::emulator::{AddressRange, Bus, CpuVariant};

    fn make_cpu() -> Cpu {
        let bus = Bus::config().ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0).unwrap().build();
        Cpu::builder(CpuVariant::Wdc65C02).bus(bus).build().unwrap()
    }

    #[test]
    fn variables_reports_a_x_y_pc_s_and_the_flag_string_for_p() {
        let mut cpu = make_cpu();
        cpu.registers_mut().a = 0x12;
        cpu.registers_mut().x = 0x34;
        cpu.registers_mut().y = 0x56;
        cpu.registers_mut().pc = 0xABCD;
        cpu.registers_mut().s = 0xFF;
        cpu.registers_mut().p = StatusRegister::N | StatusRegister::C | StatusRegister::UNUSED;

        let vars = variables(&cpu);
        let names: Vec<&str> = vars.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["A", "X", "Y", "PC", "S", "P"]);
        assert_eq!(vars[0].value, "0x12");
        assert_eq!(vars[1].value, "0x34");
        assert_eq!(vars[2].value, "0x56");
        assert_eq!(vars[3].value, "0xABCD");
        assert_eq!(vars[4].value, "0xFF");
        assert_eq!(vars[5].value, "N------C"); // N and C set, rest (including UNUSED) clear
        assert_eq!(vars[3].memory_reference, Some("0xABCD".to_string())); // PC's "View Binary Data" entry point
        assert_eq!(vars[0].memory_reference, None); // no other register carries one
    }

    #[test]
    fn set_variable_writes_a_byte_register_from_hex() {
        let mut cpu = make_cpu();
        let result = set_variable(&mut cpu, "X", "0x7F").unwrap();
        assert_eq!(result, "0x7F");
        assert_eq!(cpu.registers().x, 0x7F);
    }

    #[test]
    fn set_variable_writes_pc_from_decimal() {
        let mut cpu = make_cpu();
        let result = set_variable(&mut cpu, "PC", "512").unwrap();
        assert_eq!(result, "0x0200");
        assert_eq!(cpu.registers().pc, 0x0200);
    }

    #[test]
    fn set_variable_writes_p_from_a_hex_byte() {
        let mut cpu = make_cpu();
        let result = set_variable(&mut cpu, "P", "0x83").unwrap(); // N, Z, C set (0b1000_0011)
        assert_eq!(result, "N-----ZC");
        assert!(cpu.registers().p.contains(StatusRegister::N));
        assert!(cpu.registers().p.contains(StatusRegister::Z));
        assert!(cpu.registers().p.contains(StatusRegister::C));
        assert!(!cpu.registers().p.contains(StatusRegister::V));
        assert!(cpu.registers().p.contains(StatusRegister::UNUSED));
    }

    #[test]
    fn set_variable_round_trips_a_flag_string() {
        let mut cpu = make_cpu();
        cpu.registers_mut().p = StatusRegister::UNUSED;
        let displayed = flags_string(cpu.registers().p);
        assert_eq!(displayed, "--------");

        let mut edited = displayed.clone();
        edited.replace_range(0..1, "N"); // toggle N on, leaving the rest untouched
        let result = set_variable(&mut cpu, "P", &edited).unwrap();
        assert_eq!(result, "N-------");
        assert!(cpu.registers().p.contains(StatusRegister::N));
        assert!(cpu.registers().p.contains(StatusRegister::UNUSED));
    }

    #[test]
    fn set_variable_writes_a_byte_register_from_binary() {
        let mut cpu = make_cpu();
        let result = set_variable(&mut cpu, "Y", "0b01010101").unwrap();
        assert_eq!(result, "0x55");
        assert_eq!(cpu.registers().y, 0x55);
    }

    #[test]
    fn set_variable_writes_a_byte_register_from_octal() {
        let mut cpu = make_cpu();
        let result = set_variable(&mut cpu, "X", "0o17").unwrap();
        assert_eq!(result, "0x0F");
        assert_eq!(cpu.registers().x, 0x0F);

        let result = set_variable(&mut cpu, "X", "0q17").unwrap();
        assert_eq!(result, "0x0F");
    }

    #[test]
    fn set_variable_writes_a_byte_register_from_signed_decimal() {
        let mut cpu = make_cpu();
        let result = set_variable(&mut cpu, "A", "-1").unwrap();
        assert_eq!(result, "0xFF");
        assert_eq!(cpu.registers().a, 0xFF);
    }

    #[test]
    fn set_variable_rejects_a_negative_pc_since_pc_has_no_signed_range() {
        let mut cpu = make_cpu();
        assert!(set_variable(&mut cpu, "PC", "-1").is_err());
    }

    #[test]
    fn set_variable_rejects_an_out_of_range_byte() {
        let mut cpu = make_cpu();
        assert!(set_variable(&mut cpu, "A", "256").is_err());
        assert!(set_variable(&mut cpu, "A", "-129").is_err());
    }

    #[test]
    fn set_variable_rejects_an_unknown_register_name() {
        let mut cpu = make_cpu();
        assert!(set_variable(&mut cpu, "D", "0x00").is_err());
    }
}
