//! Named collections and their property graphs, available to the query layer.
//!
//! Each collection has a graph with the same name. Graph nodes are identified by
//! document id, so a graph match can be joined back to the documents of its
//! collection.

use std::collections::HashMap;

use tpt_search_core::{IndexError, Schema};
use tpt_search_graph::PropertyGraph;
use tpt_search_index::Collection;

/// A set of named collections, each with a property graph of the same name.
#[derive(Debug, Default)]
pub struct IndexRegistry {
    collections: HashMap<String, Collection>,
    graphs: HashMap<String, PropertyGraph>,
}

impl IndexRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a collection from `schema`, with an empty graph of the same name.
    /// Replaces any existing collection and graph with that name.
    pub fn create(&mut self, name: impl Into<String>, schema: Schema) -> Result<(), IndexError> {
        let name = name.into();
        let collection = Collection::new(schema)?;
        self.collections.insert(name.clone(), collection);
        self.graphs.insert(name, PropertyGraph::new());
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

    /// Looks up the graph of a collection.
    pub fn graph(&self, name: &str) -> Option<&PropertyGraph> {
        self.graphs.get(name)
    }

    /// Looks up the graph of a collection for writing.
    pub fn graph_mut(&mut self, name: &str) -> Option<&mut PropertyGraph> {
        self.graphs.get_mut(name)
    }

    /// Names of all registered collections, sorted.
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.collections.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }
}
