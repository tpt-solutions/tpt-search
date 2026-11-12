//! Runs the HTTP API until Ctrl-C.
//!
//! Run with: `cargo run -p tpt-search-api --example serve -- 127.0.0.1:8080`

use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::watch;
use tpt_search_api::{serve, AppState};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8080".to_string());
    let listener = TcpListener::bind(&addr).await?;
    eprintln!("tpt-search API listening on {addr} (Ctrl-C to stop)");

    let (stop, shutdown) = watch::channel(false);
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = stop.send(true);
    });
    serve(listener, Arc::new(AppState::new()), shutdown).await
}
