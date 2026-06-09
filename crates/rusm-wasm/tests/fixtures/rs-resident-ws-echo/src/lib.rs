//! A resident WebSocket echo handler: one long-lived instance serves every
//! connection and echoes each frame back to its sender (vs the broadcast room).
//! Used by `ws_bench` to measure the resident WS path against per-connection.

use rusm_rs::Pid;

struct Echo;

impl rusm_rs::ws::Handler for Echo {
    fn message(&mut self, conn: Pid, data: Vec<u8>) {
        rusm_rs::ws::send(conn, &data); // echo to the sender only
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
        rusm_rs::ws::serve(Echo);
    }
}

export!(Component);
