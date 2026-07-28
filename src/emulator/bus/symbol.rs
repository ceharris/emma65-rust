//! Support for symbolic labels assigned to addresses.

use std::collections::HashMap;
use std::path::Path;
use tokio::fs;

/// A symbol consisting of a name and associated address.
#[derive(Debug, Clone, PartialEq)]
struct Symbol {
    name: String,
    address: u16,
}

#[derive(Debug, Clone)]
pub struct SymbolTable {
    symbols: Vec<Option<Symbol>>,
    by_name: HashMap<String, usize>,
    by_address: HashMap<u16, Vec<usize>>,
}

impl SymbolTable {

    /// Constructs a new symbol table.   
    pub fn new() -> Self {
        Self {
            symbols: Vec::new(),
            by_name: HashMap::new(),
            by_address: HashMap::new(),
        }
    }

    /// Inserts the mapping `name -> address` into the table. 
    pub fn insert(&mut self, name: String, address: u16) {
        let idx = self.symbols.len();
        // Could avoid clone here by changing to `name: Rc<str>` in Symbol; probably not worth it
        self.by_name.insert(name.clone(), idx);
        self.by_address.entry(address).or_default().push(idx);
        self.symbols.push(Some(Symbol { name, address }));
    }

    /// Inserts all mappings from `source` into the table.
    pub fn insert_from(&mut self, source: &SymbolTable) {
        for symbol in source.symbols.iter().as_ref() {
            if let Some(symbol) = symbol {
                self.insert(symbol.name.to_string(), symbol.address);
            }
        }
    }

    /// Removes any existing mapping for `name` in the table.
    /// Silently ignores requests to remove a name that has no mapping.
    pub fn remove(&mut self, name: &str) {
        if let Some(idx) = self.by_name.get(name) {
            self.symbols[*idx] = None;
        }
    }

    /// Gets the address mapped by `name`, if any.
    pub fn address_for(&self, name: &str) -> Option<u16> {
        self.by_name.get(name).and_then(|&i| self.symbols[i].as_ref()).map(|s| s.address)
    }

    /// Gets an iterator for the names mapped to `address`.
    pub fn names_for(&self, address: u16) -> impl Iterator<Item = &str> {
        self.by_address
            .get(&address)
            .into_iter()
            .flatten()
            .filter_map(|i| self.symbols[*i].as_ref())
            .map(|s| s.name.as_str())
    }

}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}


/// Parses a VICE monitor labels file and inserts each label into a `SymbolTable`.
pub async fn load_vice_labels<P: AsRef<Path>>(path: P) -> Result<SymbolTable, &'static str> {
    let contents = fs::read_to_string(path)
        .await
        .map_err(|_| "failed to read labels file")?;
    parse_vice_labels(&contents)
}

fn parse_vice_labels(contents: &str) -> Result<SymbolTable, &'static str> {
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

        table.insert(label.to_string(), address);
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
    fn remove_name_expunges_symbol() {
        let mut table = SymbolTable::default();
        table.insert("foo".to_string(), 0xBEEF);
        table.insert("bar".to_string(), 0xBEEF);
        table.remove(&"foo");
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
        let table = parse_vice_labels(VICE_LABELS).unwrap();
        assert_eq!(table.address_for("COLD_START"), Some(0xEF3A));
        assert_eq!(table.address_for("cls_sequence"), Some(0xF1F4));
    }

    #[test]
    fn parse_vice_labels_garbage() {
        let err = parse_vice_labels("garbage").unwrap_err();
        assert!(err.contains("malformed"));
    }

    #[tokio::test]
    async fn load_vice_labels_file() {
        let path  = tempfile::Builder::new().suffix(".lbl").tempfile().unwrap();
        tokio::fs::write(&path.path(), VICE_LABELS.as_bytes()).await.unwrap();
        let table = load_vice_labels(path).await.unwrap();
        assert_eq!(table.address_for("COLD_START"), Some(0xEF3A));
        assert_eq!(table.address_for("cls_sequence"), Some(0xF1F4));
    }

}