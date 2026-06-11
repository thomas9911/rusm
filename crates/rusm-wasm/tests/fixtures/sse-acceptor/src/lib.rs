//! A resident **SSE acceptor** proving the offloaded live-fan-out pattern that
//! `rusm_rs::http::serve_sse`'s own docs prescribe for an endless feed: per
//! connection, reply a streaming `text/event-stream` head, then **spawn a pump**
//! (`sse-pump`) and hand it the responder pid — never pumping inline, so the
//! acceptor's loop is free to accept the next connection immediately (no
//! head-of-line blocking). The pump subscribes to the broker and writes the live
//! stream. Requires the `spawn` capability; the pump inherits it (non-escalating).

wit_bindgen::generate!({
    world: "process",
    path: "wit",
    with: { "rusm:runtime/actor@0.1.0": rusm_rs::rusm::runtime::actor },
});

struct Component;

impl Guest for Component {
    fn run() {
        // One line: the SDK replies the SSE head and offloads each connection to a
        // freshly-spawned `sse-pump`, never head-of-line blocking the acceptor.
        rusm_rs::http::serve_sse_offloaded("sse-pump");
    }
}

export!(Component);
