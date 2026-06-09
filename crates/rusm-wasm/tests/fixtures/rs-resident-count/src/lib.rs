//! A resident HTTP handler proving statefulness: one long-lived instance counts
//! requests, so the same component answers "hit #1", "hit #2", … across requests
//! (a per-request instance would always say "hit #1"). The counter is `&mut self`
//! state on the handler, which persists because the instance is resident.

struct Counter {
    hits: u64,
}

impl rusm_rs::http::Handler for Counter {
    fn handle(&mut self, _request: rusm_rs::http::Request) -> rusm_rs::http::Response {
        self.hits += 1;
        rusm_rs::http::Response::text(format!("hit #{}\n", self.hits))
    }
}

wit_bindgen::generate!({
    world: "process",
    path: "wit",
    with: { "rusm:runtime/actor@0.1.0": rusm_rs::rusm::runtime::actor },
});

struct Component;

impl Guest for Component {
    fn run() {
        rusm_rs::http::serve(Counter { hits: 0 }); // never returns
    }
}

export!(Component);
