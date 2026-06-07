//! A guest fixture exercising the **whole** `rusm:runtime` actor ABI from inside a
//! component. On `run` it:
//!   1. receives `[reply-to pid: 8 LE][target-to-kill pid: 8 LE]`,
//!   2. calls every actor op (own-pid, set-label, register, whereis, info,
//!      list-processes, is-alive, kill, unregister),
//!   3. replies `[own pid: 8 LE][flags: 1 byte]`, one bit per op that succeeded.
//! The host asserts the flags + observable effects (target killed, name released).

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
        let target = u64::from_le_bytes(msg[8..16].try_into().unwrap());

        let me = actor::own_pid();
        actor::set_label("echo-worker");
        let checks = [
            actor::register("echo"),
            actor::whereis("echo") == Some(me),
            actor::info(me).is_some_and(|i| i.pid == me && i.label.as_deref() == Some("echo-worker")),
            actor::list_processes().contains(&me),
            actor::is_alive(target),
            actor::kill(target),
            actor::unregister("echo"),
        ];

        let mut flags = 0u8;
        for (bit, ok) in checks.iter().enumerate() {
            if *ok {
                flags |= 1 << bit;
            }
        }
        let mut out = me.to_le_bytes().to_vec();
        out.push(flags);
        actor::send(reply_to, &out);
    }
}

export!(Component);
