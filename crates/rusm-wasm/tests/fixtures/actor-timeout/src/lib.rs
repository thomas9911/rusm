//! A guest fixture exercising `receive-timeout` (Erlang's `receive … after`).
//! On `run` it:
//!   1. receives `[reply-to pid: 8 LE]`,
//!   2. calls `receive-timeout(30)` with nothing pending → expects `none` (bit 0),
//!   3. signals `"armed"` to reply-to, then `receive-timeout(5000)` → expects the
//!      host's `"ping"` to arrive before the deadline (bit 1),
//!   4. replies `[own pid: 8 LE][flags: 1 byte]`.
//! The host asserts both bits — that the timeout elapses with an empty mailbox,
//! and that a message delivered before the deadline is returned, not dropped.

wit_bindgen::generate!({
    world: "process",
    path: "wit",
});

use rusm::runtime::actor;

struct Component;

impl Guest for Component {
    fn run() {
        let msg = actor::receive();
        let reply_to = u64::from_le_bytes(msg[0..8].try_into().unwrap());

        // (1) Nothing is pending, so a short timeout must elapse → none.
        let timed_out = actor::receive_timeout(30).is_none();

        // Tell the host we are about to wait; it then sends "ping".
        actor::send(reply_to, b"armed");

        // (2) A message that arrives before a long deadline must be returned.
        let got_ping = actor::receive_timeout(5_000).as_deref() == Some(b"ping".as_slice());

        let mut flags = 0u8;
        if timed_out {
            flags |= 1 << 0;
        }
        if got_ping {
            flags |= 1 << 1;
        }
        let mut out = actor::own_pid().to_le_bytes().to_vec();
        out.push(flags);
        actor::send(reply_to, &out);
    }
}

export!(Component);
