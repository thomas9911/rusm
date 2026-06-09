//! Ergonomics for a **resident** WebSocket handler: one long-lived process serves
//! *all* connections and holds shared state (a chat room, a pub/sub hub, presence).
//! Each connection is identified by its **writer pid** (`conn`) — the process that
//! owns that socket's sink; reply to a connection by sending bytes to its `conn`
//! (see [`send`]). The host gateway turns socket lifecycle + frames into `open` /
//! `message` / `close` events on the actor wire.

use crate::Pid;

pub use crate::send_bytes as send;

/// A resident WebSocket handler. `&mut self` is shared state across every
/// connection; reply to a connection with [`send`]`(conn, &frame)`.
pub trait Handler {
    /// A new connection opened (its `conn` is the writer pid to reply to).
    fn open(&mut self, conn: Pid) {
        let _ = conn;
    }
    /// One inbound frame from `conn`.
    fn message(&mut self, conn: Pid, data: Vec<u8>);
    /// `conn` closed.
    fn close(&mut self, conn: Pid) {
        let _ = conn;
    }
}

/// Runs `handler` as the resident WebSocket serving loop: dispatch each
/// `open`/`message`/`close` event from the host gateway. Never returns — call it
/// from a component's `run`.
pub fn serve<H: Handler>(mut handler: H) -> ! {
    loop {
        // Binary event from the host gateway: [op: u8][conn: u64 LE][data…] — no
        // per-frame JSON parse / number-array rebuild.
        let raw = crate::receive_bytes();
        if raw.len() < 9 {
            continue;
        }
        let conn = Pid(u64::from_le_bytes(raw[1..9].try_into().expect("8-byte conn")));
        match raw[0] {
            0 => handler.open(conn),
            1 => handler.message(conn, raw[9..].to_vec()),
            2 => handler.close(conn),
            _ => {}
        }
    }
}
