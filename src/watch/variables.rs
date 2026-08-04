use super::expr::Operand;
use std::collections::HashMap;

/// A collection of name-to-operand mappings for Watchpoint variables.
pub struct Variables {
    map: HashMap<String, Operand>,
    /// Variable names in ID order, so a variable's name can be recovered from
    /// its [`Operand`] ID (e.g. for display purposes).
    names: Vec<String>,
}

impl Variables {

    /// Creates a new variables collection.
    pub fn new() -> Self {
        Self { map: HashMap::new(), names: Vec::new() }
    }

    /// Gets the mapping for `name` to the corresponding [`Operand`], if any.
    pub fn get(&self, name: &str) -> Option<Operand> {
        self.map.get(name).copied()
    }

    /// Gets the mapping for `name` to the corresponding [`Operand`], creating the mapping if
    /// it does not exist.
    pub fn get_or_create(&mut self, name: &str) -> Operand {
        if let Some(&id) = self.map.get(name) {
            return id;
        }
        let id = self.names.len() as Operand;
        self.names.push(name.to_string());
        self.map.insert(name.to_string(), id);
        id
    }

    /// Gets the length (size) of the mapping table.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns all variable names, in ID order (so a name's index in the
    /// returned slice is its [`Operand`] ID).
    pub fn names(&self) -> &[String] {
        &self.names
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

    #[test]
    fn names_are_returned_in_id_order() {
        let mut vars = Variables::new();
        vars.get_or_create("x");
        vars.get_or_create("y");
        assert_eq!(vars.names(), &["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn names_are_unaffected_by_repeated_get_or_create() {
        let mut vars = Variables::new();
        vars.get_or_create("x");
        vars.get_or_create("y");
        vars.get_or_create("x");
        assert_eq!(vars.names(), &["x".to_string(), "y".to_string()]);
    }
}
