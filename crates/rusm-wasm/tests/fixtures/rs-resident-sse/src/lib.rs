//! A resident SSE handler: each request streams five `text/event-stream` events
//! from one long-lived instance via `serve_sse`. Proves the resident path produces
//! a streamed (chunked) body, not a buffered one.

wit_bindgen::generate!({
    world: "process",
    path: "wit",
    with: { "rusm:runtime/actor@0.1.0": rusm_rs::rusm::runtime::actor },
});

struct Component;

impl Guest for Component {
    fn run() {
        rusm_rs::http::serve_sse(|_request| {
            (0..5).map(|n| format!("data: tick {n}\n\n").into_bytes())
        });
    }
}

export!(Component);
