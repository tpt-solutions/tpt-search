//! HTTP routes and handlers.
//!
//! | Method | Path | Purpose |
//! |---|---|---|
//! | GET | `/health` | liveness |
//! | GET | `/indexes` | list index names |
//! | PUT | `/indexes/{name}` | create an index from a schema (JSON `Schema`) |
//! | GET | `/indexes/{name}/schema` | the index's schema |
//! | DELETE | `/indexes/{name}` | delete an index and its graph |
//! | PUT | `/indexes/{name}/documents/{id}` | insert or replace a document |
//! | GET | `/indexes/{name}/documents/{id}` | fetch a document |
//! | DELETE | `/indexes/{name}/documents/{id}` | delete a document |
//! | POST | `/indexes/{name}/search` | run a query (`{"query": "...", "limit": n}`) |
//! | POST | `/indexes/{name}/search/stream` | the same hits as NDJSON, one line per hit |
//! | GET | `/indexes/{name}/subscribe` | WebSocket: change events for the index |
//!
//! Request bodies are limited to [`MAX_BODY`] bytes. Errors are JSON `{"error": "..."}`.
//! A query that runs longer than [`QUERY_TIMEOUT`] fails with 504.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::stream;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tpt_search_core::{DocumentId, QueryError, Schema, SearchError};
use tpt_search_query::{parse_query, QueryExecutor, QueryPlan, QueryPlanner};

use crate::convert::{document_from_json, document_to_json};
use crate::state::AppState;
use crate::ws;

/// Largest request body accepted, in bytes.
pub const MAX_BODY: usize = 1 << 20;
/// Hits returned when a search does not give a limit.
pub const DEFAULT_LIMIT: usize = 10;
/// Largest limit a search may ask for.
pub const MAX_LIMIT: usize = 1000;
/// Longest a query may run before the request fails with 504.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Bytes of an oversized body still read and discarded after the limit is passed, so the
/// client can read the 413 instead of meeting a connection reset.
const DRAIN_LIMIT: usize = 16 * MAX_BODY;

/// A response body. Every response uses this type, so streamed and buffered bodies share a
/// handler signature.
pub type Body = BoxBody<Bytes, Infallible>;

/// A failure the client sees as an HTTP error.
#[derive(Debug)]
pub enum ApiError {
    /// Malformed request.
    BadRequest(String),
    /// No such index, document, or route.
    NotFound(String),
    /// The resource already exists.
    Conflict(String),
    /// Well-formed but rejected by the schema or the index.
    Unprocessable(String),
    /// Body larger than [`MAX_BODY`].
    TooLarge,
    /// The query did not finish within [`QUERY_TIMEOUT`].
    Timeout,
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ApiError::Timeout => StatusCode::GATEWAY_TIMEOUT,
        }
    }

    fn message(&self) -> String {
        match self {
            ApiError::BadRequest(m)
            | ApiError::NotFound(m)
            | ApiError::Conflict(m)
            | ApiError::Unprocessable(m) => m.clone(),
            ApiError::TooLarge => format!("request body exceeds {MAX_BODY} bytes"),
            ApiError::Timeout => {
                format!("query did not finish within {}s", QUERY_TIMEOUT.as_secs())
            }
        }
    }
}

impl From<QueryError> for ApiError {
    fn from(e: QueryError) -> Self {
        ApiError::from(SearchError::from(e))
    }
}

impl From<SearchError> for ApiError {
    fn from(e: SearchError) -> Self {
        match e {
            SearchError::Query(QueryError::ParseError(m)) => ApiError::BadRequest(m),
            SearchError::Query(QueryError::Timeout) => ApiError::Timeout,
            SearchError::Query(QueryError::ResourceExhausted(m)) => ApiError::Unprocessable(m),
            other => ApiError::Unprocessable(other.to_string()),
        }
    }
}

fn buffered(status: StatusCode, content_type: &'static str, bytes: Bytes) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, content_type)
        .body(Full::new(bytes).boxed())
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::from_static(b"{}")).boxed()))
}

fn json_response(status: StatusCode, value: &Value) -> Response<Body> {
    buffered(status, "application/json", Bytes::from(value.to_string()))
}

fn error_response(e: ApiError) -> Response<Body> {
    json_response(e.status(), &json!({ "error": e.message() }))
}

/// Serves one request. Never fails at the HTTP level: every error becomes a response.
pub async fn handle(
    req: Request<Incoming>,
    state: Arc<AppState>,
) -> Result<Response<Body>, Infallible> {
    Ok(match route(req, &state).await {
        Ok(response) => response,
        Err(e) => error_response(e),
    })
}

async fn route(req: Request<Incoming>, state: &Arc<AppState>) -> Result<Response<Body>, ApiError> {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match (method.as_str(), parts.as_slice()) {
        ("GET", ["health"]) => Ok(json_response(StatusCode::OK, &json!({ "status": "ok" }))),
        ("GET", ["indexes"]) => Ok(list_indexes(state)),
        ("PUT", ["indexes", name]) => {
            let body = read_json(req).await?;
            create_index(state, name, body)
        }
        ("DELETE", ["indexes", name]) => delete_index(state, name),
        ("GET", ["indexes", name, "schema"]) => get_schema(state, name),
        ("PUT", ["indexes", name, "documents", id]) => {
            let body = read_json(req).await?;
            upsert(state, name, id, &body)
        }
        ("GET", ["indexes", name, "documents", id]) => get_document(state, name, id),
        ("DELETE", ["indexes", name, "documents", id]) => delete_document(state, name, id),
        ("POST", ["indexes", name, "search"]) => {
            let body = read_json(req).await?;
            search(state, name, &body)
        }
        ("POST", ["indexes", name, "search", "stream"]) => {
            let body = read_json(req).await?;
            search_stream(state, name, &body)
        }
        ("GET", ["indexes", name, "subscribe"]) => subscribe(state, name, req),
        _ => Err(ApiError::NotFound(format!("no route for {method} {path}"))),
    }
}

async fn read_json(req: Request<Incoming>) -> Result<Value, ApiError> {
    let mut body = req.into_body();
    let mut buf = Vec::new();
    let mut too_large = false;
    let mut drained = 0usize;
    while let Some(frame) = body.frame().await {
        let frame =
            frame.map_err(|e| ApiError::BadRequest(format!("reading the body failed: {e}")))?;
        let Some(data) = frame.data_ref() else {
            continue;
        };
        if too_large {
            drained += data.len();
            if drained > DRAIN_LIMIT {
                break;
            }
        } else if buf.len() + data.len() > MAX_BODY {
            too_large = true;
        } else {
            buf.extend_from_slice(data);
        }
    }
    if too_large {
        return Err(ApiError::TooLarge);
    }
    if buf.is_empty() {
        return Err(ApiError::BadRequest("expected a JSON body".into()));
    }
    serde_json::from_slice(&buf).map_err(|e| ApiError::BadRequest(format!("invalid JSON: {e}")))
}

fn list_indexes(state: &Arc<AppState>) -> Response<Body> {
    let registry = state.registry.read().unwrap_or_else(|e| e.into_inner());
    let names: Vec<&str> = registry.names();
    json_response(StatusCode::OK, &json!(names))
}

fn create_index(
    state: &Arc<AppState>,
    name: &str,
    body: Value,
) -> Result<Response<Body>, ApiError> {
    let schema: Schema = serde_json::from_value(body)
        .map_err(|e| ApiError::BadRequest(format!("invalid schema: {e}")))?;
    let mut registry = state.registry.write().unwrap_or_else(|e| e.into_inner());
    if registry.get(name).is_some() {
        return Err(ApiError::Conflict(format!("index `{name}` already exists")));
    }
    registry
        .create(name, schema)
        .map_err(|e| ApiError::Unprocessable(e.to_string()))?;
    Ok(json_response(StatusCode::CREATED, &json!({ "name": name })))
}

fn delete_index(state: &Arc<AppState>, name: &str) -> Result<Response<Body>, ApiError> {
    let mut registry = state.registry.write().unwrap_or_else(|e| e.into_inner());
    if !registry.remove(name) {
        return Err(ApiError::NotFound(format!("no index `{name}`")));
    }
    Ok(json_response(StatusCode::OK, &json!({ "deleted": name })))
}

fn get_schema(state: &Arc<AppState>, name: &str) -> Result<Response<Body>, ApiError> {
    let registry = state.registry.read().unwrap_or_else(|e| e.into_inner());
    let collection = registry
        .get(name)
        .ok_or_else(|| ApiError::NotFound(format!("no index `{name}`")))?;
    let schema = serde_json::to_value(collection.schema())
        .map_err(|e| ApiError::Unprocessable(e.to_string()))?;
    Ok(json_response(StatusCode::OK, &schema))
}

fn upsert(
    state: &Arc<AppState>,
    name: &str,
    id: &str,
    body: &Value,
) -> Result<Response<Body>, ApiError> {
    // Convert under the read lock, then take the write lock for the update itself.
    let doc = {
        let registry = state.registry.read().unwrap_or_else(|e| e.into_inner());
        let collection = registry
            .get(name)
            .ok_or_else(|| ApiError::NotFound(format!("no index `{name}`")))?;
        document_from_json(collection.schema(), id, body).map_err(ApiError::Unprocessable)?
    };
    {
        let mut registry = state.registry.write().unwrap_or_else(|e| e.into_inner());
        let collection = registry
            .get_mut(name)
            .ok_or_else(|| ApiError::NotFound(format!("no index `{name}`")))?;
        collection
            .upsert(&doc)
            .map_err(|e| ApiError::Unprocessable(e.to_string()))?;
    }
    state.publish(name, &json!({ "op": "upsert", "index": name, "id": id }));
    Ok(json_response(StatusCode::OK, &json!({ "id": id })))
}

fn get_document(state: &Arc<AppState>, name: &str, id: &str) -> Result<Response<Body>, ApiError> {
    let registry = state.registry.read().unwrap_or_else(|e| e.into_inner());
    let collection = registry
        .get(name)
        .ok_or_else(|| ApiError::NotFound(format!("no index `{name}`")))?;
    let doc = collection
        .get(&DocumentId::from(id))
        .ok_or_else(|| ApiError::NotFound(format!("no document `{id}`")))?;
    Ok(json_response(StatusCode::OK, &document_to_json(&doc)))
}

fn delete_document(
    state: &Arc<AppState>,
    name: &str,
    id: &str,
) -> Result<Response<Body>, ApiError> {
    let existed = {
        let mut registry = state.registry.write().unwrap_or_else(|e| e.into_inner());
        let collection = registry
            .get_mut(name)
            .ok_or_else(|| ApiError::NotFound(format!("no index `{name}`")))?;
        collection.delete(&DocumentId::from(id))
    };
    state.publish(
        name,
        &json!({ "op": "delete", "index": name, "id": id, "existed": existed }),
    );
    Ok(json_response(
        StatusCode::OK,
        &json!({ "deleted": existed }),
    ))
}

/// The query text and limit of a search request.
fn search_request(body: &Value) -> Result<(&str, usize), ApiError> {
    let text = body
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::BadRequest("expected a `query` string".into()))?;
    let limit = match body.get("limit") {
        None => DEFAULT_LIMIT,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=MAX_LIMIT as u64).contains(n))
            .ok_or_else(|| {
                ApiError::BadRequest(format!("`limit` must be between 1 and {MAX_LIMIT}"))
            })? as usize,
    };
    Ok((text, limit))
}

/// Parses, plans, optimizes, and runs a search under the registry's read lock.
fn run_search(
    state: &Arc<AppState>,
    name: &str,
    text: &str,
    limit: usize,
) -> Result<(QueryPlan, tpt_search_query::QueryResult), ApiError> {
    let registry = state.registry.read().unwrap_or_else(|e| e.into_inner());
    let collection = registry
        .get(name)
        .ok_or_else(|| ApiError::NotFound(format!("no index `{name}`")))?;
    let query = parse_query(text)?;
    let plan = QueryPlanner::new().plan(name, collection, &query)?;
    let plan = QueryPlanner::new().optimize(plan.with_limit(limit), collection);
    let result = QueryExecutor::new(&registry)
        .with_timeout(QUERY_TIMEOUT)
        .execute(&plan)?;
    Ok((plan, result))
}

/// JSON for one hit: its id, its fields, and the highlight spans of the match.
fn hit_json(doc: &tpt_search_core::Document, highlights: &[tpt_search_index::Highlight]) -> Value {
    let spans: Vec<Value> = highlights
        .iter()
        .map(|h| json!({ "field": h.field, "spans": h.positions }))
        .collect();
    json!({
        "id": doc.id.as_str(),
        "document": document_to_json(doc)["fields"].clone(),
        "highlights": spans,
    })
}

fn search(state: &Arc<AppState>, name: &str, body: &Value) -> Result<Response<Body>, ApiError> {
    let (text, limit) = search_request(body)?;
    let (_, result) = run_search(state, name, text, limit)?;
    let hits: Vec<Value> = result
        .documents
        .iter()
        .map(|doc| {
            let spans = result
                .highlights
                .get(&doc.id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            hit_json(doc, spans)
        })
        .collect();
    Ok(json_response(
        StatusCode::OK,
        &json!({
            "total_hits": result.total_hits,
            "took_ms": result.took_ms,
            "hits": hits,
        }),
    ))
}

/// Streams the hits as NDJSON: one line per hit, then a final summary line. The body is
/// sent in chunks of `HITS_PER_CHUNK` lines.
fn search_stream(
    state: &Arc<AppState>,
    name: &str,
    body: &Value,
) -> Result<Response<Body>, ApiError> {
    const HITS_PER_CHUNK: usize = 50;
    let (text, limit) = search_request(body)?;
    let (_, result) = run_search(state, name, text, limit)?;

    let mut lines: Vec<String> = result
        .documents
        .iter()
        .map(|doc| {
            let spans = result
                .highlights
                .get(&doc.id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            hit_json(doc, spans).to_string()
        })
        .collect();
    lines.push(json!({ "total_hits": result.total_hits, "took_ms": result.took_ms }).to_string());

    let chunks: Vec<Result<Frame<Bytes>, Infallible>> = lines
        .chunks(HITS_PER_CHUNK)
        .map(|chunk| {
            let mut text = chunk.join("\n");
            text.push('\n');
            Ok(Frame::data(Bytes::from(text)))
        })
        .collect();
    let body = StreamBody::new(stream::iter(chunks)).boxed();
    Response::builder()
        .status(StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, "application/x-ndjson")
        .body(body)
        .map_err(|e| ApiError::Unprocessable(e.to_string()))
}

fn subscribe(
    state: &Arc<AppState>,
    name: &str,
    req: Request<Incoming>,
) -> Result<Response<Body>, ApiError> {
    {
        let registry = state.registry.read().unwrap_or_else(|e| e.into_inner());
        if registry.get(name).is_none() {
            return Err(ApiError::NotFound(format!("no index `{name}`")));
        }
    }
    if !ws::is_upgrade(&req) {
        return Err(ApiError::BadRequest("expected a WebSocket upgrade".into()));
    }
    let response = ws::handshake_response(&req)
        .ok_or_else(|| ApiError::BadRequest("missing Sec-WebSocket-Key".into()))?;
    // Subscribe before answering, so no event between the two is lost.
    let events = state.subscribe(name);
    tokio::spawn(async move {
        if let Ok(upgraded) = hyper::upgrade::on(req).await {
            ws::pump(TokioIo::new(upgraded), events).await;
        }
    });
    Ok(response.map(|body| body.boxed()))
}
