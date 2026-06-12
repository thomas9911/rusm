//! Ergonomics for a **per-connection** WebSocket handler: the host runs one sandboxed
//! component process *per connection*, so a handler is naturally isolated — its state
//! is that one connection's. The host delivers the connection's **writer pid** as the
//! first message (the process that owns the socket sink); reply by sending frames to it
//! (see [`Connection::send`]). Each later message is one inbound frame; the process is
//! killed when the socket closes (no `close` callback needed — exit cascades clean up).

use crate::Pid;

pub use crate::send_bytes as send;

/// One WebSocket connection — the per-connection process's view of its socket. Reply
/// to the client by writing frames; the host's writer process owns the actual sink.
pub struct Connection {
    writer: Pid,
}

impl Connection {
    /// The connection's writer pid (the reply target).
    pub fn writer(&self) -> Pid {
        self.writer
    }

    /// Send one frame back to the client. Dropped if the socket has closed.
    pub fn send(&self, frame: &[u8]) {
        crate::send_bytes(self.writer, frame);
    }
}

/// A per-connection WebSocket handler. There is one handler instance per connection,
/// so `&mut self` is *this connection's* state (no cross-connection sharing — keep
/// shared state in a `[components.<name>]` service or `kv`).
pub trait Handler {
    /// The connection opened. Default: do nothing.
    fn open(&mut self, conn: &Connection) {
        let _ = conn;
    }
    /// One inbound frame from the client.
    fn message(&mut self, conn: &Connection, data: Vec<u8>);
}

/// Run `handler` for this connection: learn the writer pid (the host's message 1),
/// fire [`Handler::open`], then dispatch each inbound frame to [`Handler::message`].
/// Never returns — the host kills the process when the socket closes. Call it from a
/// component's `run`.
pub fn serve<H: Handler>(mut handler: H) -> ! {
    // Message 1: the writer pid (decimal — the RUSM "tell me where to answer"
    // convention shared with the other per-connection paths).
    let writer = Pid(String::from_utf8(crate::receive_bytes())
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0));
    let conn = Connection { writer };
    handler.open(&conn);
    loop {
        let data = crate::receive_bytes();
        handler.message(&conn, data);
    }
}
