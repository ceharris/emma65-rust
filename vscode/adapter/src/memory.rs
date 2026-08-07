//! `readMemory`/`writeMemory` request handling, backed by `Bus::peek_range`/`Bus::write`.
//!
//! Mirrors the Tauri debugger's `memory.rs::get_memory`/`write_memory`, translated from
//! Tauri commands taking `(addr, data)` to DAP's `readMemory`/`writeMemory` requests, which
//! additionally resolve a `memoryReference`/`offset` pair into a start address (same
//! resolution `disasm::disassemble` already does) and base64-encode/decode the byte payload.
//! Covers both the Tauri debugger's Memory panel and Stack panel, since the stack is just a
//! memory range starting at `0x0100` — no separate stack-specific request exists in DAP.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use dap::requests::{ReadMemoryArguments, WriteMemoryArguments};
use dap::responses::{ReadMemoryResponse, WriteMemoryResponse};
use emma65::emulator::Cpu;

use crate::disasm::parse_memory_reference;

/// Resolves a DAP `memoryReference`/`offset` pair into a 16-bit address, wrapping like
/// `disasm::disassemble`'s equivalent resolution.
fn resolve_address(memory_reference: &str, offset: Option<i64>) -> Result<u16, String> {
    Ok(parse_memory_reference(memory_reference)?.wrapping_add(offset.unwrap_or(0)) as u16)
}

/// Reads `args.count` bytes starting at the resolved address, via `Bus::peek_range` so no
/// device side effects occur, matching `get_memory`'s read semantics.
pub fn read_memory(cpu: &Cpu, args: &ReadMemoryArguments) -> Result<ReadMemoryResponse, String> {
    let address = resolve_address(&args.memory_reference, args.offset)?;
    let mut buf = vec![0u8; args.count.max(0) as usize];
    cpu.bus().peek_range(address, &mut buf).map_err(|e| e.to_string())?;
    Ok(ReadMemoryResponse { address: format!("0x{address:04X}"), unreadable_bytes: None, data: Some(BASE64.encode(&buf)) })
}

/// Writes the base64-decoded payload starting at the resolved address, via `Bus::write` so
/// device side effects and ROM write protection apply, matching `write_memory`'s semantics
/// (minus its Tauri-only `patch` bypass, which DAP has no equivalent field for).
///
/// When `args.allow_partial` is set, a write failure (e.g. hitting protected ROM) stops the
/// loop and reports how many bytes were written so far instead of failing the whole request.
pub fn write_memory(cpu: &mut Cpu, args: &WriteMemoryArguments) -> Result<WriteMemoryResponse, String> {
    let address = resolve_address(&args.memory_reference, args.offset)?;
    let data = BASE64.decode(&args.data).map_err(|e| format!("invalid data: {e}"))?;
    let allow_partial = args.allow_partial.unwrap_or(false);

    let mut written = 0i64;
    for &byte in &data {
        let addr = address.wrapping_add(written as u16);
        match cpu.bus_mut().write(addr, byte) {
            Ok(()) => written += 1,
            Err(_) if allow_partial => return Ok(WriteMemoryResponse { offset: Some(0), bytes_written: Some(written) }),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(WriteMemoryResponse { offset: Some(0), bytes_written: Some(written) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use emma65::emulator::{AddressRange, Bus, CpuVariant, RomWritePolicy};

    fn make_cpu() -> Cpu {
        let bus = Bus::config().ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0).unwrap().build();
        Cpu::builder(CpuVariant::Wdc65C02).bus(bus).build().unwrap()
    }

    fn read_args(memory_reference: &str, offset: Option<i64>, count: i64) -> ReadMemoryArguments {
        ReadMemoryArguments { memory_reference: memory_reference.to_string(), offset, count }
    }

    fn write_args(memory_reference: &str, offset: Option<i64>, data: &str, allow_partial: Option<bool>) -> WriteMemoryArguments {
        WriteMemoryArguments { memory_reference: memory_reference.to_string(), offset, allow_partial, data: data.to_string() }
    }

    #[test]
    fn reads_bytes_from_a_hex_memory_reference() {
        let mut cpu = make_cpu();
        cpu.bus_mut().write(0x0200, 0xAB).unwrap();
        cpu.bus_mut().write(0x0201, 0xCD).unwrap();

        let response = read_memory(&cpu, &read_args("0x0200", None, 2)).unwrap();
        assert_eq!(response.address, "0x0200");
        assert_eq!(response.unreadable_bytes, None);
        assert_eq!(BASE64.decode(response.data.unwrap()).unwrap(), vec![0xAB, 0xCD]);
    }

    #[test]
    fn reads_bytes_with_a_decimal_memory_reference_and_offset() {
        let mut cpu = make_cpu();
        cpu.bus_mut().write(0x0200, 0x42).unwrap();

        let response = read_memory(&cpu, &read_args("500", Some(12), 1)).unwrap(); // 500 + 12 = 0x0200
        assert_eq!(response.address, "0x0200");
        assert_eq!(BASE64.decode(response.data.unwrap()).unwrap(), vec![0x42]);
    }

    #[test]
    fn writes_bytes_through_the_bus() {
        let mut cpu = make_cpu();
        let data = BASE64.encode([0x11, 0x22, 0x33]);

        let response = write_memory(&mut cpu, &write_args("0x0300", None, &data, None)).unwrap();
        assert_eq!(response.bytes_written, Some(3));
        assert_eq!(cpu.bus().peek(0x0300).unwrap(), 0x11);
        assert_eq!(cpu.bus().peek(0x0301).unwrap(), 0x22);
        assert_eq!(cpu.bus().peek(0x0302).unwrap(), 0x33);
    }

    #[test]
    fn rejects_invalid_base64() {
        let mut cpu = make_cpu();
        assert!(write_memory(&mut cpu, &write_args("0x0300", None, "not-base64!!", None)).is_err());
    }

    #[test]
    fn write_stops_and_reports_progress_when_rom_write_fails_and_allow_partial_is_set() {
        let mut cpu = {
            let bus = Bus::config()
                .rom_write_policy(RomWritePolicy::Error)
                .ram_with_fill(AddressRange::new(0x0000, 0x7FFF), 0)
                .unwrap()
                .rom(AddressRange::new(0x8000, 0xFFFF), vec![0u8; 0x8000])
                .unwrap()
                .build();
            Cpu::builder(CpuVariant::Wdc65C02).bus(bus).build().unwrap()
        };
        let data = BASE64.encode([0x11, 0x22, 0x33]);

        let response = write_memory(&mut cpu, &write_args("0x7FFF", None, &data, Some(true))).unwrap();
        assert_eq!(response.bytes_written, Some(1)); // wrote 0x7FFF (RAM), stopped at 0x8000 (ROM)
        assert_eq!(cpu.bus().peek(0x7FFF).unwrap(), 0x11);
    }

    #[test]
    fn write_fails_the_whole_request_when_rom_write_fails_and_allow_partial_is_not_set() {
        let mut cpu = {
            let bus = Bus::config()
                .rom_write_policy(RomWritePolicy::Error)
                .ram_with_fill(AddressRange::new(0x0000, 0x7FFF), 0)
                .unwrap()
                .rom(AddressRange::new(0x8000, 0xFFFF), vec![0u8; 0x8000])
                .unwrap()
                .build();
            Cpu::builder(CpuVariant::Wdc65C02).bus(bus).build().unwrap()
        };
        let data = BASE64.encode([0x11, 0x22]);

        assert!(write_memory(&mut cpu, &write_args("0x7FFF", None, &data, None)).is_err());
    }
}
