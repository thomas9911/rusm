//! Integration test: a real WebSocket client drives the live server.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use rusm_bench::{Node, RunnerConfig};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// Reads frames until one parses as the given message variant predicate holds.
async fn next_text<S>(ws: &mut S) -> String
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        match ws.next().await.expect("stream ended").expect("ws error") {
            Message::Text(t) => return t.to_string(),
            _ => continue,
        }
    }
}

#[tokio::test]
async fn client_receives_hello_then_drives_a_run() {
    // Fast tick so the test doesn't wait long.
    let node = Node::new(RunnerConfig {
        ticks_per_second: 60,
        ..RunnerConfig::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(rusm_bench::serve_on(listener, Arc::clone(&node)));

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .expect("connect");

    // First message is the scenario menu.
    let hello = next_text(&mut ws).await;
    assert!(hello.contains("\"type\":\"hello\""));
    assert!(hello.contains("connection-storm"));

    // Start a run.
    ws.send(Message::Text(
        "{\"type\":\"run\",\"scenario\":\"connection-storm\"}".into(),
    ))
    .await
    .unwrap();

    // Eventually a tick reports the run is live.
    let running = loop {
        let text = next_text(&mut ws).await;
        if text.contains("\"running\":true") {
            break text;
        }
    };
    assert!(running.contains("connection-storm"));

    // An invalid command yields an error message, not a disconnect.
    ws.send(Message::Text("{\"type\":\"bogus\"}".into()))
        .await
        .unwrap();
    let err = loop {
        let text = next_text(&mut ws).await;
        if text.contains("\"type\":\"error\"") {
            break text;
        }
    };
    assert!(err.contains("error"));

    // A ping is ignored (not a command) and doesn't disrupt the stream.
    ws.send(Message::Ping(Vec::new().into())).await.unwrap();

    // Stop returns to idle.
    ws.send(Message::Text("{\"type\":\"stop\"}".into()))
        .await
        .unwrap();
    let idle = loop {
        let text = next_text(&mut ws).await;
        if text.contains("\"running\":false") {
            break text;
        }
    };
    assert!(idle.contains("\"running\":false"));
}

#[tokio::test]
async fn server_handles_client_disconnect_cleanly() {
    let node = Node::new(RunnerConfig {
        ticks_per_second: 60,
        ..RunnerConfig::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(rusm_bench::serve_on(listener, Arc::clone(&node)));

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .expect("connect");
    let _ = next_text(&mut ws).await; // hello
    ws.close(None).await.unwrap();

    // A second client still connects and is served after the first leaves.
    let (mut ws2, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .expect("reconnect");
    assert!(next_text(&mut ws2).await.contains("\"type\":\"hello\""));
}

#[tokio::test]
async fn serve_binds_and_serves_on_an_address() {
    // Find a free port, then drive the public `serve` entry point (not `serve_on`).
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);
    let node = Node::new(RunnerConfig {
        ticks_per_second: 60,
        ..RunnerConfig::default()
    });
    let listen = addr.to_string();
    tokio::spawn(async move {
        let _ = rusm_bench::serve(&listen, node).await;
    });

    // Retry until the listener is accepting, then confirm we're served.
    let url = format!("ws://{addr}");
    let mut ws = loop {
        match tokio_tungstenite::connect_async(&url).await {
            Ok((ws, _)) => break ws,
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
        }
    };
    assert!(next_text(&mut ws).await.contains("\"type\":\"hello\""));
}

#[tokio::test]
async fn server_survives_non_websocket_connection() {
    use tokio::io::AsyncWriteExt;

    let node = Node::new(RunnerConfig {
        ticks_per_second: 60,
        ..RunnerConfig::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(rusm_bench::serve_on(listener, Arc::clone(&node)));

    // A raw, non-WebSocket payload: the handshake fails and the task bows out.
    let mut raw = tokio::net::TcpStream::connect(addr).await.unwrap();
    raw.write_all(b"GARBAGE / HTTP/1.1\r\n\r\n").await.unwrap();
    drop(raw);

    // The server is unharmed: a real client still gets served.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .expect("connect after garbage");
    assert!(next_text(&mut ws).await.contains("\"type\":\"hello\""));
}
