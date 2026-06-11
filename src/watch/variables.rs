use std::collections::HashMap;
use super::expr::Operand;

/// A collection of name-to-operand mappings for Watchpoint variables.
pub struct Variables {
    map: HashMap<String, Operand>,
}

impl Variables {

    /// Creates a new variables collection.
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    /// Gets the mapping for `name` to the corresponding [`Operand`], if any.
    pub fn get(&self, name: &str) -> Option<Operand> {
        self.map.get(name).copied()
    }

    /// Gets the mapping for `name` to the corresponding [`Operand`], creating the mapping if
    /// it does not exist.
    pub fn get_or_create(&mut self, name: &str) -> Operand {
        let next_id = self.map.len() as Operand;
        *self.map.entry(name.to_string()).or_insert(next_id)
    }

    /// Gets the length (size) of the mapping table.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Tests whether the mapping table is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

}

impl Default for Variables {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_on_empty_returns_none() {
        let vars = Variables::new();
        assert_eq!(vars.get("x"), None);
    }

    #[test]
    fn get_or_create_assigns_sequential_ids() {
        let mut vars = Variables::new();
        let id_x = vars.get_or_create("x");
        let id_y = vars.get_or_create("y");
        assert_ne!(id_x, id_y);
        assert_eq!(vars.len(), 2);
    }

    #[test]
    fn get_or_create_is_idempotent() {
        let mut vars = Variables::new();
        let id1 = vars.get_or_create("x");
        let id2 = vars.get_or_create("x");
        assert_eq!(id1, id2);
        assert_eq!(vars.len(), 1);
    }

    #[test]
    fn get_returns_id_after_create() {
        let mut vars = Variables::new();
        let id = vars.get_or_create("x");
        assert_eq!(vars.get("x"), Some(id));
    }
}
