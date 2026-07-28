//! Support for symbolic labels assigned to addresses.

use std::collections::HashMap;

/// A symbol consisting of a name and associated address.
#[derive(Debug, Clone, PartialEq)]
struct Symbol {
    name: String,
    address: u16,
}

#[derive(Clone)]
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

    /// Removes any existing mapping for `name` in the table.
    /// Silently ignores requests to remove a name that has no mapping.
    pub fn remove(&mut self, name: &str) {
        if let Some(idx) = self.by_name.get(name) {
            self.symbols[*idx] = None;
        }
    }

    /// Gets the address mapped by `name`, if any.
    pub fn address_for(&self, name: &str) -> Option<u16> {
        self.by_name.get(name)
            .map_or(None, |&i| self.symbols[i].as_ref())
            .map_or(None, |s| Some(s.address))
    }

    /// Gets an iterator for the names mapped to `address`.
    pub fn names_for(&self, address: u16) -> impl Iterator<Item = &str> {
        self.by_address
            .get(&address)
            .into_iter()
            .flatten()
            .filter(|&i| self.symbols[*i].is_some())
            .map(|i| self.symbols[*i].as_ref().unwrap())
            .map(|s| s.name.as_str())
    }

}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::emulator::bus::symbol::SymbolTable;

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

}