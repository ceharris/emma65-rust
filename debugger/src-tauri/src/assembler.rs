//! Assembler panel: `assemble_preview` assembles source text and reports the
//! resulting segments without touching memory, for the Assemble confirmation
//! dialog; `assemble_and_load` (invoked only once the user confirms) assembles
//! the same source again and writes the result into emulator memory via
//! `Bus::patch`. Re-assembling on confirm rather than carrying the first
//! assemble's result across the Tauri IPC boundary is deliberate — `assemble`
//! is a pure, deterministic function of the source text, so a second call is
//! cheap and avoids needing `AssembledProgram` to be `Serialize`.

use tauri::{AppHandle, Emitter, State};

use emma65::assembler;
use emma65::emulator::{Cpu, SymbolSource};

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

fn diagnostics_from_errors(errors: &[assembler::Error]) -> Vec<AssembleDiagnostic> {
    errors.iter().map(|e| AssembleDiagnostic { line: e.line(), column: e.column(), message: e.message().to_string() }).collect()
}

/// Assembles `source` and reports the resulting segments/diagnostics without
/// writing anything to memory — used to populate the Assemble confirmation
/// dialog before the user decides whether to commit the write.
fn assemble_preview_report(source: &str) -> AssembleReport {
    match assembler::assemble(source) {
        Ok(program) => {
            let segments =
                program.segments.iter().map(|s| SegmentSummary { origin: s.origin, length: s.bytes.len() }).collect();
            AssembleReport { success: true, diagnostics: Vec::new(), segments, symbol_count: program.symbols.len() }
        }
        Err(errors) => {
            AssembleReport { success: false, diagnostics: diagnostics_from_errors(&errors), segments: Vec::new(), symbol_count: 0 }
        }
    }
}

/// Assembles `source` and, on success, patches the resulting segments into
/// `cpu`'s bus and replaces the `Assembler`-sourced entries in its symbol
/// table with the newly assembled symbols, leaving `File`/`User`-sourced
/// entries untouched. This is what prevents a re-assemble after moving a
/// label from leaving a stale, ghost entry for that name at its old address.
///
/// On failure, makes zero bus writes, even when a later segment in a
/// multi-`.org` source would have succeeded.
fn assemble_and_patch(source: &str, cpu: &mut Cpu) -> AssembleReport {
    let program = match assembler::assemble(source) {
        Ok(program) => program,
        Err(errors) => {
            return AssembleReport { success: false, diagnostics: diagnostics_from_errors(&errors), segments: Vec::new(), symbol_count: 0 };
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
    let symbol_table = bus.symbol_table_mut();
    symbol_table.clear_source(&SymbolSource::Assembler);
    symbol_table.insert_from(&program.symbols);

    AssembleReport { success: true, diagnostics: Vec::new(), segments, symbol_count: program.symbols.len() }
}

/// Assembles `source` and reports the resulting segments/diagnostics without
/// writing anything to memory — the Assembler panel calls this first, to
/// populate its confirmation dialog, before ever calling `assemble_and_load`.
/// Doesn't touch the CPU at all (assembling is pure), so unlike
/// `assemble_and_load` it isn't gated on the CPU being halted.
#[tauri::command]
pub fn assemble_preview(source: String) -> AssembleReport {
    assemble_preview_report(&source)
}

/// Assembles `source` and patches the result into the CPU's bus, replacing
/// the bus symbol table's `Assembler`-sourced entries with the newly
/// assembled symbols (so ROM-loaded labels and any user-defined ones survive
/// an assemble, and a re-assemble after moving a label doesn't leave a ghost
/// entry behind).
///
/// Only callable while the CPU is halted; returns an error if the CPU is
/// running. A bad assembly source is *not* an `Err` here — it's reported as
/// `AssembleReport { success: false, .. }`; `Err` is reserved for
/// infrastructure failure (CPU not ready). Emits `"debugger-halted"`,
/// `"memory-modified"`, and `"symbols-changed"` on return regardless of
/// `success`, to refresh dependent panels (a no-op refresh on failure is
/// harmless).
///
/// The Assembler panel only calls this after the user has confirmed the
/// segment preview from `assemble_preview` — it re-assembles `source` rather
/// than reusing that preview's result, since `assemble` is pure/deterministic
/// and a second call is cheaper than carrying an `AssembledProgram` across
/// the IPC boundary.
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
    app.emit("symbols-changed", ()).ok();
    Ok(report)
}

/// Reads a source file's full contents as UTF-8 text, for the Assembler
/// panel's Open… command.
#[tauri::command]
pub async fn read_source_file(path: String) -> Result<String, String> {
    tokio::fs::read_to_string(&path).await.map_err(|e| e.to_string())
}

/// Writes `contents` verbatim to the file at `path`, creating or overwriting
/// it, for the Assembler panel's Save/Save As… commands.
#[tauri::command]
pub async fn write_source_file(path: String, contents: String) -> Result<(), String> {
    tokio::fs::write(&path, contents).await.map_err(|e| e.to_string())
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
    fn assemble_preview_report_good_source_reports_segments_without_a_cpu() {
        let report = assemble_preview_report(".org $8000\nstart:\nLDA #$01\nSTA $10\n");

        assert!(report.success);
        assert!(report.diagnostics.is_empty());
        assert_eq!(report.segments, vec![SegmentSummary { origin: 0x8000, length: 4 }]);
        assert_eq!(report.symbol_count, 1);
    }

    #[test]
    fn assemble_preview_report_bad_source_reports_diagnostics() {
        let report = assemble_preview_report(".org $8000\nLDA missing\n");

        assert!(!report.success);
        assert!(report.segments.is_empty());
        assert_eq!(report.symbol_count, 0);
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].line, 2);
        assert!(report.diagnostics[0].message.contains("undefined symbol"));
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
    fn assemble_and_patch_reassemble_after_label_move_leaves_no_ghost() {
        let mut cpu = make_cpu();
        let file_source = SymbolSource::File(std::path::PathBuf::from("/rom/labels.lbl"));
        cpu.bus_mut().symbol_table_mut().insert_tagged("ROM_ENTRY".to_string(), 0xF000, file_source.clone());

        let first = assemble_and_patch(".org $8000\nSTART:\nNOP\n", &mut cpu);
        assert!(first.success);
        assert_eq!(cpu.bus().symbol_table().address_for("START"), Some(0x8000));

        let second = assemble_and_patch(".org $9000\nSTART:\nNOP\n", &mut cpu);
        assert!(second.success);

        // The label moved; its old address must no longer report it.
        assert!(!cpu.bus().symbol_table().names_for(0x8000).any(|n| n == "START"));
        assert_eq!(cpu.bus().symbol_table().address_for("START"), Some(0x9000));

        // A pre-existing File-sourced symbol survives both assembles untouched.
        assert_eq!(cpu.bus().symbol_table().address_for("ROM_ENTRY"), Some(0xF000));
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

    #[tokio::test]
    async fn write_source_file_then_read_source_file_round_trips_contents() {
        let dir = std::env::temp_dir().join(format!("emma65-assembler-test-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("program.s").to_string_lossy().into_owned();

        write_source_file(path.clone(), ".org $8000\nSTART:\nNOP\n".to_string()).await.unwrap();
        let contents = read_source_file(path).await.unwrap();

        assert_eq!(contents, ".org $8000\nSTART:\nNOP\n");
        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn read_source_file_missing_path_reports_error() {
        let result = read_source_file("/nonexistent/emma65-assembler-test/program.s".to_string()).await;
        assert!(result.is_err());
    }
}
