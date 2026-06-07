//! A `wasi:http` **SSE** component: streams `text/event-stream` events over time,
//! produced **by the guest**. The host (`wasmtime-wasi-http` + hyper) flushes each
//! chunk the instant the guest yields it — true server-sent events from a sandboxed
//! actor, no host-side templating. Each `wstd::task::sleep` parks the fiber (the
//! RUSM scheduler reuses the worker), so a long-lived SSE stream costs ~nothing idle.

use std::convert::Infallible;

use futures_lite::stream::unfold;
use wstd::http::body::{Body, Bytes};
use wstd::http::{Error, Request, Response};
use wstd::time::Duration;

#[wstd::http_server]
async fn main(_request: Request<Body>) -> Result<Response<Body>, Error> {
    // Emit five `data:` events, 50ms apart — the canonical SSE frame is `data: …\n\n`.
    let events = unfold(0u32, |n| async move {
        if n >= 5 {
            return None;
        }
        wstd::task::sleep(Duration::from_millis(50)).await;
        let frame = Bytes::from(format!("data: tick {n}\n\n"));
        Some((Ok::<_, Infallible>(frame), n + 1))
    });

    let response = Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_try_stream(events))?;
    Ok(response)
}
