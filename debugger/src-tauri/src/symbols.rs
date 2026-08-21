//! Symbols panel: a read-only snapshot of the live symbol table (name,
//! address, source, and cross-source aliases) for the Symbols panel's table.
//! Unlike Breakpoints/Watchpoints, this panel has no add/edit/remove
//! commands of its own — `SymbolTable` is mutated elsewhere (assembling,
//! loading a labels file, switching profiles), and this module just projects
//! its current contents for display.

use tauri::State;

use emma65::emulator::{SymbolSource, SymbolTable};

use crate::CpuState;

/// A single symbol table row returned to the frontend: one per live
/// `(name, source)` entry, matching `SymbolTable::iter()` directly — a name
/// legitimately appears more than once if multiple sources define it (e.g. a
/// `.lbl` file and a re-assemble both defining the same label).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct SymbolRow {
    pub name: String,
    pub address: u16,
    /// Human-readable source label: `"User"`, `"Assembler"`, or
    /// `"File: <basename>"`.
    pub source: String,
    /// Full canonical path, present only for a `File`-sourced row — the
    /// frontend shows this as a tooltip over the (basename-only) `source`
    /// text.
    pub source_path: Option<String>,
    /// Every other live name mapped to the same address, across all
    /// sources, excluding `name` itself, sorted and deduplicated.
    pub aliases: Vec<String>,
}

/// Formats `source` for display, returning `(label, full_path)`. `label` is
/// what the Source column shows; `full_path` is `Some` only for a
/// `File`-sourced entry, for the frontend to use as a tooltip.
fn format_source(source: &SymbolSource) -> (String, Option<String>) {
    match source {
        SymbolSource::User => ("User".to_string(), None),
        SymbolSource::Assembler => ("Assembler".to_string(), None),
        SymbolSource::File(path) => {
            let basename = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
            (format!("File: {basename}"), Some(path.display().to_string()))
        }
    }
}

/// Returns every other live name mapped to `address`, across all sources,
/// excluding `name` itself (a name can appear more than once in
/// `names_for` if more than one source defines it at the same address —
/// that's still just "the same name", not an alias of itself).
fn aliases_for(table: &SymbolTable, name: &str, address: u16) -> Vec<String> {
    let mut aliases: Vec<String> = table.names_for(address).filter(|&n| n != name).map(String::from).collect();
    aliases.sort();
    aliases.dedup();
    aliases
}

/// Projects every live entry in `table` into the rows the Symbols panel
/// displays.
fn symbol_rows(table: &SymbolTable) -> Vec<SymbolRow> {
    table
        .iter()
        .map(|(name, source, address)| {
            let (source, source_path) = format_source(source);
            SymbolRow { name: name.to_string(), address, source, source_path, aliases: aliases_for(table, name, address) }
        })
        .collect()
}

/// Returns a snapshot of every live symbol table entry, for the Symbols
/// panel. Returns an empty list (not an error) if the CPU isn't ready, since
/// the panel should just render empty in that case rather than show an
/// error state.
#[tauri::command]
pub fn get_symbols(cpu_state: State<CpuState>) -> Vec<SymbolRow> {
    let guard = cpu_state.0.lock().unwrap();
    match guard.as_ref() {
        Some(cpu) => symbol_rows(cpu.bus().symbol_table()),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn symbol_rows_empty_table() {
        assert!(symbol_rows(&SymbolTable::default()).is_empty());
    }

    #[test]
    fn symbol_rows_single_entry_has_no_aliases() {
        let mut table = SymbolTable::default();
        table.insert("foo".to_string(), 0x1000);
        let rows = symbol_rows(&table);
        assert_eq!(rows, vec![SymbolRow {
            name: "foo".to_string(),
            address: 0x1000,
            source: "User".to_string(),
            source_path: None,
            aliases: vec![],
        }]);
    }

    #[test]
    fn symbol_rows_multiple_names_same_address_are_mutual_aliases() {
        let mut table = SymbolTable::default();
        table.insert("foo".to_string(), 0xBEEF);
        table.insert("bar".to_string(), 0xBEEF);
        let rows = symbol_rows(&table);
        assert_eq!(rows.len(), 2);
        let foo = rows.iter().find(|r| r.name == "foo").unwrap();
        assert_eq!(foo.aliases, vec!["bar".to_string()]);
        let bar = rows.iter().find(|r| r.name == "bar").unwrap();
        assert_eq!(bar.aliases, vec!["foo".to_string()]);
    }

    #[test]
    fn symbol_rows_one_row_per_source_for_a_multiply_defined_name() {
        let mut table = SymbolTable::default();
        let file_source = SymbolSource::File(PathBuf::from("/some/program.lbl"));
        table.insert_tagged("START".to_string(), 0x8000, file_source.clone());
        table.insert_tagged("START".to_string(), 0x9000, SymbolSource::Assembler);

        let rows = symbol_rows(&table);
        assert_eq!(rows.len(), 2);

        let file_row = rows.iter().find(|r| r.source.starts_with("File:")).unwrap();
        assert_eq!(file_row.name, "START");
        assert_eq!(file_row.address, 0x8000);
        assert_eq!(file_row.source, "File: program.lbl");
        assert_eq!(file_row.source_path.as_deref(), Some("/some/program.lbl"));
        // Same name from another source at a different address isn't an alias.
        assert!(file_row.aliases.is_empty());

        let asm_row = rows.iter().find(|r| r.source == "Assembler").unwrap();
        assert_eq!(asm_row.address, 0x9000);
        assert!(asm_row.aliases.is_empty());
    }

    #[test]
    fn symbol_rows_aliases_exclude_same_name_from_another_source_at_the_same_address() {
        // Two sources defining the *same* name at the *same* address: still
        // not an alias of itself, even though `names_for` yields "foo" twice.
        let mut table = SymbolTable::default();
        table.insert_tagged("foo".to_string(), 0x1000, SymbolSource::User);
        table.insert_tagged("foo".to_string(), 0x1000, SymbolSource::Assembler);
        table.insert_tagged("bar".to_string(), 0x1000, SymbolSource::User);

        let rows = symbol_rows(&table);
        for row in rows.iter().filter(|r| r.name == "foo") {
            assert_eq!(row.aliases, vec!["bar".to_string()]);
        }
    }
}
