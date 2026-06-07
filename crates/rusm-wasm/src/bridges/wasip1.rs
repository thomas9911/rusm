//! The **wasip1 bridge**: run a WASI **preview1 core module** as a `rusm-otp`
//! process — RUSM on Lunatic's home turf (Lunatic hosts *only* wasip1 core
//! modules), but with the same instance-per-process isolation, epoch preemption,
//! pooling allocator and default-deny capabilities as the component path.
//!
//! Two things distinguish a core module from a component:
//!  1. **WASI is preview1** (`wasi_snapshot_preview1`), wired via [`WasiP1Ctx`].
//!  2. **The actor ABI is raw** — a core module can't pass a WIT `list<u8>`, so the
//!     `rusm:runtime` world becomes flat `rusm::*` imports that marshal through the
//!     guest's exported linear `memory` (pointer + length). The host functions are
//!     the *same* thin calls into `rusm-otp` as the component [`actor`](crate::actor)
//!     glue; only the calling convention differs.
//!
//! The shared engine/epoch/pooling levers live in [`crate`]; this file is only the
//! core-module-specific glue (host type, raw ABI, prepare/spawn/run).

use std::sync::atomic::Ordering;
use std::sync::Arc;

use rusm_otp::{Context, ExitReason, Pid, ProcessHandle, Received, Runtime};
use wasmtime::Result;
use wasmtime::{
    Caller, Engine, Extern, InstancePre, Linker, Memory, Module, ModuleExport, ResourceLimiter,
    Store,
};
use wasmtime_wasi::p1::WasiP1Ctx;

use crate::caps::{Capabilities, CapabilityProfile};
use crate::{Counters, WasmRuntime};

/// Store data for a **core-module** guest: its preview1 WASI context, a per-process
/// memory ceiling enforced as a [`ResourceLimiter`], and the actor handles (pid,
/// runtime, mailbox, shared counters) backing the raw `rusm::*` host ABI. The
/// component path's analogue is [`WasiHost`](super::WasiHost); this one carries a
/// [`WasiP1Ctx`] instead of a component `WasiCtx`/`ResourceTable`.
pub(crate) struct CoreHost {
    wasi: WasiP1Ctx,
    /// Logical linear-memory cap (bytes) from the process's capabilities.
    max_memory: usize,
    /// The owning process's pid (for `own_pid`/`register`/`set_label`).
    pid: u64,
    /// Runtime-wide counters (the `notify` progress signal).
    shared: Arc<Counters>,
    /// Handle to the actor runtime, backing the actor host functions.
    rt: Runtime,
    /// The process's mailbox, for `receive`. `None` only for a bare host built
    /// outside a spawned process (a direct-instantiation test).
    ctx: Option<Context>,
}

impl ResourceLimiter for CoreHost {
    /// Denies growth past the capability's memory ceiling — `memory.grow` then
    /// returns -1 to the guest (no host trap), the standard sandbox signal.
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool> {
        Ok(desired <= self.max_memory)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool> {
        Ok(true)
    }
}

/// A core module whose host imports are resolved **once** and whose entry-export
/// index is precomputed — so a spawn skips both per-spawn import resolution *and*
/// the by-name export lookup (the [`ModuleExport`] is resolved at prepare time).
/// Opaque on purpose: it hides the internal host type.
#[derive(Clone)]
pub struct PreparedModule {
    pre: InstancePre<CoreHost>,
    entry: ModuleExport,
}

/// The guest's exported linear memory, named `memory` by convention (every
/// `wasm32-wasip1` module exports it). Absent → the guest can't use the byte ABI.
fn memory_of(caller: &mut Caller<'_, CoreHost>) -> Result<Memory> {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| wasmtime::Error::msg("guest has no exported `memory`"))
}

/// Reads `len` bytes from guest memory at `ptr` (a bad range traps the guest).
fn read_bytes(caller: &mut Caller<'_, CoreHost>, ptr: i32, len: i32) -> Result<Vec<u8>> {
    let mem = memory_of(caller)?;
    let mut buf = vec![0u8; len.max(0) as usize];
    mem.read(&caller, ptr as usize, &mut buf)?;
    Ok(buf)
}

/// Reads a UTF-8 string (a name/label) from guest memory.
fn read_string(caller: &mut Caller<'_, CoreHost>, ptr: i32, len: i32) -> Result<String> {
    String::from_utf8(read_bytes(caller, ptr, len)?)
        .map_err(|_| wasmtime::Error::msg("name is not valid UTF-8"))
}

/// Builds the core-module linker once: preview1 WASI plus the raw `rusm::*` actor
/// ABI. Every actor function is the same call into `rusm-otp` as the component
/// world (see [`crate::actor`]); the difference is purely the calling convention —
/// scalars and `(ptr, len)` windows into the guest's linear memory.
pub(crate) fn build_linker(engine: &Engine) -> Result<Linker<CoreHost>> {
    let mut linker = Linker::new(engine);
    wasmtime_wasi::p1::add_to_linker_async(&mut linker, |h: &mut CoreHost| &mut h.wasi)?;

    // own_pid() -> pid
    linker.func_wrap("rusm", "own_pid", |caller: Caller<'_, CoreHost>| {
        caller.data().pid as i64
    })?;
    // notify(): bump the runtime-wide progress counter (the fairness signal).
    linker.func_wrap("rusm", "notify", |caller: Caller<'_, CoreHost>| {
        caller
            .data()
            .shared
            .notifications
            .fetch_add(1, Ordering::Relaxed);
    })?;
    // send(to, ptr, len): copy the guest's bytes into `to`'s mailbox.
    linker.func_wrap(
        "rusm",
        "send",
        |mut caller: Caller<'_, CoreHost>, to: i64, ptr: i32, len: i32| -> Result<()> {
            let bytes = read_bytes(&mut caller, ptr, len)?;
            caller.data().rt.send(Pid::from_raw(to as u64), bytes);
            Ok(())
        },
    )?;
    // list_processes(ptr, cap) -> count: write up to `cap` pids (u64 LE) at `ptr`,
    // return the *total* count (so the guest can tell if it was truncated).
    linker.func_wrap(
        "rusm",
        "list_processes",
        |mut caller: Caller<'_, CoreHost>, ptr: i32, cap: i32| -> Result<i32> {
            let pids = caller.data().rt.list();
            let n = pids.len().min(cap.max(0) as usize);
            let mut buf = Vec::with_capacity(n * 8);
            for p in &pids[..n] {
                buf.extend_from_slice(&p.raw().to_le_bytes());
            }
            let mem = memory_of(&mut caller)?;
            mem.write(&mut caller, ptr as usize, &buf)?;
            Ok(pids.len() as i32)
        },
    )?;
    // is_alive(pid) -> bool
    linker.func_wrap(
        "rusm",
        "is_alive",
        |caller: Caller<'_, CoreHost>, target: i64| -> i32 {
            caller.data().rt.is_alive(Pid::from_raw(target as u64)) as i32
        },
    )?;
    // kill(pid) -> bool
    linker.func_wrap(
        "rusm",
        "kill",
        |caller: Caller<'_, CoreHost>, target: i64| -> i32 {
            caller.data().rt.kill(Pid::from_raw(target as u64)) as i32
        },
    )?;
    // register(ptr, len) -> bool: register the caller under the given name.
    linker.func_wrap(
        "rusm",
        "register",
        |mut caller: Caller<'_, CoreHost>, ptr: i32, len: i32| -> Result<i32> {
            let name = read_string(&mut caller, ptr, len)?;
            let pid = caller.data().pid;
            Ok(caller.data().rt.register(name, Pid::from_raw(pid)) as i32)
        },
    )?;
    // whereis(ptr, len) -> pid (or -1 if no process holds the name).
    linker.func_wrap(
        "rusm",
        "whereis",
        |mut caller: Caller<'_, CoreHost>, ptr: i32, len: i32| -> Result<i64> {
            let name = read_string(&mut caller, ptr, len)?;
            Ok(caller
                .data()
                .rt
                .whereis(&name)
                .map_or(-1, |p| p.raw() as i64))
        },
    )?;
    // unregister(ptr, len) -> bool
    linker.func_wrap(
        "rusm",
        "unregister",
        |mut caller: Caller<'_, CoreHost>, ptr: i32, len: i32| -> Result<i32> {
            let name = read_string(&mut caller, ptr, len)?;
            Ok(caller.data().rt.unregister(&name) as i32)
        },
    )?;
    // set_label(ptr, len): set the caller's human-readable label (observability).
    linker.func_wrap(
        "rusm",
        "set_label",
        |mut caller: Caller<'_, CoreHost>, ptr: i32, len: i32| -> Result<()> {
            let label = read_string(&mut caller, ptr, len)?;
            let pid = caller.data().pid;
            caller.data().rt.set_label(Pid::from_raw(pid), label);
            Ok(())
        },
    )?;
    // receive(ptr, cap) -> len: **async** — park the fiber until a user message
    // arrives (freeing the Tokio worker), then write up to `cap` bytes at `ptr` and
    // return the message's true length. Mirrors the component `receive`: signals
    // and streams are skipped, only user messages are delivered.
    linker.func_wrap_async(
        "rusm",
        "receive",
        |mut caller: Caller<'_, CoreHost>, (ptr, cap): (i32, i32)| {
            Box::new(async move {
                let bytes = {
                    let ctx = caller
                        .data_mut()
                        .ctx
                        .as_mut()
                        .ok_or_else(|| wasmtime::Error::msg("receive requires a mailbox"))?;
                    loop {
                        if let Received::Message(b) = ctx.recv().await {
                            break b;
                        }
                    }
                };
                let n = bytes.len().min(cap.max(0) as usize);
                let mem = memory_of(&mut caller)?;
                mem.write(&mut caller, ptr as usize, &bytes[..n])?;
                Ok(bytes.len() as i32)
            })
        },
    )?;
    Ok(linker)
}

impl WasmRuntime {
    /// Resolves a core module's host imports **once** against the preview1 + actor
    /// linker and precomputes its `entry` export index — the fast path for spawning
    /// the same module+entry many times. Errors if the export is missing.
    pub fn prepare(&self, module: &Module, entry: &str) -> Result<PreparedModule> {
        let pre = self.linker.instantiate_pre(module)?;
        let entry = module
            .get_export_index(entry)
            .ok_or_else(|| wasmtime::Error::msg(format!("module has no `{entry}` export")))?;
        Ok(PreparedModule { pre, entry })
    }

    /// Spawns a prepared core module as an isolated process under the default-deny
    /// **`Sandboxed`** profile. Use [`spawn_with`](WasmRuntime::spawn_with) to grant
    /// more.
    pub fn spawn(&self, prepared: &PreparedModule) -> ProcessHandle {
        self.spawn_with(prepared, CapabilityProfile::Sandboxed.capabilities())
    }

    /// Spawns a prepared core module as an isolated process running its entry export
    /// under the given [`Capabilities`]. A fresh instance + preview1 WASI context per
    /// process; a trap (or a denied capability the guest turns into a trap) exits the
    /// process [`Crashed`](ExitReason::Crashed).
    pub fn spawn_with(&self, prepared: &PreparedModule, caps: Capabilities) -> ProcessHandle {
        let engine = self.engine.clone();
        let rt = self.rt.clone();
        let pre = prepared.pre.clone();
        let entry = prepared.entry;
        let shared = Arc::clone(&self.shared);
        self.rt
            .spawn(move |ctx| run(engine, pre, entry, caps, rt, shared, ctx))
    }
}

/// The process body for a core module: build its preview1 WASI context, instantiate
/// it in a fresh store, and run its entry export — exiting
/// [`Crashed`](ExitReason::Crashed) on any failure. `rt` is moved into the host and
/// the crash-exit reads it back through the store, so the runtime handle is cloned
/// exactly once. Yields to the scheduler on each epoch tick.
#[allow(clippy::too_many_arguments)]
async fn run(
    engine: Engine,
    pre: InstancePre<CoreHost>,
    entry: ModuleExport,
    caps: Capabilities,
    rt: Runtime,
    shared: Arc<Counters>,
    ctx: Context,
) {
    let pid = ctx.pid();
    let wasi = match caps.build_wasi_p1() {
        Ok(wasi) => wasi,
        Err(_) => {
            rt.exit(pid, ExitReason::Crashed);
            return;
        }
    };
    let host = CoreHost {
        wasi,
        max_memory: caps.memory_limit(),
        pid: pid.raw(),
        shared,
        rt,
        ctx: Some(ctx),
    };
    let mut store = Store::new(&engine, host);
    // Enforce the per-process memory ceiling (CoreHost is the ResourceLimiter).
    store.limiter(|host| host as &mut dyn ResourceLimiter);
    store.set_epoch_deadline(1);
    store.epoch_deadline_async_yield_and_update(1);

    let outcome = async {
        let instance = pre.instantiate_async(&mut store).await?;
        // Precomputed index — no per-spawn by-name export lookup.
        let func = instance
            .get_module_export(&mut store, &entry)
            .and_then(Extern::into_func)
            .ok_or_else(|| wasmtime::Error::msg("entry export is not a function"))?;
        func.typed::<(), ()>(&store)?
            .call_async(&mut store, ())
            .await
    }
    .await;
    if outcome.is_err() {
        // The host (and its runtime handle) is still in the store — no extra clone.
        store.data().rt.exit(pid, ExitReason::Crashed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Returns immediately — the simplest core-module process body.
    const NOOP: &str = r#"(module (func (export "run")))"#;

    const TRAPS: &str = r#"(module (func (export "run") unreachable))"#;

    const SPINS: &str = r#"(module (func (export "run") (loop (br 0))))"#;

    // Uses preview1 WASI: fills 8 bytes of linear memory with `random_get`, which
    // WASI provides by default. Running without a trap proves wasip1 is wired.
    const USES_WASI: &str = r#"(module
        (import "wasi_snapshot_preview1" "random_get"
            (func $random_get (param i32 i32) (result i32)))
        (memory (export "memory") 1)
        (func (export "run")
            (drop (call $random_get (i32.const 0) (i32.const 8)))))"#;

    // Starts with one page and tries to grow by two more; if growth is denied
    // (memory.grow returns -1) it traps — so a memory cap below 192 KiB crashes it.
    const GROWS: &str = r#"(module
        (memory (export "memory") 1)
        (func (export "run")
            (if (i32.eq (memory.grow (i32.const 2)) (i32.const -1)) (then unreachable))))"#;

    // Sets a label, then blocks on `receive` forever — so a test can observe the
    // label via `info` before killing it.
    const SETS_LABEL: &str = r#"(module
        (import "rusm" "set_label" (func $set_label (param i32 i32)))
        (import "rusm" "receive" (func $receive (param i32 i32) (result i32)))
        (memory (export "memory") 1)
        (data (i32.const 0) "tagged")
        (func (export "run")
            (call $set_label (i32.const 0) (i32.const 6))
            (drop (call $receive (i32.const 64) (i32.const 8)))))"#;

    // Drives the *entire* raw actor ABI: receives [sender(8)][victim(8)], then
    // exercises own_pid/register/whereis(hit+miss)/is_alive/list_processes/
    // unregister/kill, accumulates a result flag per op, calls notify, and replies
    // [own_pid(8)][flags(1)] to the sender. The wasip1 twin of the component
    // `a_component_drives_the_whole_actor_abi` test.
    const ACTOR_ABI: &str = r#"(module
        (import "rusm" "own_pid" (func $own_pid (result i64)))
        (import "rusm" "register" (func $register (param i32 i32) (result i32)))
        (import "rusm" "whereis" (func $whereis (param i32 i32) (result i64)))
        (import "rusm" "unregister" (func $unregister (param i32 i32) (result i32)))
        (import "rusm" "is_alive" (func $is_alive (param i64) (result i32)))
        (import "rusm" "kill" (func $kill (param i64) (result i32)))
        (import "rusm" "list_processes" (func $list (param i32 i32) (result i32)))
        (import "rusm" "send" (func $send (param i64 i32 i32)))
        (import "rusm" "receive" (func $receive (param i32 i32) (result i32)))
        (import "rusm" "notify" (func $notify))
        (memory (export "memory") 1)
        (data (i32.const 100) "worker")
        (data (i32.const 110) "absent")
        (func (export "run")
            (local $self i64) (local $sender i64) (local $victim i64) (local $flags i32)
            (drop (call $receive (i32.const 0) (i32.const 16)))
            (local.set $sender (i64.load (i32.const 0)))
            (local.set $victim (i64.load (i32.const 8)))
            (local.set $self (call $own_pid))
            (if (i32.eq (call $register (i32.const 100) (i32.const 6)) (i32.const 1))
                (then (local.set $flags (i32.or (local.get $flags) (i32.const 1)))))
            (if (i64.eq (call $whereis (i32.const 100) (i32.const 6)) (local.get $self))
                (then (local.set $flags (i32.or (local.get $flags) (i32.const 2)))))
            (if (i64.eq (call $whereis (i32.const 110) (i32.const 6)) (i64.const -1))
                (then (local.set $flags (i32.or (local.get $flags) (i32.const 4)))))
            (if (i32.eq (call $is_alive (local.get $self)) (i32.const 1))
                (then (local.set $flags (i32.or (local.get $flags) (i32.const 8)))))
            (if (i32.ge_s (call $list (i32.const 300) (i32.const 16)) (i32.const 1))
                (then (local.set $flags (i32.or (local.get $flags) (i32.const 16)))))
            (if (i32.eq (call $unregister (i32.const 100) (i32.const 6)) (i32.const 1))
                (then (local.set $flags (i32.or (local.get $flags) (i32.const 32)))))
            (if (i32.eq (call $kill (local.get $victim)) (i32.const 1))
                (then (local.set $flags (i32.or (local.get $flags) (i32.const 64)))))
            (call $notify)
            (i64.store (i32.const 32) (local.get $self))
            (i32.store8 (i32.const 40) (local.get $flags))
            (call $send (local.get $sender) (i32.const 32) (i32.const 9))))"#;

    /// Monitors a freshly spawned guest and returns the exit reason it observes.
    async fn exit_reason_of(rt: &Runtime, guest: &ProcessHandle) -> ExitReason {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let watcher = rt
            .spawn(move |mut ctx| async move {
                let _ = tx.send(ctx.recv().await);
            })
            .pid();
        rt.monitor(watcher, guest.pid());
        match rx.await.unwrap() {
            Received::Down { reason, .. } => reason,
            other => panic!("expected a Down, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_core_module_runs_as_a_process_and_is_reaped() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr.prepare(&wr.compile(NOOP).unwrap(), "run").unwrap();
        wr.spawn(&pre).join().await;
        assert_eq!(rt.finished(), 1);
        assert_eq!(rt.process_count(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_trapping_core_module_crashes_the_process() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr.prepare(&wr.compile(TRAPS).unwrap(), "run").unwrap();
        let guest = wr.spawn(&pre);
        assert_eq!(exit_reason_of(&rt, &guest).await, ExitReason::Crashed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_missing_entry_export_is_an_error() {
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        assert!(wr.prepare(&wr.compile(NOOP).unwrap(), "nope").is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_core_module_uses_wasip1_wasi() {
        // random_get is granted to every process; the guest runs without a trap.
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr.prepare(&wr.compile(USES_WASI).unwrap(), "run").unwrap();
        let guest = wr.spawn(&pre);
        assert_eq!(exit_reason_of(&rt, &guest).await, ExitReason::Normal);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_memory_cap_crashes_a_core_module_that_grows_past_it() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr.prepare(&wr.compile(GROWS).unwrap(), "run").unwrap();
        // One page: the two-page growth is denied → trap → Crashed.
        let capped = wr.spawn_with(&pre, Capabilities::nothing().max_memory(64 << 10));
        assert_eq!(exit_reason_of(&rt, &capped).await, ExitReason::Crashed);
        // Room for the growth → normal exit.
        let roomy = wr.spawn_with(&pre, Capabilities::nothing().max_memory(256 << 10));
        assert_eq!(exit_reason_of(&rt, &roomy).await, ExitReason::Normal);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_core_module_sets_its_label_via_the_raw_abi() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr.prepare(&wr.compile(SETS_LABEL).unwrap(), "run").unwrap();
        let guest = wr.spawn(&pre);
        let pid = guest.pid();
        // The guest sets its label then blocks; observe it, then kill it.
        let mut labelled = false;
        for _ in 0..200 {
            if rt.info(pid).and_then(|i| i.label).as_deref() == Some("tagged") {
                labelled = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(labelled, "the guest must set its label via the raw ABI");
        guest.kill();
        guest.join().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_core_module_drives_the_whole_raw_actor_abi() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr.prepare(&wr.compile(ACTOR_ABI).unwrap(), "run").unwrap();

        // A victim the guest will kill via the ABI.
        let victim = rt.spawn(|_| std::future::pending::<()>());
        let victim_pid = victim.pid();

        let guest = wr.spawn(&pre);
        let guest_pid = guest.pid();

        // A native process pings the guest with [its pid][victim pid] and awaits
        // the guest's reply [guest pid][flags].
        let (tx, rx) = tokio::sync::oneshot::channel();
        let ping_rt = rt.clone();
        rt.spawn(move |mut ctx| async move {
            let mut msg = ctx.pid().raw().to_le_bytes().to_vec();
            msg.extend_from_slice(&victim_pid.raw().to_le_bytes());
            ping_rt.send(guest_pid, msg);
            let _ = tx.send(ctx.recv().await.message().unwrap());
        });

        let reply = rx.await.unwrap();
        let reported = u64::from_le_bytes(reply[0..8].try_into().unwrap());
        assert_eq!(reported, guest_pid.raw(), "own_pid via the raw ABI");
        // All seven ops succeeded: register, whereis(hit), whereis(miss),
        // is_alive, list_processes, unregister, kill.
        assert_eq!(reply[8], 0b0111_1111, "every raw actor op should succeed");
        assert!(wr.notifications() >= 1, "the guest called notify");

        // Observable effects: the victim was killed and the name released.
        for _ in 0..200 {
            if !rt.is_alive(victim_pid) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(!rt.is_alive(victim_pid), "the guest killed the victim");
        assert_eq!(rt.whereis("worker"), None, "the guest released its name");
        guest.join().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_infinite_loop_core_module_yields_and_stays_killable() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr.prepare(&wr.compile(SPINS).unwrap(), "run").unwrap();

        // A bystander must still run alongside the spinner — proof of preemption.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let bystander = rt.spawn(move |_| async move {
            let _ = tx.send(());
        });

        let spinner = wr.spawn(&pre);
        let spinner_pid = spinner.pid();
        rx.await.unwrap();
        bystander.join().await;

        assert!(rt.is_alive(spinner_pid));
        spinner.kill();
        spinner.join().await;
        assert!(!rt.is_alive(spinner_pid));
    }

    // --- Misbehaving guests: the sandbox must turn each into a clean Crash, never
    // a host panic or silent corruption. These exercise the raw ABI's error paths.

    // Calls `send` but exports no `memory` — the host can't read the payload.
    const NO_MEMORY: &str = r#"(module
        (import "rusm" "send" (func $send (param i64 i32 i32)))
        (func (export "run") (call $send (i64.const 0) (i32.const 0) (i32.const 4))))"#;

    // Calls `send` with a pointer far outside its one page of linear memory.
    const BAD_POINTER: &str = r#"(module
        (import "rusm" "send" (func $send (param i64 i32 i32)))
        (memory (export "memory") 1)
        (func (export "run") (call $send (i64.const 0) (i32.const 1000000) (i32.const 16))))"#;

    // Registers a name whose bytes are not valid UTF-8.
    const BAD_UTF8: &str = r#"(module
        (import "rusm" "register" (func $register (param i32 i32) (result i32)))
        (memory (export "memory") 1)
        (data (i32.const 0) "\ff\fe\fd")
        (func (export "run") (drop (call $register (i32.const 0) (i32.const 3)))))"#;

    // Exports `run` as a global, not a function.
    const RUN_NOT_A_FUNC: &str = r#"(module (global (export "run") i32 (i32.const 0)))"#;

    // Receives into a pointer outside its linear memory — the write must fail.
    const BAD_RECEIVE: &str = r#"(module
        (import "rusm" "receive" (func $receive (param i32 i32) (result i32)))
        (memory (export "memory") 1)
        (func (export "run") (drop (call $receive (i32.const 1000000) (i32.const 16)))))"#;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_guest_without_memory_crashes_instead_of_panicking() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr.prepare(&wr.compile(NO_MEMORY).unwrap(), "run").unwrap();
        let guest = wr.spawn(&pre);
        assert_eq!(exit_reason_of(&rt, &guest).await, ExitReason::Crashed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_out_of_bounds_pointer_crashes_the_guest() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr
            .prepare(&wr.compile(BAD_POINTER).unwrap(), "run")
            .unwrap();
        let guest = wr.spawn(&pre);
        assert_eq!(exit_reason_of(&rt, &guest).await, ExitReason::Crashed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_non_utf8_name_crashes_the_guest() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr.prepare(&wr.compile(BAD_UTF8).unwrap(), "run").unwrap();
        let guest = wr.spawn(&pre);
        assert_eq!(exit_reason_of(&rt, &guest).await, ExitReason::Crashed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_entry_that_is_not_a_function_crashes_the_process() {
        // `get_export_index` resolves any export named `run`; the spawn must then
        // crash gracefully when it turns out not to be a callable function.
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr
            .prepare(&wr.compile(RUN_NOT_A_FUNC).unwrap(), "run")
            .unwrap();
        let guest = wr.spawn(&pre);
        assert_eq!(exit_reason_of(&rt, &guest).await, ExitReason::Crashed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_receive_into_a_bad_pointer_crashes_the_guest() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr
            .prepare(&wr.compile(BAD_RECEIVE).unwrap(), "run")
            .unwrap();
        let guest = wr.spawn(&pre);
        // Deliver a message so `receive` wakes and attempts its out-of-bounds write.
        rt.send(guest.pid(), vec![1, 2, 3, 4]);
        assert_eq!(exit_reason_of(&rt, &guest).await, ExitReason::Crashed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unbuildable_capability_set_crashes_before_running() {
        // A preopen of a path that doesn't exist makes the WASI context fail to
        // build — the process must crash rather than run with a broken sandbox.
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr.prepare(&wr.compile(NOOP).unwrap(), "run").unwrap();
        let caps = Capabilities::nothing().preopen("/no/such/path/rusm-test", "/mnt", true);
        let guest = wr.spawn_with(&pre, caps);
        assert_eq!(exit_reason_of(&rt, &guest).await, ExitReason::Crashed);
    }
}
