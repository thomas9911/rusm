//! A `wasi:http` **SSE firehose**: an endless `text/event-stream` that yields one
//! `data:` event per poll, as fast as the client drains it (wasi:http body
//! back-pressure throttles the guest to the socket). When the client disconnects,
//! the host write fails, the stream cancels, and the instance is torn down — one
//! dropped stream never touches the others. The stress fixture for `sse_bench`.

use std::convert::Infallible;

use futures_lite::stream::unfold;
use wstd::http::body::{Body, Bytes};
use wstd::http::{Error, Request, Response};

#[wstd::http_server]
async fn main(_request: Request<Body>) -> Result<Response<Body>, Error> {
    let events = unfold(0u64, |n| async move {
        let frame = Bytes::from(format!("data: {n}\n\n"));
        Some((Ok::<_, Infallible>(frame), n.wrapping_add(1)))
    });
    let response = Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_try_stream(events))?;
    Ok(response)
}
