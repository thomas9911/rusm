//! A WebSocket-handler component: pure sandboxed actor logic, no IO. The host owns
//! the socket and delivers each inbound WS message to this process's mailbox; as
//! message 1 it learns the pid of the connection's *writer* process. It echoes each
//! message straight back through that pid — message-in, message-out.

wit_bindgen::generate!({
    world: "process",
    path: "wit",
    // Reuse rusm-rs's actor import bindings (imported once — the library/binary split).
    with: { "rusm:runtime/actor@0.1.0": rusm_rs::rusm::runtime::actor },
});

struct Component;

impl Guest for Component {
    fn run() {
        // Message 1: the writer pid to send replies to (decimal — the RUSM
        // "tell me where to answer" convention, as in the other fixtures).
        let writer: u64 = String::from_utf8(rusm_rs::receive_bytes())
            .unwrap()
            .parse()
            .unwrap();
        let writer = rusm_rs::Pid(writer);

        // Every later message is one inbound WS message; echo it back.
        loop {
            let message = rusm_rs::receive_bytes();
            rusm_rs::send_bytes(writer, &message);
        }
    }
}

export!(Component);
