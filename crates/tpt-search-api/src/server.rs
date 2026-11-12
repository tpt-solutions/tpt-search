//! The accept loop: serves HTTP/1.1 connections until shutdown is requested.

use std::io;
use std::sync::Arc;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::http::handle;
use crate::state::AppState;

/// Serves connections on `listener` until `shutdown` becomes `true`. Connections already
/// open keep running until they finish. No new ones are accepted after shutdown.
pub async fn serve(
    listener: TcpListener,
    state: Arc<AppState>,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    let service = service_fn(move |req| handle(req, Arc::clone(&state)));
                    // A connection error affects only that connection.
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .with_upgrades()
                        .await;
                });
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return Ok(());
                }
            }
        }
    }
}
