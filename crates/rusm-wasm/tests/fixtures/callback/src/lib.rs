//! Component-to-component **request/reply** ("host-mediated callback") over the
//! actor ABI — no new runtime API, just `register`/`whereis`/`send`/`receive`.
//!
//! One component, two roles decided by a registration race (the host controls
//! ordering): the first instance wins `register("responder")` and serves; a later
//! instance finds it via `whereis` and calls it, then forwards the reply to the
//! test's `collector`.

wit_bindgen::generate!({
    world: "process",
    path: "wit",
});

use rusm::runtime::actor;

struct Component;

impl Guest for Component {
    fn run() {
        let me = actor::own_pid();
        if actor::register("responder") {
            // Responder: reply to each request with the payload byte doubled.
            // Message layout: [reply-to pid: 8 LE][payload byte].
            loop {
                let msg = actor::receive();
                let reply_to = u64::from_le_bytes(msg[0..8].try_into().unwrap());
                actor::send(reply_to, &[msg[8].wrapping_mul(2)]);
            }
        } else {
            // Caller: call the responder, then forward the reply to the collector.
            let responder = actor::whereis("responder").expect("responder registered");
            let mut request = me.to_le_bytes().to_vec();
            request.push(21);
            actor::send(responder, &request);
            let reply = actor::receive(); // [42]
            let collector = actor::whereis("collector").expect("collector registered");
            actor::send(collector, &reply);
        }
    }
}

export!(Component);
