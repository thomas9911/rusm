//! A `wasi:http` **SSE firehose** (raw `wasi:http`, no wstd): an endless
//! `text/event-stream` that writes one `data:` event as fast as the client drains it.
//! Each `blocking-write-and-flush` parks the fiber when the socket buffer is full —
//! so the loop is throttled to the socket (back-pressure), never a busy spin — and a
//! failed write (the client hung up) ends it, tearing the instance down. One dropped
//! stream never touches the others. The stress fixture for `sse_bench`.

use wasip2::exports::wasi::http::incoming_handler::Guest;
use wasip2::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

impl Guest for Component {
    fn handle(_request: IncomingRequest, response_out: ResponseOutparam) {
        let headers = Fields::new();
        let _ = headers.append(&"content-type".to_string(), b"text/event-stream");
        let response = OutgoingResponse::new(headers);
        let _ = response.set_status_code(200);
        let body = response.body().expect("a fresh response has a body");
        ResponseOutparam::set(response_out, Ok(response));
        let Ok(stream) = body.write() else { return };

        let mut n: u64 = 0;
        // Each write blocks (parks) until the socket can take more, so this is paced by
        // the client's drain rate, not a spin. A write error = client gone → stop.
        while stream
            .blocking_write_and_flush(format!("data: {n}\n\n").as_bytes())
            .is_ok()
        {
            n = n.wrapping_add(1);
        }
        drop(stream); // release the borrow before finishing the body
        let _ = OutgoingBody::finish(body, None);
    }
}

wasip2::http::proxy::export!(Component with_types_in wasip2);
