//! WebSocket support: the RFC 6455 handshake and server-to-client text frames.
//!
//! The server only sends events. Client frames are not parsed beyond what is needed to
//! detect that the client has gone away, which happens when the socket write fails.

use base64::Engine;
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::broadcast;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// The `Sec-WebSocket-Accept` value for a client key.
pub fn accept_key(client_key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(client_key.as_bytes());
    hasher.update(GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

/// Whether a request asks to upgrade to a WebSocket.
pub fn is_upgrade<B>(req: &Request<B>) -> bool {
    req.headers()
        .get(hyper::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
        && req.headers().contains_key(hyper::header::SEC_WEBSOCKET_KEY)
}

/// The 101 response that accepts an upgrade request. `None` if the key is missing.
pub fn handshake_response<B>(req: &Request<B>) -> Option<Response<Full<Bytes>>> {
    let key = req
        .headers()
        .get(hyper::header::SEC_WEBSOCKET_KEY)?
        .to_str()
        .ok()?;
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(hyper::header::UPGRADE, "websocket")
        .header(hyper::header::CONNECTION, "Upgrade")
        .header("Sec-WebSocket-Accept", accept_key(key))
        .body(Full::new(Bytes::new()))
        .ok()
}

/// Encodes one unmasked text frame. Server frames are never masked.
pub fn text_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x81); // FIN, text
    match payload.len() {
        n if n < 126 => frame.push(n as u8),
        n if n <= u16::MAX as usize => {
            frame.push(126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            frame.push(127);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(payload);
    frame
}

/// Sends every event from `events` as a text frame until the client goes away or the
/// index's feed closes. A subscriber that lags skips the events it missed.
pub async fn pump<W: AsyncWrite + Unpin>(mut socket: W, mut events: broadcast::Receiver<String>) {
    loop {
        match events.recv().await {
            Ok(event) => {
                if socket
                    .write_all(&text_frame(event.as_bytes()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    let _ = socket.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_key_matches_the_rfc_example() {
        // Example from RFC 6455, section 1.3.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn frames_use_the_right_length_encoding() {
        assert_eq!(text_frame(b"hi"), [0x81, 2, b'h', b'i']);
        let medium = text_frame(&vec![b'x'; 300]);
        assert_eq!(&medium[..4], &[0x81, 126, 1, 44]);
        let large = text_frame(&vec![b'x'; 70_000]);
        assert_eq!(large[1], 127);
        assert_eq!(large.len(), 70_000 + 10);
    }
}
