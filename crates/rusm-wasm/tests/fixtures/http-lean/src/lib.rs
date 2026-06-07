//! A **lean** `wasi:http` component: the raw bindings (wasip2), no wstd async
//! reactor. Every request is answered, by the guest, with `200 hello`. This is the
//! counterpart to `http-hello` (wstd) — it shows what the RUSM host can serve when
//! the guest carries no per-request runtime overhead.

use wasip2::http::types::{Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam};

struct Handler;

impl wasip2::exports::wasi::http::incoming_handler::Guest for Handler {
    fn handle(_request: IncomingRequest, response_out: ResponseOutparam) {
        // A fresh 200 response (the default status), take its body, send the headers,
        // write the body, finish — the canonical wasi:http response order.
        let response = OutgoingResponse::new(Fields::new());
        let body = response.body().expect("response body");
        ResponseOutparam::set(response_out, Ok(response));

        let stream = body.write().expect("body stream");
        stream
            .blocking_write_and_flush(b"hello from RUSM\n")
            .expect("write body");
        drop(stream); // release the borrow before finishing the body
        OutgoingBody::finish(body, None).expect("finish body");
    }
}

wasip2::http::proxy::export!(Handler with_types_in wasip2);
