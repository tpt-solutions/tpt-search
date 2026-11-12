//! C ABI for `tpt-search`.
//!
//! The surface is a handful of `extern "C"` functions over an opaque engine handle. Documents
//! and results cross the boundary as UTF-8 JSON, using the same shapes as the HTTP API, so a
//! C, Python or Go binding needs only a JSON parser on its side.
//!
//! Conventions:
//!
//! * Every function returns a status code. `TPT_OK` (0) is success; anything else is a
//!   failure, and [`tpt_last_error`] describes it for the calling thread.
//! * Strings passed in are NUL-terminated UTF-8 and only borrowed for the call.
//! * Strings returned through an out parameter are owned by the caller and must be released
//!   with [`tpt_string_free`].
//! * A panic inside the library is caught and reported as `TPT_ERR_INTERNAL`; it never
//!   unwinds into the caller.
//! * An engine may be shared between threads. Each call takes the engine's lock for the
//!   duration of the call.
//!
//! The header in `include/tpt_search.h` is written by hand to match these declarations.

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::ffi::{c_char, c_int, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use serde_json::{json, Value};
use tpt_search_api::convert::{document_from_json, document_to_json};
use tpt_search_api::{DEFAULT_LIMIT, MAX_LIMIT};
use tpt_search_core::{DocumentId, QueryError, Schema};
use tpt_search_index::Highlight;
use tpt_search_query::{parse_query, IndexRegistry, QueryExecutor, QueryPlanner};

/// Success.
pub const TPT_OK: c_int = 0;
/// A required pointer was null, or a string was not valid UTF-8.
pub const TPT_ERR_INVALID_ARGUMENT: c_int = 1;
/// The named index or document does not exist.
pub const TPT_ERR_NOT_FOUND: c_int = 2;
/// The schema, document or query was rejected.
pub const TPT_ERR_INVALID_INPUT: c_int = 3;
/// An index with that name already exists.
pub const TPT_ERR_CONFLICT: c_int = 4;
/// The library hit an internal error, such as a panic. The engine may be unusable.
pub const TPT_ERR_INTERNAL: c_int = 5;
/// The query exceeded its time budget.
pub const TPT_ERR_TIMEOUT: c_int = 6;

/// Largest `limit` accepted by [`tpt_search`]. Matches the HTTP API.
pub const TPT_MAX_LIMIT: usize = MAX_LIMIT;

/// How long one search may run before it is abandoned.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

const VERSION: &[u8] = concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes();

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

/// Opaque engine handle. Created by [`tpt_engine_new`] and released by [`tpt_engine_free`].
pub struct TptEngine {
    registry: RwLock<IndexRegistry>,
}

impl TptEngine {
    fn read(&self) -> RwLockReadGuard<'_, IndexRegistry> {
        self.registry.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> RwLockWriteGuard<'_, IndexRegistry> {
        self.registry.write().unwrap_or_else(|e| e.into_inner())
    }
}

/// A failure with the status code it maps to.
struct Failure {
    code: c_int,
    message: String,
}

type CallResult<T = ()> = Result<T, Failure>;

impl Failure {
    fn new(code: c_int, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn argument(message: impl Into<String>) -> Self {
        Self::new(TPT_ERR_INVALID_ARGUMENT, message)
    }

    fn input(message: impl Into<String>) -> Self {
        Self::new(TPT_ERR_INVALID_INPUT, message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(TPT_ERR_NOT_FOUND, message)
    }
}

fn set_error(message: &str) {
    // Interior NULs cannot be represented in a C string, so they are replaced.
    let text = CString::new(message.replace('\0', "?")).unwrap_or_default();
    LAST_ERROR.with(|slot| *slot.borrow_mut() = text);
}

/// Runs a call body, converting its outcome and any panic into a status code.
fn guard(body: impl FnOnce() -> CallResult) -> c_int {
    let outcome = catch_unwind(AssertUnwindSafe(body))
        .unwrap_or_else(|_| Err(Failure::new(TPT_ERR_INTERNAL, "internal panic")));
    match outcome {
        Ok(()) => {
            set_error("");
            TPT_OK
        }
        Err(failure) => {
            set_error(&failure.message);
            failure.code
        }
    }
}

/// Borrows the engine behind a handle.
///
/// # Safety
/// `engine` must be null or a live handle from [`tpt_engine_new`].
unsafe fn engine_ref<'a>(engine: *const TptEngine) -> CallResult<&'a TptEngine> {
    engine
        .as_ref()
        .ok_or_else(|| Failure::argument("engine is null"))
}

/// Reads a borrowed C string as UTF-8.
///
/// # Safety
/// `ptr` must be null or point to a NUL-terminated string that stays valid for the call.
unsafe fn read_str<'a>(ptr: *const c_char, what: &str) -> CallResult<&'a str> {
    if ptr.is_null() {
        return Err(Failure::argument(format!("{what} is null")));
    }
    CStr::from_ptr(ptr)
        .to_str()
        .map_err(|_| Failure::argument(format!("{what} is not valid UTF-8")))
}

/// Stores an owned string in a caller's out parameter.
///
/// # Safety
/// `out` must be null or a valid pointer to writable storage for one `*mut c_char`.
unsafe fn write_out(out: *mut *mut c_char, text: String) -> CallResult {
    if out.is_null() {
        return Err(Failure::argument("output pointer is null"));
    }
    let owned = CString::new(text).map_err(|_| Failure::internal("output contains NUL"))?;
    *out = owned.into_raw();
    Ok(())
}

impl Failure {
    fn internal(message: impl Into<String>) -> Self {
        Self::new(TPT_ERR_INTERNAL, message)
    }
}

/// Maps a query error to its status code.
fn query_failure(error: QueryError) -> Failure {
    match error {
        QueryError::Timeout => Failure::new(TPT_ERR_TIMEOUT, error.to_string()),
        QueryError::UnknownIndex(_) => Failure::not_found(error.to_string()),
        other => Failure::input(other.to_string()),
    }
}

/// Creates an engine with no indexes. Release it with [`tpt_engine_free`].
#[no_mangle]
pub extern "C" fn tpt_engine_new() -> *mut TptEngine {
    Box::into_raw(Box::new(TptEngine {
        registry: RwLock::new(IndexRegistry::new()),
    }))
}

/// Releases an engine and every index it holds. Passing null does nothing.
///
/// # Safety
/// `engine` must be null or a handle from [`tpt_engine_new`] that has not been freed.
#[no_mangle]
pub unsafe extern "C" fn tpt_engine_free(engine: *mut TptEngine) {
    if !engine.is_null() {
        drop(Box::from_raw(engine));
    }
}

/// Creates an index from a JSON schema, the same document the HTTP API accepts.
///
/// Returns `TPT_ERR_CONFLICT` if an index with that name already exists.
///
/// # Safety
/// `engine` must be a live handle. `name` and `schema_json` must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn tpt_create_index(
    engine: *const TptEngine,
    name: *const c_char,
    schema_json: *const c_char,
) -> c_int {
    guard(|| {
        let engine = engine_ref(engine)?;
        let name = read_str(name, "index name")?;
        let schema: Schema = serde_json::from_str(read_str(schema_json, "schema")?)
            .map_err(|e| Failure::input(format!("invalid schema: {e}")))?;
        let mut registry = engine.write();
        if registry.get(name).is_some() {
            return Err(Failure::new(
                TPT_ERR_CONFLICT,
                format!("index `{name}` already exists"),
            ));
        }
        registry
            .create(name, schema)
            .map_err(|e| Failure::input(e.to_string()))
    })
}

/// Removes an index and its graph. Returns `TPT_ERR_NOT_FOUND` if it does not exist.
///
/// # Safety
/// `engine` must be a live handle. `name` must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn tpt_drop_index(engine: *const TptEngine, name: *const c_char) -> c_int {
    guard(|| {
        let engine = engine_ref(engine)?;
        let name = read_str(name, "index name")?;
        if engine.write().remove(name) {
            Ok(())
        } else {
            Err(Failure::not_found(format!("no index `{name}`")))
        }
    })
}

/// Inserts or replaces a document. `body_json` is a JSON object of field values, converted
/// according to the schema, exactly as the HTTP API reads them.
///
/// # Safety
/// `engine` must be a live handle. All strings must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn tpt_upsert(
    engine: *const TptEngine,
    index: *const c_char,
    id: *const c_char,
    body_json: *const c_char,
) -> c_int {
    guard(|| {
        let engine = engine_ref(engine)?;
        let index = read_str(index, "index name")?;
        let id = read_str(id, "document id")?;
        let body: Value = serde_json::from_str(read_str(body_json, "document")?)
            .map_err(|e| Failure::input(format!("invalid document JSON: {e}")))?;
        let mut registry = engine.write();
        let collection = registry
            .get_mut(index)
            .ok_or_else(|| Failure::not_found(format!("no index `{index}`")))?;
        let doc = document_from_json(collection.schema(), id, &body).map_err(Failure::input)?;
        collection
            .upsert(&doc)
            .map_err(|e| Failure::input(e.to_string()))?;
        Ok(())
    })
}

/// Deletes a document. If `existed` is not null it receives whether the document was present.
///
/// # Safety
/// `engine` must be a live handle. `index` and `id` must be NUL-terminated UTF-8. `existed`
/// must be null or point to writable storage for one `bool`.
#[no_mangle]
pub unsafe extern "C" fn tpt_delete(
    engine: *const TptEngine,
    index: *const c_char,
    id: *const c_char,
    existed: *mut bool,
) -> c_int {
    guard(|| {
        let engine = engine_ref(engine)?;
        let index = read_str(index, "index name")?;
        let id = read_str(id, "document id")?;
        let mut registry = engine.write();
        let collection = registry
            .get_mut(index)
            .ok_or_else(|| Failure::not_found(format!("no index `{index}`")))?;
        let was_present = collection.delete(&DocumentId::from(id));
        if let Some(slot) = existed.as_mut() {
            *slot = was_present;
        }
        Ok(())
    })
}

/// Reads one document as JSON, in the same shape as the HTTP API. Returns
/// `TPT_ERR_NOT_FOUND` if the index or the document is missing.
///
/// # Safety
/// `engine` must be a live handle. `index` and `id` must be NUL-terminated UTF-8. `out_json`
/// must be writable; on success it receives a string to release with [`tpt_string_free`].
#[no_mangle]
pub unsafe extern "C" fn tpt_get(
    engine: *const TptEngine,
    index: *const c_char,
    id: *const c_char,
    out_json: *mut *mut c_char,
) -> c_int {
    guard(|| {
        let engine = engine_ref(engine)?;
        let index = read_str(index, "index name")?;
        let id = read_str(id, "document id")?;
        let registry = engine.read();
        let collection = registry
            .get(index)
            .ok_or_else(|| Failure::not_found(format!("no index `{index}`")))?;
        let doc = collection
            .get(&DocumentId::from(id))
            .ok_or_else(|| Failure::not_found(format!("no document `{id}`")))?;
        write_out(out_json, document_to_json(&doc).to_string())
    })
}

/// Runs a query and returns the result as JSON:
/// `{"total_hits", "took_ms", "hits": [{"id", "document", "highlights"}]}`.
///
/// `limit` is the number of hits to return. Zero selects the default, and values above the
/// API maximum are rejected with `TPT_ERR_INVALID_INPUT`.
///
/// # Safety
/// `engine` must be a live handle. `index` and `query` must be NUL-terminated UTF-8.
/// `out_json` must be writable; on success it receives a string to release with
/// [`tpt_string_free`].
#[no_mangle]
pub unsafe extern "C" fn tpt_search(
    engine: *const TptEngine,
    index: *const c_char,
    query: *const c_char,
    limit: usize,
    out_json: *mut *mut c_char,
) -> c_int {
    guard(|| {
        let engine = engine_ref(engine)?;
        let index = read_str(index, "index name")?;
        let text = read_str(query, "query")?;
        let limit = match limit {
            0 => DEFAULT_LIMIT,
            n if n <= TPT_MAX_LIMIT => n,
            _ => {
                return Err(Failure::input(format!(
                    "`limit` must be between 1 and {TPT_MAX_LIMIT}"
                )))
            }
        };
        let registry = engine.read();
        let collection = registry
            .get(index)
            .ok_or_else(|| Failure::not_found(format!("no index `{index}`")))?;
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
                let spans: &[Highlight] = result
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
        let body = json!({
            "total_hits": result.total_hits,
            "took_ms": result.took_ms,
            "hits": hits,
        });
        write_out(out_json, body.to_string())
    })
}

/// Releases a string returned through an out parameter. Passing null does nothing.
///
/// # Safety
/// `text` must be null or a string this library returned, and must be released only once.
#[no_mangle]
pub unsafe extern "C" fn tpt_string_free(text: *mut c_char) {
    if !text.is_null() {
        drop(CString::from_raw(text));
    }
}

/// The message for the most recent failed call on this thread. The pointer stays valid until
/// the next call on this thread. After a successful call it is the empty string.
#[no_mangle]
pub extern "C" fn tpt_last_error() -> *const c_char {
    LAST_ERROR.with(|slot| slot.borrow().as_ptr())
}

/// The library version as a NUL-terminated string. The pointer is static.
#[no_mangle]
pub extern "C" fn tpt_version() -> *const c_char {
    VERSION.as_ptr().cast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_nul_terminated() {
        let text = unsafe { CStr::from_ptr(tpt_version()) };
        assert_eq!(text.to_str().unwrap(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn panics_become_internal_errors() {
        let code = guard(|| panic!("boom"));
        assert_eq!(code, TPT_ERR_INTERNAL);
        let message = unsafe { CStr::from_ptr(tpt_last_error()) };
        assert_eq!(message.to_str().unwrap(), "internal panic");
    }
}
