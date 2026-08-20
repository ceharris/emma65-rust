//! Support for symbolic labels assigned to addresses.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;

/// The origin of a symbol table entry, used to resolve precedence when
/// multiple sources define the same name and to selectively replace a
/// single source's contribution without disturbing the others.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SymbolSource {
    /// Loaded from a VICE labels file, identified by its canonicalized,
    /// absolute path.
    File(PathBuf),
    /// Produced by `emma65::assembler::assemble()`.
    Assembler,
    /// Defined directly by a person (or, in tests, code standing in for
    /// one). Also the default source for the backward-compatible
    /// `insert`/`remove` methods.
    User,
}

impl SymbolSource {
    /// Resolution precedence: higher wins. `User` > `Assembler` > `File(_)`.
    fn precedence(&self) -> u8 {
        match self {
            SymbolSource::File(_) => 0,
            SymbolSource::Assembler => 1,
            SymbolSource::User => 2,
        }
    }
}

/// A table of symbolic names mapped to addresses, tagged by the source that
/// contributed each mapping. A name may have at most one live entry per
/// source; `address_for` resolves the highest-precedence entry, while
/// `names_for` surfaces every source's entry with no shadowing.
#[derive(Debug, Clone)]
pub struct SymbolTable {
    by_name: HashMap<String, Vec<(SymbolSource, u16)>>,
    by_address: HashMap<u16, Vec<(String, SymbolSource)>>,
}

impl SymbolTable {

    /// Removes the `(name, source)` reverse pointer from `address`'s bucket
    /// in `by_address`, dropping the bucket entirely once empty. Takes
    /// `by_address` directly (rather than `&mut self`) so callers can hold
    /// a live borrow into `by_name` at the same time.
    fn evict_from_by_address(
        by_address: &mut HashMap<u16, Vec<(String, SymbolSource)>>,
        name: &str,
        source: &SymbolSource,
        address: u16,
    ) {
        if let Some(bucket) = by_address.get_mut(&address) {
            bucket.retain(|(n, s)| !(n == name && s == source));
            if bucket.is_empty() {
                by_address.remove(&address);
            }
        }
    }

    /// Constructs a new symbol table.
    pub fn new() -> Self {
        Self {
            by_name: HashMap::new(),
            by_address: HashMap::new(),
        }
    }

    /// Inserts the mapping `name -> address` into the table, tagged as
    /// `SymbolSource::User`.
    pub fn insert(&mut self, name: String, address: u16) {
        self.insert_tagged(name, address, SymbolSource::User);
    }

    /// Inserts the mapping `name -> address` into the table under `source`.
    /// If `source` already has a live entry for `name`, it is updated in
    /// place (including relocating its `by_address` reverse pointer) rather
    /// than appended.
    pub fn insert_tagged(&mut self, name: String, address: u16, source: SymbolSource) {
        let entries = self.by_name.entry(name.clone()).or_default();
        if let Some(entry) = entries.iter_mut().find(|(s, _)| *s == source) {
            if entry.1 == address {
                return;
            }
            let old_address = entry.1;
            entry.1 = address;
            Self::evict_from_by_address(&mut self.by_address, &name, &source, old_address);
        } else {
            entries.push((source.clone(), address));
        }
        self.by_address.entry(address).or_default().push((name, source));
    }

    /// Inserts all mappings from `source`, preserving each entry's own tag.
    pub fn insert_from(&mut self, source: &SymbolTable) {
        for (name, entry_source, address) in source.iter() {
            self.insert_tagged(name.to_string(), address, entry_source.clone());
        }
    }

    /// Removes any existing `SymbolSource::User` mapping for `name` in the
    /// table. Silently ignores requests to remove a name that has no such
    /// mapping.
    pub fn remove(&mut self, name: &str) {
        self.remove_tagged(name, &SymbolSource::User);
    }

    /// Removes any existing mapping for `name` tagged with `source`.
    /// Silently ignores requests to remove a mapping that doesn't exist.
    pub fn remove_tagged(&mut self, name: &str, source: &SymbolSource) {
        let Some(entries) = self.by_name.get_mut(name) else {
            return;
        };
        if let Some(pos) = entries.iter().position(|(s, _)| s == source) {
            let (_, address) = entries.remove(pos);
            let now_empty = entries.is_empty();
            Self::evict_from_by_address(&mut self.by_address, name, source, address);
            if now_empty {
                self.by_name.remove(name);
            }
        }
    }

    /// Clears all mappings from this table, regardless of source.
    pub fn clear(&mut self) {
        self.by_name.clear();
        self.by_address.clear();
    }

    /// Removes every live entry tagged with `source` (for `File`, an exact
    /// match of the canonicalized path), leaving all other sources' entries
    /// untouched.
    pub fn clear_source(&mut self, source: &SymbolSource) {
        let mut emptied = Vec::new();
        for (name, entries) in self.by_name.iter_mut() {
            if let Some(pos) = entries.iter().position(|(s, _)| s == source) {
                let (_, address) = entries.remove(pos);
                Self::evict_from_by_address(&mut self.by_address, name, source, address);
            }
            if entries.is_empty() {
                emptied.push(name.clone());
            }
        }
        for name in emptied {
            self.by_name.remove(&name);
        }
    }

    /// Returns the number of live name -> address mappings in this table,
    /// counting each source's entry separately.
    pub fn len(&self) -> usize {
        self.by_name.values().map(|entries| entries.len()).sum()
    }

    /// Returns `true` if this table has no live mappings.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Gets the address mapped by `name`, resolving the highest-precedence
    /// live entry (`User` > `Assembler` > `File(_)`). Ties within the same
    /// tier resolve to whichever entry is encountered first.
    pub fn address_for(&self, name: &str) -> Option<u16> {
        let entries = self.by_name.get(name)?;
        let mut best: Option<(u8, u16)> = None;
        for (source, address) in entries {
            let precedence = source.precedence();
            if best.is_none_or(|(best_precedence, _)| precedence > best_precedence) {
                best = Some((precedence, *address));
            }
        }
        best.map(|(_, address)| address)
    }

    /// Gets an iterator for the names mapped to `address`, across every
    /// source with a live entry there — no precedence shadowing.
    pub fn names_for(&self, address: u16) -> impl Iterator<Item = &str> {
        self.by_address
            .get(&address)
            .into_iter()
            .flatten()
            .map(|(name, _)| name.as_str())
    }

    /// Returns the unique canonicalized paths currently contributing at
    /// least one live `File`-sourced symbol.
    pub fn file_sources(&self) -> impl Iterator<Item = &Path> {
        let mut seen = std::collections::HashSet::new();
        self.by_name
            .values()
            .flat_map(|entries| entries.iter())
            .filter_map(move |(source, _)| match source {
                SymbolSource::File(path) if seen.insert(path.as_path()) => Some(path.as_path()),
                _ => None,
            })
    }

    /// Returns `true` if `path` is currently contributing at least one live
    /// `File`-sourced symbol.
    pub fn has_file_source(&self, path: &Path) -> bool {
        self.by_name
            .values()
            .flat_map(|entries| entries.iter())
            .any(|(source, _)| matches!(source, SymbolSource::File(p) if p.as_path() == path))
    }

    /// Returns an iterator over every live entry in the table.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &SymbolSource, u16)> {
        self.by_name.iter().flat_map(|(name, entries)| {
            entries
                .iter()
                .map(move |(source, address)| (name.as_str(), source, *address))
        })
    }

}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}


/// Parses a VICE monitor labels file and inserts each label into a
/// `SymbolTable`, tagged with `SymbolSource::File` using the path's
/// canonicalized, absolute form.
pub async fn load_vice_labels<P: AsRef<Path>>(path: P) -> Result<SymbolTable, &'static str> {
    let canonical = fs::canonicalize(&path)
        .await
        .map_err(|_| "failed to resolve labels file path")?;
    let contents = fs::read_to_string(&path)
        .await
        .map_err(|_| "failed to read labels file")?;
    parse_vice_labels(&contents, SymbolSource::File(canonical))
}

fn parse_vice_labels(contents: &str, source: SymbolSource) -> Result<SymbolTable, &'static str> {
    let mut table = SymbolTable::default();

    for line in contents.lines() {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let mut tokens = line.split_whitespace();

        let cmd = tokens.next().ok_or("malformed line: missing command")?;
        if cmd != "al" {
            return Err("malformed line: expected 'al' command");
        }

        let addr_tok = tokens.next().ok_or("malformed line: missing address")?;
        let hex_part = match addr_tok.split_once(':') {
            Some((_space, hex)) => hex,
            None => addr_tok,
        };
        let address = u16::from_str_radix(hex_part, 16).map_err(|_| "malformed line: invalid hex address")?;

        let label_tok = tokens.next().ok_or("malformed line: missing label name")?;
        let label = label_tok.strip_prefix('.').unwrap_or(label_tok);

        table.insert_tagged(label.to_string(), address, source.clone());
    }

    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile;

    #[test]
    fn insert_multiple_names_same_address() {
        let mut table = SymbolTable::default();
        table.insert("foo".to_string(), 0xBEEF);
        table.insert("bar".to_string(), 0xBEEF);
        assert_eq!(table.address_for("foo"), Some(0xBEEF));
        assert_eq!(table.address_for("bar"), Some(0xBEEF));
        let names: Vec<&str> = table.names_for(0xBEEF).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"bar"));
    }

    #[test]
    fn insert_silently_accepts_existing_name_with_same_address() {
        let mut table = SymbolTable::default();
        table.insert("foo".to_string(), 0xBEEF);
        table.insert("foo".to_string(), 0xBEEF);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn insert_same_name_multiple_addresses() {
        let mut table = SymbolTable::default();
        // when same name is inserted twice, last insert wins
        table.insert("foo".to_string(), 0xDEAD);
        table.insert("foo".to_string(), 0xBEEF);
        assert_eq!(table.address_for("foo"), Some(0xBEEF));
        let names: Vec<&str> = table.names_for(0xBEEF).collect();
        assert!(names.contains(&"foo"));
    }

    #[test]
    fn insert_moving_name_evicts_old_address_ghost() {
        // Regression test: a name moving to a new address must not leave a
        // stale entry behind at its old address.
        let mut table = SymbolTable::default();
        table.insert("foo".to_string(), 0xDEAD);
        table.insert("foo".to_string(), 0xBEEF);
        let names_at_old: Vec<&str> = table.names_for(0xDEAD).collect();
        assert!(!names_at_old.contains(&"foo"));
        let names_at_new: Vec<&str> = table.names_for(0xBEEF).collect();
        assert!(names_at_new.contains(&"foo"));
    }

    #[test]
    fn multi_source_coexistence_and_precedence() {
        let mut table = SymbolTable::default();
        let file_source = SymbolSource::File(PathBuf::from("/some/labels.lbl"));
        table.insert_tagged("foo".to_string(), 0x1000, file_source.clone());
        table.insert_tagged("foo".to_string(), 0x2000, SymbolSource::Assembler);

        assert_eq!(table.address_for("foo"), Some(0x2000));
        let names_at_file_addr: Vec<&str> = table.names_for(0x1000).collect();
        assert!(names_at_file_addr.contains(&"foo"));
        let names_at_assembler_addr: Vec<&str> = table.names_for(0x2000).collect();
        assert!(names_at_assembler_addr.contains(&"foo"));
    }

    #[test]
    fn user_source_wins_full_precedence() {
        let mut table = SymbolTable::default();
        let file_source = SymbolSource::File(PathBuf::from("/some/labels.lbl"));
        table.insert_tagged("foo".to_string(), 0x1000, file_source);
        table.insert_tagged("foo".to_string(), 0x2000, SymbolSource::Assembler);
        table.insert_tagged("foo".to_string(), 0x3000, SymbolSource::User);

        assert_eq!(table.address_for("foo"), Some(0x3000));
    }

    #[test]
    fn clear_source_removes_only_targeted_source() {
        let mut table = SymbolTable::default();
        let file_a = SymbolSource::File(PathBuf::from("/a.lbl"));
        table.insert_tagged("foo".to_string(), 0x1000, file_a.clone());
        table.insert_tagged("foo".to_string(), 0x2000, SymbolSource::Assembler);
        table.insert_tagged("bar".to_string(), 0x3000, file_a.clone());

        table.clear_source(&SymbolSource::Assembler);

        assert_eq!(table.address_for("foo"), Some(0x1000));
        assert_eq!(table.address_for("bar"), Some(0x3000));

        table.clear_source(&file_a);
        assert_eq!(table.address_for("foo"), None);
        assert_eq!(table.address_for("bar"), None);
    }

    #[test]
    fn file_sources_and_has_file_source() {
        let mut table = SymbolTable::default();
        let path_a = PathBuf::from("/a.lbl");
        let path_b = PathBuf::from("/b.lbl");
        table.insert_tagged("foo".to_string(), 0x1000, SymbolSource::File(path_a.clone()));
        table.insert_tagged("bar".to_string(), 0x2000, SymbolSource::File(path_b.clone()));

        assert!(table.has_file_source(&path_a));
        assert!(table.has_file_source(&path_b));
        let mut sources: Vec<&Path> = table.file_sources().collect();
        sources.sort();
        assert_eq!(sources, vec![path_a.as_path(), path_b.as_path()]);

        table.clear_source(&SymbolSource::File(path_a.clone()));
        assert!(!table.has_file_source(&path_a));
        assert!(table.has_file_source(&path_b));
    }

    #[test]
    fn insert_from_preserves_source_tags() {
        let mut source = SymbolTable::default();
        let file_a = SymbolSource::File(PathBuf::from("/a.lbl"));
        source.insert_tagged("foo".to_string(), 0x1000, file_a.clone());
        source.insert_tagged("foo".to_string(), 0x2000, SymbolSource::Assembler);

        let mut dest = SymbolTable::default();
        dest.insert_tagged("foo".to_string(), 0x3000, SymbolSource::User);
        dest.insert_from(&source);

        // User entry from dest survives and still wins precedence.
        assert_eq!(dest.address_for("foo"), Some(0x3000));
        // But the merged File/Assembler entries are present too.
        assert!(dest.names_for(0x1000).any(|n| n == "foo"));
        assert!(dest.names_for(0x2000).any(|n| n == "foo"));
    }

    #[test]
    fn len_counts_live_mappings() {
        let mut table = SymbolTable::default();
        assert_eq!(table.len(), 0);
        assert!(table.is_empty());
        table.insert("foo".to_string(), 0xBEEF);
        table.insert("bar".to_string(), 0xBEEF);
        assert_eq!(table.len(), 2);
        table.remove("foo");
        assert_eq!(table.len(), 1);
        assert!(!table.is_empty());
    }

    #[test]
    fn remove_name_expunges_symbol() {
        let mut table = SymbolTable::default();
        table.insert("foo".to_string(), 0xBEEF);
        table.insert("bar".to_string(), 0xBEEF);
        table.remove("foo");
        assert_eq!(table.address_for("foo"), None);
        let names: Vec<&str> = table.names_for(0xBEEF).collect();
        assert!(!names.contains(&"foo"));
        assert!(names.contains(&"bar"));
    }

    const VICE_LABELS: &str = "al 00EF3A .COLD_START
                               al 00E517 .CONINT
                               al 00E10D .GIVAYF
                               al 00D35E .RESTART
                               al 000200 .__BSS_LOAD__
                               al 000200 .__BSS_RUN__
                               al 000000 .__BSS_SIZE__
                               al 00F218 .noop_isr
                               al 00F208 .@done
                               al 00F1FD .@next
                               al 00F1F4 .cls_sequence";

    #[test]
    fn parse_vice_labels_str() {
        let table = parse_vice_labels(VICE_LABELS, SymbolSource::User).unwrap();
        assert_eq!(table.address_for("COLD_START"), Some(0xEF3A));
        assert_eq!(table.address_for("cls_sequence"), Some(0xF1F4));
    }

    #[test]
    fn parse_vice_labels_garbage() {
        let err = parse_vice_labels("garbage", SymbolSource::User).unwrap_err();
        assert!(err.contains("malformed"));
    }

    #[tokio::test]
    async fn load_vice_labels_file() {
        let path  = tempfile::Builder::new().suffix(".lbl").tempfile().unwrap();
        tokio::fs::write(&path.path(), VICE_LABELS.as_bytes()).await.unwrap();
        let table = load_vice_labels(path.path()).await.unwrap();
        assert_eq!(table.address_for("COLD_START"), Some(0xEF3A));
        assert_eq!(table.address_for("cls_sequence"), Some(0xF1F4));

        let canonical = tokio::fs::canonicalize(path.path()).await.unwrap();
        for (_, source, _) in table.iter() {
            assert_eq!(source, &SymbolSource::File(canonical.clone()));
        }
    }

}
