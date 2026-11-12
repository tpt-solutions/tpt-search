//! HTTP and WebSocket API for `tpt-search`, built on `hyper` directly.
//!
//! The web frameworks (`axum`, `tonic`) and GraphQL (`async-graphql`) were checked
//! against `deny.toml` first and fail it (Apache-2.0-only and MPL-2.0 crates), so the API
//! speaks HTTP/1.1 and WebSocket itself. gRPC is not offered for the same reason.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use tokio::net::TcpListener;
//! use tokio::sync::watch;
//! use tpt_search_api::{serve, AppState};
//!
//! # async fn run() -> std::io::Result<()> {
//! let state = Arc::new(AppState::new());
//! let listener = TcpListener::bind("127.0.0.1:8080").await?;
//! let (_stop, shutdown) = watch::channel(false);
//! serve(listener, state, shutdown).await
//! # }
//! ```

#![doc(html_root_url = "https://docs.rs/tpt-search-api")]

pub mod convert;
pub mod http;
pub mod server;
pub mod state;
pub mod ws;

pub use http::{ApiError, DEFAULT_LIMIT, MAX_BODY, MAX_LIMIT};
pub use server::serve;
pub use state::AppState;
