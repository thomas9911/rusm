//! The **actor host ABI**: binds the `rusm:runtime` WIT world and implements its
//! `actor` interface on [`WasiHost`] as thin calls into `rusm-otp`.
//!
//! This is the lift→call-OTP→lower glue — a guest's `send`/`receive`/`list`/`kill`
//! become `rusm-otp` operations. `receive` is **async**, so a guest blocked on its
//! mailbox suspends its fiber and frees the Tokio worker (the "write blocking code,
//! get async" property). The runtime stays the source of truth; this never
//! reimplements OTP.

use rusm_otp::{stream, Pid, Received};

use crate::bridges::WasiHost;

wasmtime::component::bindgen!({
    world: "process",
    path: "wit",
    imports: { default: async },
    exports: { default: async },
});

use rusm::runtime::actor;

/// Wires the actor interface into a component linker.
pub(crate) fn add_to_linker(
    linker: &mut wasmtime::component::Linker<WasiHost>,
) -> wasmtime::Result<()> {
    actor::add_to_linker::<_, wasmtime::component::HasSelf<WasiHost>>(linker, |host| host)
}

impl actor::Host for WasiHost {
    async fn own_pid(&mut self) -> u64 {
        self.pid
    }

    async fn send(&mut self, to: u64, message: Vec<u8>) {
        self.rt.send(Pid::from_raw(to), message);
    }

    async fn receive(&mut self) -> Vec<u8> {
        let ctx = self
            .ctx
            .as_mut()
            .expect("receive runs inside a spawned process");
        // Deliver user messages; skip signals/streams (richer receive arrives in
        // later phases).
        loop {
            if let Received::Message(bytes) = ctx.recv().await {
                return bytes;
            }
        }
    }

    async fn list_processes(&mut self) -> Vec<u64> {
        self.rt.list().into_iter().map(|p| p.raw()).collect()
    }

    async fn info(&mut self, target: u64) -> Option<actor::ProcessInfo> {
        self.rt
            .info(Pid::from_raw(target))
            .map(|i| actor::ProcessInfo {
                pid: i.pid.raw(),
                links: i.links as u32,
                monitors: i.monitors as u32,
                names: i.names,
                label: i.label,
                mailbox_depth: i.mailbox_depth as u32,
                trap_exit: i.trap_exit,
            })
    }

    async fn is_alive(&mut self, target: u64) -> bool {
        self.rt.is_alive(Pid::from_raw(target))
    }

    async fn kill(&mut self, target: u64) -> bool {
        self.rt.kill(Pid::from_raw(target))
    }

    async fn register(&mut self, name: String) -> bool {
        self.rt.register(name, Pid::from_raw(self.pid))
    }

    async fn whereis(&mut self, name: String) -> Option<u64> {
        self.rt.whereis(&name).map(|p| p.raw())
    }

    async fn unregister(&mut self, name: String) -> bool {
        self.rt.unregister(&name)
    }

    async fn set_label(&mut self, label: String) {
        self.rt.set_label(Pid::from_raw(self.pid), label);
    }

    async fn stream_open(&mut self, to: u64) -> Option<u64> {
        let (writer, reader) = stream();
        if !self.rt.send_stream(Pid::from_raw(to), reader) {
            return None; // target gone
        }
        let id = self.next_stream;
        self.next_stream += 1;
        self.out_streams.insert(id, writer);
        Some(id)
    }

    async fn stream_write(&mut self, handle: u64, chunk: Vec<u8>) -> bool {
        // Clone the writer out so the await holds no borrow of the store.
        match self.out_streams.get(&handle).cloned() {
            Some(writer) => writer.write(chunk).await.is_ok(),
            None => false,
        }
    }

    async fn stream_close(&mut self, handle: u64) {
        self.out_streams.remove(&handle); // dropping the writer signals EOF
    }

    async fn stream_accept(&mut self) -> u64 {
        let ctx = self
            .ctx
            .as_mut()
            .expect("stream-accept runs inside a spawned process");
        // Like `receive`, deliver only streams here; skip plain messages/signals.
        let reader = loop {
            if let Received::Stream(handle) = ctx.recv().await {
                break handle;
            }
        };
        let id = self.next_stream;
        self.next_stream += 1;
        self.in_streams.insert(id, reader);
        id
    }

    async fn stream_read(&mut self, handle: u64) -> Option<Vec<u8>> {
        // Take the reader out (it isn't Clone — single consumer), await, re-insert
        // unless the stream has ended.
        let mut reader = self.in_streams.remove(&handle)?;
        match reader.read().await {
            Some(chunk) => {
                self.in_streams.insert(handle, reader);
                Some(chunk)
            }
            None => None, // end of stream
        }
    }
}
