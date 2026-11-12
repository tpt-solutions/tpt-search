//! Python bindings for `tpt-search`.
//!
//! The module is `tpt_search`. An [`Engine`] holds named indexes. Schemas, documents and
//! results are plain Python dicts and lists; they cross the boundary as JSON, using the same
//! shapes as the HTTP API.
//!
//! ```python
//! from tpt_search import Engine
//!
//! engine = Engine()
//! engine.create_index("docs", {
//!     "name": "docs",
//!     "primary_key": "id",
//!     "fields": {
//!         "id": {"name": "id", "field_type": "Text", "indexed": False, "stored": True,
//!                "analyzer": None, "vector_config": None},
//!         "title": {"name": "title", "field_type": "Text", "indexed": True, "stored": True,
//!                   "analyzer": None, "vector_config": None},
//!     },
//! })
//! engine.upsert("docs", "d1", {"title": "common rare marker"})
//! engine.search("docs", 'MATCH(title, "rare")')["hits"][0]["id"]  # "d1"
//! ```

use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::PyModule;
use serde_json::{json, Value};
use tpt_search_api::convert::{document_from_json, document_to_json};
use tpt_search_api::{DEFAULT_LIMIT, MAX_LIMIT};
use tpt_search_core::{DocumentId, QueryError, Schema};
use tpt_search_query::{parse_query, IndexRegistry, QueryExecutor, QueryPlanner};

create_exception!(
    tpt_search,
    TptError,
    PyException,
    "Base class for tpt-search errors."
);
create_exception!(
    tpt_search,
    NotFoundError,
    TptError,
    "The index or document does not exist."
);
create_exception!(
    tpt_search,
    InvalidInputError,
    TptError,
    "The schema, document or query was rejected."
);
create_exception!(
    tpt_search,
    ConflictError,
    TptError,
    "An index with that name already exists."
);
create_exception!(
    tpt_search,
    QueryTimeoutError,
    TptError,
    "The query exceeded its time budget."
);

/// How long one search may run before it is abandoned. Matches the HTTP API.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// A failure, classified so it can become the matching Python exception.
#[derive(Debug)]
enum Failure {
    NotFound(String),
    Invalid(String),
    Conflict(String),
    Timeout(String),
}

type Outcome<T = ()> = Result<T, Failure>;

impl Failure {
    fn into_err(self) -> PyErr {
        match self {
            Failure::NotFound(m) => NotFoundError::new_err(m),
            Failure::Invalid(m) => InvalidInputError::new_err(m),
            Failure::Conflict(m) => ConflictError::new_err(m),
            Failure::Timeout(m) => QueryTimeoutError::new_err(m),
        }
    }
}

fn query_failure(error: QueryError) -> Failure {
    match error {
        QueryError::Timeout => Failure::Timeout(error.to_string()),
        QueryError::UnknownIndex(_) => Failure::NotFound(error.to_string()),
        other => Failure::Invalid(other.to_string()),
    }
}

/// Serializes a Python object to JSON text with the standard `json` module.
fn to_json(py: Python<'_>, object: &Bound<'_, PyAny>) -> PyResult<String> {
    PyModule::import(py, "json")?
        .call_method1("dumps", (object,))?
        .extract()
}

/// Parses JSON text into Python objects with the standard `json` module.
fn from_json<'py>(py: Python<'py>, text: &str) -> PyResult<Bound<'py, PyAny>> {
    PyModule::import(py, "json")?.call_method1("loads", (text,))
}

/// A set of named indexes. Methods are safe to call from several threads.
#[pyclass(module = "tpt_search")]
pub struct Engine {
    registry: RwLock<IndexRegistry>,
}

impl Engine {
    fn read(&self) -> RwLockReadGuard<'_, IndexRegistry> {
        self.registry.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> RwLockWriteGuard<'_, IndexRegistry> {
        self.registry.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Parses, plans, optimizes and runs a query, returning the API-shaped result as JSON.
    fn run_search(&self, index: &str, text: &str, limit: usize) -> Outcome<Value> {
        let registry = self.read();
        let collection = registry
            .get(index)
            .ok_or_else(|| Failure::NotFound(format!("no index `{index}`")))?;
        let parsed = parse_query(text).map_err(query_failure)?;
        let plan = QueryPlanner::new()
            .plan(index, collection, &parsed)
            .map_err(query_failure)?;
        let plan = QueryPlanner::new().optimize(plan.with_limit(limit), collection);
        let result = QueryExecutor::new(&registry)
            .with_timeout(QUERY_TIMEOUT)
            .execute(&plan)
            .map_err(query_failure)?;

        let hits: Vec<Value> = result
            .documents
            .iter()
            .map(|doc| {
                let spans = result
                    .highlights
                    .get(&doc.id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let highlights: Vec<Value> = spans
                    .iter()
                    .map(|h| json!({ "field": h.field, "spans": h.positions }))
                    .collect();
                json!({
                    "id": doc.id.as_str(),
                    "document": document_to_json(doc)["fields"].clone(),
                    "highlights": highlights,
                })
            })
            .collect();
        Ok(json!({
            "total_hits": result.total_hits,
            "took_ms": result.took_ms,
            "hits": hits,
        }))
    }
}

#[pymethods]
impl Engine {
    #[new]
    fn new() -> Self {
        Self {
            registry: RwLock::new(IndexRegistry::new()),
        }
    }

    /// Creates an index from a schema dict. Raises `ConflictError` if the name is taken.
    fn create_index(&self, py: Python<'_>, name: &str, schema: &Bound<'_, PyAny>) -> PyResult<()> {
        let text = to_json(py, schema)?;
        let schema: Schema = serde_json::from_str(&text)
            .map_err(|e| Failure::Invalid(format!("invalid schema: {e}")).into_err())?;
        let mut registry = self.write();
        if registry.get(name).is_some() {
            return Err(Failure::Conflict(format!("index `{name}` already exists")).into_err());
        }
        registry
            .create(name, schema)
            .map_err(|e| Failure::Invalid(e.to_string()).into_err())
    }

    /// Removes an index. Raises `NotFoundError` if it does not exist.
    fn drop_index(&self, name: &str) -> PyResult<()> {
        if self.write().remove(name) {
            Ok(())
        } else {
            Err(Failure::NotFound(format!("no index `{name}`")).into_err())
        }
    }

    /// Names of all indexes, sorted.
    fn indexes(&self) -> Vec<String> {
        self.read().names().into_iter().map(str::to_owned).collect()
    }

    /// Inserts or replaces a document. `document` is a dict of field values.
    fn upsert(
        &self,
        py: Python<'_>,
        index: &str,
        id: &str,
        document: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let body: Value = serde_json::from_str(&to_json(py, document)?)
            .map_err(|e| Failure::Invalid(format!("invalid document: {e}")).into_err())?;
        let mut registry = self.write();
        let collection = registry
            .get_mut(index)
            .ok_or_else(|| Failure::NotFound(format!("no index `{index}`")).into_err())?;
        let doc = document_from_json(collection.schema(), id, &body)
            .map_err(|e| Failure::Invalid(e).into_err())?;
        collection
            .upsert(&doc)
            .map(|_| ())
            .map_err(|e| Failure::Invalid(e.to_string()).into_err())
    }

    /// Reads a document as a dict, or returns `None` if it does not exist.
    fn get<'py>(
        &self,
        py: Python<'py>,
        index: &str,
        id: &str,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let registry = self.read();
        let collection = registry
            .get(index)
            .ok_or_else(|| Failure::NotFound(format!("no index `{index}`")).into_err())?;
        match collection.get(&DocumentId::from(id)) {
            None => Ok(None),
            Some(doc) => {
                let text = document_to_json(&doc).to_string();
                from_json(py, &text).map(Some)
            }
        }
    }

    /// Deletes a document. Returns whether it was present.
    fn delete(&self, index: &str, id: &str) -> PyResult<bool> {
        let mut registry = self.write();
        let collection = registry
            .get_mut(index)
            .ok_or_else(|| Failure::NotFound(format!("no index `{index}`")).into_err())?;
        Ok(collection.delete(&DocumentId::from(id)))
    }

    /// Runs a query and returns `{"total_hits", "took_ms", "hits"}`. `limit` 0 selects the
    /// default. The search runs with the GIL released, so other Python threads keep going.
    #[pyo3(signature = (index, query, limit = 0))]
    fn search<'py>(
        &self,
        py: Python<'py>,
        index: &str,
        query: &str,
        limit: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        let limit = match limit {
            0 => DEFAULT_LIMIT,
            n if n <= MAX_LIMIT => n,
            _ => {
                return Err(
                    Failure::Invalid(format!("`limit` must be between 1 and {MAX_LIMIT}"))
                        .into_err(),
                )
            }
        };
        let body = py
            .allow_threads(|| self.run_search(index, query, limit))
            .map_err(Failure::into_err)?;
        from_json(py, &body.to_string())
    }
}

#[pymodule]
fn tpt_search(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Engine>()?;
    m.add("TptError", py.get_type::<TptError>())?;
    m.add("NotFoundError", py.get_type::<NotFoundError>())?;
    m.add("InvalidInputError", py.get_type::<InvalidInputError>())?;
    m.add("ConflictError", py.get_type::<ConflictError>())?;
    m.add("QueryTimeoutError", py.get_type::<QueryTimeoutError>())?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
