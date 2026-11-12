//! Load check for the HTTP API: loads documents, then runs searches from concurrent
//! clients and reports throughput and latency percentiles.
//!
//! Run with: `cargo run --release -p tpt-search-api --example load -- 20000 8 2000`
//! Arguments: documents to load, concurrent clients, searches per client.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tpt_search_api::{serve, AppState};

async fn request(addr: std::net::SocketAddr, method: &str, path: &str, body: &str) -> u16 {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(body.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw);
    text.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let docs: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(20_000);
    let clients: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(8);
    let per_client: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(2_000);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, shutdown) = watch::channel(false);
    tokio::spawn(serve(listener, Arc::new(AppState::new()), shutdown));

    let schema = r#"{"name":"papers","primary_key":"id","fields":{"id":{"name":"id","field_type":"Text","indexed":false,"stored":true,"analyzer":null,"vector_config":null},"title":{"name":"title","field_type":"Text","indexed":true,"stored":true,"analyzer":{"tokenizer":"Standard","filters":["Lowercase"]},"vector_config":null},"year":{"name":"year","field_type":"Integer","indexed":true,"stored":true,"analyzer":null,"vector_config":null}}}"#;
    assert_eq!(request(addr, "PUT", "/indexes/papers", schema).await, 201);

    let started = Instant::now();
    for i in 0..docs {
        let title = if i % 3 == 0 {
            "rust search engine"
        } else {
            "cooking with pasta"
        };
        let body = format!(r#"{{"title":"{title} {i}","year":{}}}"#, 2000 + i % 25);
        assert_eq!(
            request(
                addr,
                "PUT",
                &format!("/indexes/papers/documents/d{i}"),
                &body
            )
            .await,
            200
        );
    }
    let load = started.elapsed();
    println!(
        "loaded {docs} documents in {:.2}s ({:.0} writes/s)",
        load.as_secs_f64(),
        docs as f64 / load.as_secs_f64()
    );

    let started = Instant::now();
    let mut tasks = Vec::new();
    for _ in 0..clients {
        tasks.push(tokio::spawn(async move {
            let mut latencies: Vec<Duration> = Vec::with_capacity(per_client);
            for _ in 0..per_client {
                let t = Instant::now();
                let status = request(
                    addr,
                    "POST",
                    "/indexes/papers/search",
                    r#"{"query":"MATCH(title, \"rust\") AND year >= 2010","limit":10}"#,
                )
                .await;
                assert_eq!(status, 200);
                latencies.push(t.elapsed());
            }
            latencies
        }));
    }
    let mut all: Vec<Duration> = Vec::new();
    for t in tasks {
        all.extend(t.await.unwrap());
    }
    let elapsed = started.elapsed();
    all.sort();
    let pct = |p: f64| all[((all.len() as f64 * p) as usize).min(all.len() - 1)];
    println!(
        "{} searches from {clients} clients in {:.2}s ({:.0} req/s)",
        all.len(),
        elapsed.as_secs_f64(),
        all.len() as f64 / elapsed.as_secs_f64()
    );
    println!(
        "latency p50 {:.2} ms  p95 {:.2} ms  p99 {:.2} ms",
        pct(0.50).as_secs_f64() * 1e3,
        pct(0.95).as_secs_f64() * 1e3,
        pct(0.99).as_secs_f64() * 1e3
    );
    let _ = stop.send(true);
}
