//! **rusm-rs** — the ergonomic Rust *guest* crate for RUSM: write a component (or
//! a service) in Rust over the `rusm:runtime` actor world, the Rust twin of
//! rusm-ts. It wraps the raw `wit-bindgen` actor bindings into a small, idiomatic
//! API — `Pid`, `send`/`receive` (serde-typed), `spawn`, the registry, `Stream` —
//! and re-exports the `Guest` trait + `export!` macro so a guest crate depends on
//! this and `rusm_rs::export!`s its component (the wit-bindgen library/binary
//! split, via `default_bindings_module`).
//!
//! Blocking "just works": `receive`/`Stream::read` suspend the instance's fiber
//! (freeing the scheduler thread) until data arrives — like a Rust host process,
//! and like an Erlang `receive`.

// This crate owns the actor **import** bindings; a guest maps to them with
// `with: { "rusm:runtime/actor@0.1.0": rusm_rs::rusm::runtime::actor }` and
// `export!`s its own `run` — the wit-bindgen library/binary split, so the actor
// interface is imported exactly once in the final component. (See the `rs-guest`
// fixture / the docs for the guest-side boilerplate.)
wit_bindgen::generate!({
    world: "imports",
    path: "wit",
});

use rusm::runtime::actor;
use serde::de::DeserializeOwned;
use serde::Serialize;

pub use rusm_rs_macros::service;
pub use serde;
pub use serde_json;

pub mod wire;

/// A process identifier (Erlang's pid).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pid(pub u64);

impl std::fmt::Display for Pid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// This process's own pid (`self()`).
pub fn me() -> Pid {
    Pid(actor::own_pid())
}

/// Every live pid (subject to capability).
pub fn list() -> Vec<Pid> {
    actor::list_processes().into_iter().map(Pid).collect()
}

/// Spawn a registered component by name → its pid (capability-gated `spawn`).
pub fn spawn(component: &str) -> Result<Pid, String> {
    actor::spawn(component).map(Pid)
}

/// Register this process under a name in the node registry.
pub fn register(name: &str) -> bool {
    actor::register(name)
}

/// Look up a registered name, or `None` if unregistered.
pub fn whereis(name: &str) -> Option<Pid> {
    actor::whereis(name).map(Pid)
}

/// Release a registered name.
pub fn unregister(name: &str) -> bool {
    actor::unregister(name)
}

/// Set this process's human-readable label (shown in introspection).
pub fn set_label(label: &str) {
    actor::set_label(label);
}

/// Whether a pid is still alive (subject to capability).
pub fn is_alive(pid: Pid) -> bool {
    actor::is_alive(pid.0)
}

/// Kill a pid (subject to capability).
pub fn kill(pid: Pid) -> bool {
    actor::kill(pid.0)
}

/// Send raw bytes to a pid (dropped if it's gone).
pub fn send_bytes(to: Pid, msg: &[u8]) {
    actor::send(to.0, msg);
}

thread_local! {
    /// Messages the RPC client set aside while awaiting a reply, so the app's own
    /// `receive` still sees them (the guest is single-threaded — one mailbox).
    static INBOX: std::cell::RefCell<std::collections::VecDeque<Vec<u8>>> =
        std::cell::RefCell::new(std::collections::VecDeque::new());
}

/// Set a message aside for the app's own `receive` (used by the RPC client).
pub(crate) fn stash(raw: Vec<u8>) {
    INBOX.with(|q| q.borrow_mut().push_back(raw));
}

/// Block until the next message arrives; returns its raw bytes. Drains any mail
/// the RPC client set aside first (FIFO preserved).
pub fn receive_bytes() -> Vec<u8> {
    if let Some(raw) = INBOX.with(|q| q.borrow_mut().pop_front()) {
        return raw;
    }
    actor::receive()
}

/// Send a serializable value as a JSON message — the wire shared with TS guests.
pub fn send<T: Serialize>(to: Pid, msg: &T) -> serde_json::Result<()> {
    actor::send(to.0, &serde_json::to_vec(msg)?);
    Ok(())
}

/// Block for the next message and deserialize it from JSON.
pub fn receive<T: DeserializeOwned>() -> serde_json::Result<T> {
    serde_json::from_slice(&actor::receive())
}

/// A back-pressured byte stream to or from another process — the same primitive
/// as the host's, surfaced ergonomically. `read` suspends the fiber until a chunk
/// arrives; `None` is end-of-stream.
pub struct Stream {
    handle: u64,
}

impl Stream {
    /// Open a stream to a pid; `None` if the target is gone.
    pub fn open(to: Pid) -> Option<Stream> {
        actor::stream_open(to.0).map(|handle| Stream { handle })
    }

    /// Block until an incoming stream arrives, and take it for reading.
    pub fn accept() -> Stream {
        Stream {
            handle: actor::stream_accept(),
        }
    }

    /// Write one chunk; `false` once the reader is gone.
    pub fn write(&self, chunk: &[u8]) -> bool {
        actor::stream_write(self.handle, chunk)
    }

    /// Read the next chunk, or `None` at end-of-stream.
    pub fn read(&self) -> Option<Vec<u8>> {
        actor::stream_read(self.handle)
    }

    /// Close the write end (signals end-of-stream to the reader).
    pub fn close(self) {
        actor::stream_close(self.handle);
    }
}
