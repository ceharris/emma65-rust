//! Assembler panel: `assemble_and_load` assembles source text and writes the
//! result directly into emulator memory via `Bus::patch`.

use tauri::{AppHandle, Emitter, State};

use emma65::assembler;
use emma65::emulator::Cpu;

use crate::CpuState;

#[derive(Clone, serde::Serialize)]
pub struct AssembleDiagnostic {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct SegmentSummary {
    pub origin: u16,
    pub length: usize,
}

#[derive(Clone, serde::Serialize)]
pub struct AssembleReport {
    pub success: bool,
    pub diagnostics: Vec<AssembleDiagnostic>,
    pub segments: Vec<SegmentSummary>,
    pub symbol_count: usize,
}

/// Assembles `source` and, on success, patches the resulting segments into
/// `cpu`'s bus and merges the resulting symbols into its symbol table.
///
/// On failure, makes zero bus writes, even when a later segment in a
/// multi-`.org` source would have succeeded.
fn assemble_and_patch(source: &str, cpu: &mut Cpu) -> AssembleReport {
    let program = match assembler::assemble(source) {
        Ok(program) => program,
        Err(errors) => {
            let diagnostics = errors
                .iter()
                .map(|e| AssembleDiagnostic { line: e.line(), column: e.column(), message: e.message().to_string() })
                .collect();
            return AssembleReport { success: false, diagnostics, segments: Vec::new(), symbol_count: 0 };
        }
    };

    let bus = cpu.bus_mut();
    let segments = program
        .segments
        .iter()
        .map(|segment| {
            for (i, &byte) in segment.bytes.iter().enumerate() {
                bus.patch(segment.origin.wrapping_add(i as u16), byte);
            }
            SegmentSummary { origin: segment.origin, length: segment.bytes.len() }
        })
        .collect();
    bus.symbol_table_mut().insert_from(&program.symbols);

    AssembleReport { success: true, diagnostics: Vec::new(), segments, symbol_count: program.symbols.len() }
}

/// Assembles `source` and patches the result into the CPU's bus, merging the
/// resulting symbols into the bus symbol table without clearing existing
/// ones (so ROM-loaded labels survive an assemble).
///
/// Only callable while the CPU is halted; returns an error if the CPU is
/// running. A bad assembly source is *not* an `Err` here — it's reported as
/// `AssembleReport { success: false, .. }`; `Err` is reserved for
/// infrastructure failure (CPU not ready). Emits `"debugger-halted"` and
/// `"memory-modified"` on return regardless of `success`, to refresh
/// dependent panels (a no-op refresh on failure is harmless).
#[tauri::command]
pub fn assemble_and_load(source: String, cpu_state: State<CpuState>, app: AppHandle) -> Result<AssembleReport, String> {
    let (report, pc) = {
        let mut guard = cpu_state.0.lock().unwrap();
        let cpu = guard.as_mut().ok_or("CPU not ready")?;
        let report = assemble_and_patch(&source, cpu);
        (report, cpu.registers().pc)
    };
    app.emit("debugger-halted", pc).ok();
    app.emit("memory-modified", ()).ok();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use emma65::emulator::{AddressRange, Bus, ClockSpeed, CpuBuilder, CpuVariant};

    fn make_cpu() -> Cpu {
        let bus = Bus::config()
            .ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0)
            .unwrap()
            .build();
        let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
            .clock_speed(ClockSpeed::mhz(1.8432))
            .bus(bus)
            .build()
            .unwrap();
        cpu.reset().unwrap();
        cpu
    }

    #[test]
    fn assemble_and_patch_good_source_lands_bytes_and_reports_segments() {
        let mut cpu = make_cpu();
        let report = assemble_and_patch(".org $8000\nstart:\nLDA #$01\nSTA $10\n", &mut cpu);

        assert!(report.success);
        assert!(report.diagnostics.is_empty());
        assert_eq!(report.segments, vec![SegmentSummary { origin: 0x8000, length: 4 }]);
        assert_eq!(report.symbol_count, 1);

        let mut buf = [0u8; 4];
        cpu.bus().peek_range(0x8000, &mut buf).unwrap();
        assert_eq!(buf, [0xA9, 0x01, 0x85, 0x10]);
    }

    #[test]
    fn assemble_and_patch_bad_source_reports_diagnostics_and_writes_nothing() {
        let mut cpu = make_cpu();

        let mut before = [0u8; 0x100];
        cpu.bus().peek_range(0x8000, &mut before).unwrap();

        let report = assemble_and_patch(".org $8000\nLDA missing\n", &mut cpu);

        assert!(!report.success);
        assert!(report.segments.is_empty());
        assert_eq!(report.symbol_count, 0);
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].line, 2);
        assert!(report.diagnostics[0].message.contains("undefined symbol"));

        let mut after = [0u8; 0x100];
        cpu.bus().peek_range(0x8000, &mut after).unwrap();
        assert_eq!(before, after, "a failed assemble must not write any bytes");
    }

    #[test]
    fn assemble_and_patch_merges_symbols_additively_without_clearing_existing() {
        let mut cpu = make_cpu();
        cpu.bus_mut().symbol_table_mut().insert("existing".to_string(), 0x1234);

        let report = assemble_and_patch(".org $8000\nstart:\nNOP\n", &mut cpu);

        assert!(report.success);
        assert_eq!(cpu.bus().symbol_table().address_for("existing"), Some(0x1234));
        assert_eq!(cpu.bus().symbol_table().address_for("start"), Some(0x8000));
    }

    #[test]
    fn assemble_and_patch_multiple_org_segments_land_at_respective_origins() {
        let mut cpu = make_cpu();
        let report = assemble_and_patch(".org $8000\nLDA #$01\n.org $9000\nJMP $8000\n", &mut cpu);

        assert!(report.success);
        assert_eq!(
            report.segments,
            vec![SegmentSummary { origin: 0x8000, length: 2 }, SegmentSummary { origin: 0x9000, length: 3 }],
        );

        let mut first = [0u8; 2];
        cpu.bus().peek_range(0x8000, &mut first).unwrap();
        assert_eq!(first, [0xA9, 0x01]);

        let mut second = [0u8; 3];
        cpu.bus().peek_range(0x9000, &mut second).unwrap();
        assert_eq!(second, [0x4C, 0x00, 0x80]);
    }
}
