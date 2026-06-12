//! A `wasi:http` **SSE** component (raw `wasi:http`, no wstd): streams
//! `text/event-stream` events over time, produced **by the guest**. The host
//! (`wasmtime-wasi-http` + hyper) flushes each chunk the instant the guest writes it.
//! The 50 ms gap is `monotonic-clock.subscribe-duration(...).block()` — a *blocking*
//! pollable that parks the fiber (the RUSM scheduler reuses the worker), so a
//! long-lived SSE stream costs ~nothing while idle. No reactor, no busy-poll.

use wasip2::clocks::monotonic_clock;
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

        // Five `data:` events, 50 ms apart — the canonical SSE frame is `data: …\n\n`.
        for n in 0..5u32 {
            // Park the fiber for 50 ms (block on a monotonic-clock pollable — not a spin).
            monotonic_clock::subscribe_duration(50_000_000).block();
            let frame = format!("data: tick {n}\n\n");
            // A failed write means the client hung up — stop streaming.
            if stream.blocking_write_and_flush(frame.as_bytes()).is_err() {
                break;
            }
        }
        drop(stream); // release the borrow before finishing the body
        let _ = OutgoingBody::finish(body, None);
    }
}

wasip2::http::proxy::export!(Component with_types_in wasip2);
