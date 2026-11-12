//! The HTTP API end to end, over real TCP connections to a running server.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tpt_search_api::{serve, AppState};

struct Server {
    addr: SocketAddr,
    stop: watch::Sender<bool>,
}

async fn start() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, shutdown) = watch::channel(false);
    tokio::spawn(serve(listener, Arc::new(AppState::new()), shutdown));
    Server { addr, stop }
}

/// Sends one request on a fresh connection and returns the status and body.
async fn request(addr: SocketAddr, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let body = body.unwrap_or("");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(body.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .expect("response has a header block");
    let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)
    } else {
        body.to_string()
    };
    (status, body)
}

/// Decodes a chunked transfer-encoded body.
fn dechunk(mut rest: &str) -> String {
    let mut out = String::new();
    loop {
        let (size_line, after) = rest.split_once("\r\n").expect("chunk size line");
        let size = usize::from_str_radix(size_line.trim(), 16).expect("hex chunk size");
        if size == 0 {
            return out;
        }
        out.push_str(&after[..size]);
        rest = &after[size + 2..];
    }
}

fn schema() -> Value {
    json!({
        "name": "papers",
        "primary_key": "id",
        "fields": {
            "id": {"name": "id", "field_type": "Text", "indexed": false, "stored": true, "analyzer": null, "vector_config": null},
            "title": {"name": "title", "field_type": "Text", "indexed": true, "stored": true,
                      "analyzer": {"tokenizer": "Standard", "filters": ["Lowercase"]}, "vector_config": null},
            "year": {"name": "year", "field_type": "Integer", "indexed": true, "stored": true, "analyzer": null, "vector_config": null}
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn health_and_index_lifecycle() {
    let server = start().await;
    assert_eq!(request(server.addr, "GET", "/health", None).await.0, 200);

    let (status, _) = request(
        server.addr,
        "PUT",
        "/indexes/papers",
        Some(&schema().to_string()),
    )
    .await;
    assert_eq!(status, 201);
    let (status, _) = request(
        server.addr,
        "PUT",
        "/indexes/papers",
        Some(&schema().to_string()),
    )
    .await;
    assert_eq!(status, 409, "creating an existing index conflicts");

    let (status, body) = request(server.addr, "GET", "/indexes", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!(["papers"])
    );

    let (status, body) = request(server.addr, "GET", "/indexes/papers/schema", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["primary_key"],
        "id"
    );
    let _ = server.stop.send(true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn documents_round_trip_and_search_with_highlights() {
    let server = start().await;
    request(
        server.addr,
        "PUT",
        "/indexes/papers",
        Some(&schema().to_string()),
    )
    .await;

    let (status, _) = request(
        server.addr,
        "PUT",
        "/indexes/papers/documents/p1",
        Some(r#"{"title": "Rust search engines", "year": 2024}"#),
    )
    .await;
    assert_eq!(status, 200);
    request(
        server.addr,
        "PUT",
        "/indexes/papers/documents/p2",
        Some(r#"{"title": "Cooking pasta", "year": 2019}"#),
    )
    .await;

    let (status, body) = request(server.addr, "GET", "/indexes/papers/documents/p1", None).await;
    assert_eq!(status, 200);
    let doc: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(doc["fields"]["year"], 2024);

    let (status, body) = request(
        server.addr,
        "POST",
        "/indexes/papers/search",
        Some(r#"{"query": "MATCH(title, \"search\") AND year >= 2020", "limit": 5}"#),
    )
    .await;
    assert_eq!(status, 200);
    let result: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(result["total_hits"], 1);
    assert_eq!(result["hits"][0]["id"], "p1");
    assert_eq!(result["hits"][0]["highlights"][0]["field"], "title");

    let (status, body) = request(server.addr, "DELETE", "/indexes/papers/documents/p1", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["deleted"],
        true
    );
    assert_eq!(
        request(server.addr, "GET", "/indexes/papers/documents/p1", None)
            .await
            .0,
        404
    );
    let _ = server.stop.send(true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn errors_have_the_right_status_codes() {
    let server = start().await;
    request(
        server.addr,
        "PUT",
        "/indexes/papers",
        Some(&schema().to_string()),
    )
    .await;

    // Malformed JSON.
    assert_eq!(
        request(
            server.addr,
            "PUT",
            "/indexes/papers/documents/x",
            Some("{not json")
        )
        .await
        .0,
        400
    );
    // Unknown field and wrong type are rejected by the schema.
    assert_eq!(
        request(
            server.addr,
            "PUT",
            "/indexes/papers/documents/x",
            Some(r#"{"nope": 1}"#)
        )
        .await
        .0,
        422
    );
    assert_eq!(
        request(
            server.addr,
            "PUT",
            "/indexes/papers/documents/x",
            Some(r#"{"year": "2024"}"#)
        )
        .await
        .0,
        422
    );
    // Unknown index and unknown route.
    assert_eq!(
        request(server.addr, "GET", "/indexes/nope/documents/x", None)
            .await
            .0,
        404
    );
    assert_eq!(request(server.addr, "GET", "/nowhere", None).await.0, 404);
    // Bad query text and bad limit.
    assert_eq!(
        request(
            server.addr,
            "POST",
            "/indexes/papers/search",
            Some(r#"{"query": "MATCH(title"}"#)
        )
        .await
        .0,
        400
    );
    assert_eq!(
        request(
            server.addr,
            "POST",
            "/indexes/papers/search",
            Some(r#"{"query": "year >= 1", "limit": 0}"#)
        )
        .await
        .0,
        400
    );
    // Body over the limit.
    let big = format!(r#"{{"title": "{}"}}"#, "x".repeat(2 << 20));
    assert_eq!(
        request(
            server.addr,
            "PUT",
            "/indexes/papers/documents/big",
            Some(&big)
        )
        .await
        .0,
        413
    );
    let _ = server.stop.send(true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn websocket_subscribers_receive_change_events() {
    let server = start().await;
    request(
        server.addr,
        "PUT",
        "/indexes/papers",
        Some(&schema().to_string()),
    )
    .await;

    // Upgrade a connection to a WebSocket. The key is the RFC 6455 example.
    let mut ws = TcpStream::connect(server.addr).await.unwrap();
    ws.write_all(
        b"GET /indexes/papers/subscribe HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
    )
    .await
    .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0u8; 1];
        ws.read_exact(&mut byte).await.unwrap();
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    assert!(head.starts_with("HTTP/1.1 101"), "got: {head}");
    assert!(head.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));

    // A write from another connection produces an event on the socket.
    request(
        server.addr,
        "PUT",
        "/indexes/papers/documents/live",
        Some(r#"{"year": 2025}"#),
    )
    .await;

    let mut header = [0u8; 2];
    tokio::time::timeout(Duration::from_secs(5), ws.read_exact(&mut header))
        .await
        .expect("event arrives")
        .unwrap();
    assert_eq!(header[0], 0x81, "FIN and text opcode");
    let len = header[1] as usize;
    let mut payload = vec![0u8; len];
    ws.read_exact(&mut payload).await.unwrap();
    let event: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(event["op"], "upsert");
    assert_eq!(event["id"], "live");
    let _ = server.stop.send(true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_stops_accepting_connections() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (stop, shutdown) = watch::channel(false);
    let handle = tokio::spawn(serve(listener, Arc::new(AppState::new()), shutdown));
    stop.send(true).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("server stops")
        .unwrap();
    assert!(result.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streamed_search_returns_ndjson_hits_and_a_summary() {
    let server = start().await;
    request(
        server.addr,
        "PUT",
        "/indexes/papers",
        Some(&schema().to_string()),
    )
    .await;
    for i in 0..60 {
        let body = format!(r#"{{"title": "rust item {i}", "year": 2024}}"#);
        request(
            server.addr,
            "PUT",
            &format!("/indexes/papers/documents/s{i}"),
            Some(&body),
        )
        .await;
    }
    let (status, body) = request(
        server.addr,
        "POST",
        "/indexes/papers/search/stream",
        Some(r#"{"query": "MATCH(title, \"rust\")", "limit": 1000}"#),
    )
    .await;
    assert_eq!(status, 200);
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 61, "60 hits and one summary line");
    let summary: Value = serde_json::from_str(lines[60]).unwrap();
    assert_eq!(summary["total_hits"], 60);
    let first: Value = serde_json::from_str(lines[0]).unwrap();
    assert!(first["id"].is_string());
    let _ = server.stop.send(true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleting_an_index_removes_it() {
    let server = start().await;
    request(
        server.addr,
        "PUT",
        "/indexes/papers",
        Some(&schema().to_string()),
    )
    .await;
    let (status, _) = request(server.addr, "DELETE", "/indexes/papers", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        request(server.addr, "GET", "/indexes/papers/schema", None)
            .await
            .0,
        404
    );
    assert_eq!(
        request(server.addr, "DELETE", "/indexes/papers", None)
            .await
            .0,
        404
    );
    let _ = server.stop.send(true);
}
