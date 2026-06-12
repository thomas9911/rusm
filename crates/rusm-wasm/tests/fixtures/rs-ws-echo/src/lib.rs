//! A per-connection WebSocket handler (rusm-rs): the host runs one instance per
//! connection and delivers each inbound frame to its mailbox; it echoes each frame
//! straight back via `conn.send`. Pure sandboxed actor logic — IO stays host-side.
//! Exercises `rusm_rs::ws::serve` (the ergonomic per-connection API).
use rusm_rs::ws::{self, Connection, Handler};

struct Echo;

impl Handler for Echo {
    fn message(&mut self, conn: &Connection, data: Vec<u8>) {
        conn.send(&data); // echo the frame back to the sender
    }
}

#[rusm_rs::main]
fn run() {
    ws::serve(Echo);
}
