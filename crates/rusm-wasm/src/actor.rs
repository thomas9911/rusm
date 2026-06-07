//! The **actor host ABI**: binds the `rusm:runtime` WIT world and implements its
//! `actor` interface on [`WasiHost`] as thin calls into `rusm-otp`.
//!
//! This is the lift→call-OTP→lower glue — a guest's `send`/`receive`/`list`/`kill`
//! become `rusm-otp` operations. `receive` is **async**, so a guest blocked on its
//! mailbox suspends its fiber and frees the Tokio worker (the "write blocking code,
//! get async" property). The runtime stays the source of truth; this never
//! reimplements OTP.

use rusm_otp::{stream, ExitReason, Pid, Received};

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

    /// Spawn a registered component by name as a new process — the actor model's
    /// `spawn`, the unlock for per-request workers and concealed typed clients.
    /// Capability-gated (`allow-spawn`, default-deny) and **non-escalating**: the
    /// child inherits *this* process's capabilities, so it is never more privileged
    /// than its parent. Errors (rather than trapping) on a denied or unknown spawn.
    async fn spawn(&mut self, component: String) -> Result<u64, String> {
        if !self.caps.can_spawn() {
            return Err("spawn denied: missing the spawn capability".to_string());
        }
        let spawner = self.spawner.as_ref().ok_or("spawn unavailable here")?;
        let entry = spawner
            .lookup(&component)
            .ok_or_else(|| format!("unknown component `{component}`"))?;
        // No escalation: the child gets exactly this process's capabilities.
        let child = spawner.spawn_component(&entry.prepared, self.caps.clone());
        // A TS service carries its bundle as message 1 (the js-runner's protocol).
        if let Some(bundle) = &entry.bundle {
            self.rt
                .send(Pid::from_raw(child.pid().raw()), (**bundle).clone());
        }
        Ok(child.pid().raw())
    }

    /// Monitor `target`: when it dies, this process receives a `__down` message
    /// (Erlang's `monitor` — the basis for a guest `Supervisor`). Capability-gated
    /// like spawn (supervisors pair spawn + monitor). No watcher process and no
    /// polling: the runtime's monitor delivers the `Down`, which `receive`
    /// translates — event-driven and cheap.
    async fn monitor(&mut self, target: u64) {
        if self.caps.can_spawn() || self.caps.process_control() {
            self.rt
                .monitor(Pid::from_raw(self.pid), Pid::from_raw(target));
        }
    }

    async fn send(&mut self, to: u64, message: Vec<u8>) {
        self.rt.send(Pid::from_raw(to), message);
    }

    async fn receive(&mut self) -> Vec<u8> {
        let ctx = self
            .ctx
            .as_mut()
            .expect("receive runs inside a spawned process");
        loop {
            match ctx.recv().await {
                Received::Message(bytes) => return bytes,
                // A monitored process died — surface it as a `__down` message so a
                // supervisor's `receive` sees it (Erlang delivers Down to the mailbox).
                Received::Down { pid, reason, .. } => {
                    let reason = down_reason(reason);
                    return format!(r#"{{"__down":"{}","reason":"{reason}"}}"#, pid.raw())
                        .into_bytes();
                }
                _ => {} // streams / other signals are skipped here
            }
        }
    }

    async fn list_processes(&mut self) -> Vec<u64> {
        // Default-deny: without process-control a guest sees only itself.
        if !self.caps.process_control() {
            return vec![self.pid];
        }
        self.rt.list().into_iter().map(|p| p.raw()).collect()
    }

    async fn info(&mut self, target: u64) -> Option<actor::ProcessInfo> {
        if !self.caps.process_control() && target != self.pid {
            return None; // may inspect only itself
        }
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
        if !self.caps.process_control() && target != self.pid {
            return false; // may probe only itself
        }
        self.rt.is_alive(Pid::from_raw(target))
    }

    async fn kill(&mut self, target: u64) -> bool {
        if !self.caps.process_control() && target != self.pid {
            return false; // may terminate only itself
        }
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

/// The wire name for an exit reason carried in a `__down` message.
fn down_reason(reason: ExitReason) -> &'static str {
    match reason {
        ExitReason::Normal => "normal",
        ExitReason::Killed => "killed",
        ExitReason::Crashed => "crashed",
        ExitReason::NoProc => "noproc",
    }
}
