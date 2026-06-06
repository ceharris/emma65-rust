use std::collections::HashMap;
use super::expr::Operand;

pub struct Variables {
    map: HashMap<String, Operand>,
}

impl Variables {

    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    pub fn get(&self, name: &str) -> Option<Operand> {
        self.map.get(name).copied()
    }

    pub fn get_or_create(&mut self, name: &str) -> Operand {
        let next_id = self.map.len() as Operand;
        *self.map.entry(name.to_string()).or_insert(next_id)
    }
    
    pub fn len(&self) -> usize {
        self.map.len()
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
