//! A minimal `wasi:http` component (raw `wasi:http`, no wstd): every request is
//! answered, **by the guest**, with `200 hello from RUSM`. The host (hyper) only
//! moves bytes. A single blocking write — no async reactor, nothing to busy-poll.

use wasip2::exports::wasi::http::incoming_handler::Guest;
use wasip2::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

impl Guest for Component {
    fn handle(_request: IncomingRequest, response_out: ResponseOutparam) {
        let response = OutgoingResponse::new(Fields::new());
        let _ = response.set_status_code(200);
        let body = response.body().expect("a fresh response has a body");
        ResponseOutparam::set(response_out, Ok(response));
        if let Ok(stream) = body.write() {
            let _ = stream.blocking_write_and_flush(b"hello from RUSM\n");
        }
        let _ = OutgoingBody::finish(body, None);
    }
}

wasip2::http::proxy::export!(Component with_types_in wasip2);
