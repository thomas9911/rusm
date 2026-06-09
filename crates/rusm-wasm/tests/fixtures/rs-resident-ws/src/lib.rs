//! A resident WebSocket "chat room": one long-lived instance serves every
//! connection and holds the shared member list, so a frame from one connection
//! broadcasts to all — proving a singleton resident multiplexes connections (a
//! per-connection handler could never see the others). Each new connection gets a
//! "welcome" ack, so a test can confirm it's registered before broadcasting.

use rusm_rs::Pid;

#[derive(Default)]
struct Room {
    members: Vec<Pid>,
}

impl rusm_rs::ws::Handler for Room {
    fn open(&mut self, conn: Pid) {
        self.members.push(conn);
        rusm_rs::ws::send(conn, b"welcome");
    }
    fn message(&mut self, _conn: Pid, data: Vec<u8>) {
        for &member in &self.members {
            rusm_rs::ws::send(member, &data); // broadcast
        }
    }
    fn close(&mut self, conn: Pid) {
        self.members.retain(|&m| m != conn);
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
        rusm_rs::ws::serve(Room::default()); // never returns
    }
}

export!(Component);
