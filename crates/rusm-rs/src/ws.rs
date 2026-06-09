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
        let raw = crate::receive_bytes();
        let Ok(event) = serde_json::from_slice::<serde_json::Value>(&raw) else {
            continue;
        };
        let conn = event
            .get("conn")
            .and_then(|c| c.as_str())
            .and_then(|s| s.parse().ok())
            .map(Pid);
        let Some(conn) = conn else { continue };
        match event.get("op").and_then(|o| o.as_str()) {
            Some("open") => handler.open(conn),
            Some("message") => {
                let data = event
                    .get("data")
                    .and_then(|d| d.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|n| n.as_u64().map(|b| b as u8))
                            .collect()
                    })
                    .unwrap_or_default();
                handler.message(conn, data);
            }
            Some("close") => handler.close(conn),
            _ => {}
        }
    }
}
