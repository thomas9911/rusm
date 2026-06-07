//! Serving **WebSockets** (Phase 11). A WebSocket is only HTTP for its handshake;
//! after the `Upgrade` it's a raw bidirectional stream — and the handshake + the
//! protocol live entirely on the host, which RUSM controls. So WS never goes
//! through `wasi:http`: **hyper** surfaces the upgrade, **`tokio-tungstenite`** runs
//! the WS protocol (framing, ping/pong, close), and each connection is its own
//! supervised task — a failure drops only that socket, never the listener.
//!
//! This slice serves a host-side **echo**, proving the upgrade + framing +
//! concurrency on RUSM. Bridging a connection to a WASM component process (each WS
//! message ↔ the process mailbox) is the next slice.

use std::convert::Infallible;

use futures_util::{SinkExt, StreamExt};
use http_body_util::Empty;
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::WebSocketStream;

/// Serve a WebSocket **echo** on `listener` until it closes — one supervised task
/// per connection. Abort the task driving this to stop.
pub async fn serve_ws_echo(listener: TcpListener) {
    loop {
        let Ok((stream, _peer)) = listener.accept().await else {
            break;
        };
        stream.set_nodelay(true).ok();
        tokio::spawn(async move {
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), hyper::service::service_fn(upgrade))
                // `with_upgrades` is what lets `hyper::upgrade::on` hand us the
                // raw stream after the 101.
                .with_upgrades()
                .await;
        });
    }
}

/// Answer the HTTP `Upgrade` with a 101 and spawn the WebSocket task. A request
/// without a WebSocket key gets a plain 426.
async fn upgrade(
    req: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<Empty<Bytes>>, Infallible> {
    let Some(key) = req
        .headers()
        .get("sec-websocket-key")
        .and_then(|k| k.to_str().ok())
        .map(|s| s.to_owned())
    else {
        return Ok(hyper::Response::builder()
            .status(426)
            .body(Empty::new())
            .unwrap());
    };
    let accept = derive_accept_key(key.as_bytes());

    // After the 101 is sent, take the upgraded stream and run the WS protocol.
    tokio::spawn(async move {
        let Ok(upgraded) = hyper::upgrade::on(req).await else {
            return;
        };
        let mut ws =
            WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, None).await;
        while let Some(Ok(message)) = ws.next().await {
            if message.is_close() {
                break;
            }
            if (message.is_text() || message.is_binary()) && ws.send(message).await.is_err() {
                break;
            }
        }
    });

    Ok(hyper::Response::builder()
        .status(101)
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-accept", accept)
        .body(Empty::new())
        .unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::Message;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn echoes_a_websocket_message() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_ws_echo(listener));

        let (mut ws, _resp) = tokio_tungstenite::connect_async(format!("ws://{addr}/"))
            .await
            .unwrap();
        ws.send(Message::text("hello ws")).await.unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        assert_eq!(&reply.into_data()[..], b"hello ws");

        server.abort();
    }
}
