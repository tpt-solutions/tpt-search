//! Named collections available to the query layer.

use std::collections::HashMap;

use tpt_search_core::{IndexError, Schema};
use tpt_search_index::Collection;

/// A set of named collections.
#[derive(Debug, Default)]
pub struct IndexRegistry {
    collections: HashMap<String, Collection>,
}

impl IndexRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a collection from `schema` under `name`. Replaces any existing collection with that name.
    pub fn create(&mut self, name: impl Into<String>, schema: Schema) -> Result<(), IndexError> {
        let collection = Collection::new(schema)?;
        self.collections.insert(name.into(), collection);
        Ok(())
    }

    /// Looks up a collection.
    pub fn get(&self, name: &str) -> Option<&Collection> {
        self.collections.get(name)
    }

    /// Looks up a collection for writing.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut Collection> {
        self.collections.get_mut(name)
    }

    /// Names of all registered collections, sorted.
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.collections.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }
}
