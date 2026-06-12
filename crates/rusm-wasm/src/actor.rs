//! The **actor host ABI**: binds the `rusm:runtime` WIT world and implements its
//! `actor` interface on [`WasiHost`] as thin calls into `rusm-otp`.
//!
//! This is the lift→call-OTP→lower glue — a guest's `send`/`receive`/`list`/`kill`
//! become `rusm-otp` operations. `receive` is **async**, so a guest blocked on its
//! mailbox suspends its fiber and frees the Tokio worker (the "write blocking code,
//! get async" property). The runtime stays the source of truth; this never
//! reimplements OTP.

use std::sync::Arc;
use std::time::Duration;

use rusm_otp::{stream, Context, ExitReason, Pid, Received, Runtime, Strategy};

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
        // A node-registered component runs under its **declared** profile (the manifest's
        // explicit per-component policy — what's declared is what runs); an ad-hoc
        // registration with no declared profile inherits this process's caps
        // (non-escalating). Either way the `spawn` capability above gates who may spawn.
        let caps = entry.caps.clone().unwrap_or_else(|| self.caps.clone());
        let child = spawner.spawn_component(&entry.prepared, caps);
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
        next_message(ctx).await
    }

    /// Erlang's `receive … after`: the next message, or `none` if `timeout_ms`
    /// elapses first. Built on `tokio::time::timeout` over the *same* receive loop
    /// as [`receive`] — `ctx.recv()` is cancel-safe (a dropped await leaves the
    /// mailbox untouched), so a timeout never loses a message. The basis for SSE
    /// heartbeats and any guest-side deadline without a busy poll.
    async fn receive_timeout(&mut self, timeout_ms: u64) -> Option<Vec<u8>> {
        let ctx = self
            .ctx
            .as_mut()
            .expect("receive-timeout runs inside a spawned process");
        tokio::time::timeout(Duration::from_millis(timeout_ms), next_message(ctx))
            .await
            .ok()
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

    /// Supervise named child components under the **native** `rusm-otp` supervisor —
    /// the single restart implementation the guest `Supervisor` SDKs delegate to.
    /// Capability-gated like `spawn`; each child is spawned with *this* process's
    /// capabilities (non-escalating). The supervisor is **linked to the caller** and
    /// **traps exits**: if it gives up (restart budget exceeded) the caller dies too,
    /// and if the caller dies the supervisor tears its children down — clean
    /// co-termination, no orphans.
    async fn supervise(
        &mut self,
        strategy: actor::SuperviseStrategy,
        children: Vec<String>,
        max_restarts: u32,
        within_ms: u32,
    ) -> Result<u64, String> {
        if !self.caps.can_spawn() {
            return Err("supervise denied: missing the spawn capability".to_string());
        }
        let spawner = self.spawner.as_ref().ok_or("supervise unavailable here")?;
        let strategy = match strategy {
            actor::SuperviseStrategy::OneForOne => Strategy::OneForOne,
            actor::SuperviseStrategy::OneForAll => Strategy::OneForAll,
            actor::SuperviseStrategy::RestForOne => Strategy::RestForOne,
        };
        let mut sup = self.rt.supervisor(strategy).max_restarts(max_restarts);
        sup = if within_ms == 0 {
            sup.over_lifetime()
        } else {
            sup.within(Duration::from_millis(within_ms as u64))
        };
        for name in children {
            let entry = spawner
                .lookup(&name)
                .ok_or_else(|| format!("unknown component `{name}`"))?;
            let prepared = entry.prepared.clone();
            let bundle = entry.bundle.clone();
            let caps = self.caps.clone();
            let spawner = Arc::clone(spawner);
            sup = sup.child(move |rt: &Runtime| {
                let child = spawner.spawn_component(&prepared, caps.clone());
                if let Some(bundle) = &bundle {
                    rt.send(Pid::from_raw(child.pid().raw()), (**bundle).clone());
                }
                child
            });
        }
        let sup_pid = Pid::from_raw(sup.start().pid().raw());
        // Co-terminate with the caller (see the doc comment).
        self.rt.set_trap_exit(sup_pid, true);
        self.rt.link(Pid::from_raw(self.pid), sup_pid);
        Ok(sup_pid.raw())
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

    // --- durable key-value storage (gated by the `storage` capability) ---
    // Each op resolves the node's store + the named bucket (capability-checked),
    // then delegates to `rusm-kv`, rendering its error as a message for the guest.

    async fn kv_get(&mut self, bucket: String, key: String) -> Result<Option<Vec<u8>>, String> {
        self.kv_bucket(&bucket)?
            .get(&key)
            .map_err(|e| e.to_string())
    }

    async fn kv_set(&mut self, bucket: String, key: String, value: Vec<u8>) -> Result<(), String> {
        self.kv_bucket(&bucket)?
            .set(&key, &value)
            .map_err(|e| e.to_string())
    }

    async fn kv_delete(&mut self, bucket: String, key: String) -> Result<bool, String> {
        self.kv_bucket(&bucket)?
            .delete(&key)
            .map_err(|e| e.to_string())
    }

    async fn kv_exists(&mut self, bucket: String, key: String) -> Result<bool, String> {
        self.kv_bucket(&bucket)?
            .exists(&key)
            .map_err(|e| e.to_string())
    }

    async fn kv_list(&mut self, bucket: String) -> Result<Vec<String>, String> {
        self.kv_bucket(&bucket)?.list().map_err(|e| e.to_string())
    }
}

impl WasiHost {
    /// Resolve the named bucket of the node's store, enforcing the **storage**
    /// capability (default-deny) and that a store is actually configured. Shared by
    /// every `kv-*` op so the gate lives in exactly one place.
    fn kv_bucket(&self, bucket: &str) -> Result<rusm_kv::Bucket, String> {
        if !self.caps.storage_allowed() {
            return Err("kv denied: missing the storage capability".to_string());
        }
        let store = self
            .spawner
            .as_ref()
            .and_then(|s| s.store.as_ref())
            .ok_or("kv unavailable: no store configured on this node")?;
        Ok(store.bucket(bucket))
    }
}

/// The shared receive loop behind `receive` and `receive-timeout`: return the next
/// *user-visible* mailbox item as message bytes — a plain message verbatim, or a
/// monitored `Down` rendered as a `__down` JSON message (Erlang delivers Down to
/// the mailbox, the basis for a guest `Supervisor`). Streams and other signals are
/// skipped. Kept free-standing so both callers borrow `ctx` and share one body.
async fn next_message(ctx: &mut Context) -> Vec<u8> {
    loop {
        match ctx.recv().await {
            Received::Message(bytes) => return bytes,
            Received::Down { pid, reason, .. } => {
                let reason = down_reason(reason);
                return format!(r#"{{"__down":"{}","reason":"{reason}"}}"#, pid.raw()).into_bytes();
            }
            _ => {} // streams / other signals are skipped here
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
