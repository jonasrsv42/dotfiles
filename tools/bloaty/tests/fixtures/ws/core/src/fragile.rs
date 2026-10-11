//! The fragile file: dense panic sites.

use std::collections::HashMap;

pub struct Registry {
    names: HashMap<u32, String>,
    order: Vec<u32>,
}

impl Registry {
    pub fn new() -> Self {
        Registry {
            names: HashMap::new(),
            order: Vec::new(),
        }
    }

    pub fn add(&mut self, id: u32, name: &str) {
        assert!(!name.is_empty(), "empty name");
        if self.names.insert(id, name.to_string()).is_some() {
            panic!("duplicate id {id}");
        }
        self.order.push(id);
    }

    pub fn name(&self, id: u32) -> &str {
        self.names.get(&id).expect("unknown id")
    }

    pub fn first(&self) -> &str {
        let id = self.order.first().unwrap();
        self.names.get(id).unwrap()
    }

    pub fn parse(&mut self, line: &str) {
        let (id, name) = line.split_once('=').unwrap();
        let id: u32 = id.trim().parse().expect("numeric id");
        self.add(id, name.trim());
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }
}
