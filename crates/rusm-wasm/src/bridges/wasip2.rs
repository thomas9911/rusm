//! The **wasip2 bridge**: run a WASI **component** as a `rusm-otp` process.
//!
//! Mirrors the core-module path in [`crate`] (`compile`/`prepare`/`spawn`) but for
//! the component model — `Component`, a component [`Linker`](ComponentLinker) with
//! WASI p2 wired in, and a per-process [`WasiHost`]. Instance-per-process, epoch
//! preemption and the pooling allocator all carry over unchanged; a trap exits the
//! process [`Crashed`](ExitReason::Crashed). The shared efficiency levers live in
//! `lib.rs`; this file is only the component-specific glue.

use anyhow::Result;
use rusm_otp::{Context, ExitReason, ProcessHandle, Runtime};
use wasmtime::component::{
    Component, ComponentExportIndex, InstancePre as ComponentInstancePre, Linker as ComponentLinker,
};
use wasmtime::{Engine, ResourceLimiter, Store};
use wasmtime_wasi::ResourceTable;

use super::WasiHost;
use crate::caps::{Capabilities, CapabilityProfile};
use crate::WasmRuntime;

/// A component whose imports are resolved **and** whose entry-export index is
/// precomputed — so a spawn skips both per-spawn import resolution *and* the
/// by-name export lookup. Opaque on purpose: it hides the internal WASI host type.
#[derive(Clone)]
pub struct PreparedComponent {
    pre: ComponentInstancePre<WasiHost>,
    /// The `entry` export resolved once at prepare time (index, not a string).
    entry: ComponentExportIndex,
}

/// Builds the component linker once, with WASI **p2 and p3** wired in plus the
/// `rusm:runtime` actor ABI — all sharing one [`WasiHost`]. A component importing
/// the `@0.2.0` or `@0.3.0` WASI interfaces resolves against the same host.
pub(crate) fn build_linker(engine: &Engine) -> Result<ComponentLinker<WasiHost>> {
    let mut linker = ComponentLinker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    super::wasip3::add_to_linker(&mut linker)?;
    crate::actor::add_to_linker(&mut linker)?;
    Ok(linker)
}

impl WasmRuntime {
    /// Compiles a component from Wasm bytes or component-model `.wat` text.
    pub fn compile_component(&self, wasm: impl AsRef<[u8]>) -> Result<Component> {
        Ok(Component::new(&self.engine, wasm)?)
    }

    /// Resolves a component's imports **once** against the WASI linker and
    /// precomputes its `entry` export index — the fast path for spawning the same
    /// component+entrypoint many times (no per-spawn import resolution or by-name
    /// export lookup). Errors if the component has no such export.
    pub fn prepare_component(
        &self,
        component: &Component,
        entry: &str,
    ) -> Result<PreparedComponent> {
        let pre = self.component_linker.instantiate_pre(component)?;
        let entry = component
            .get_export_index(None, entry)
            .ok_or_else(|| anyhow::anyhow!("component has no `{entry}` export"))?;
        Ok(PreparedComponent { pre, entry })
    }

    /// Spawns a prepared component as an isolated process under the **default-deny
    /// `Sandboxed`** profile (no fs/net/env, a bounded heap). Use
    /// [`spawn_component_with`](WasmRuntime::spawn_component_with) to grant more.
    pub fn spawn_component(&self, prepared: &PreparedComponent) -> ProcessHandle {
        self.spawn_component_with(prepared, CapabilityProfile::Sandboxed.capabilities())
    }

    /// Spawns a prepared component as an isolated process running its entry export,
    /// under the given [`Capabilities`]. A fresh instance + WASI context per
    /// process; a trap (or a denied capability the guest turns into a trap) exits
    /// the process [`Crashed`](ExitReason::Crashed).
    pub fn spawn_component_with(
        &self,
        prepared: &PreparedComponent,
        caps: Capabilities,
    ) -> ProcessHandle {
        let engine = self.engine.clone();
        let rt = self.rt.clone();
        let pre = prepared.pre.clone();
        let entry = prepared.entry;
        self.rt
            .spawn(move |ctx| run(engine, pre, entry, caps, rt, ctx))
    }
}

/// The process body for a component: build its WASI context, instantiate it in a
/// fresh store, and run its entry export — exiting [`Crashed`](ExitReason::Crashed)
/// on any failure. `rt` is moved into the host (one clone per spawn), and the
/// crash-exit reads it back through the store, so the runtime handle is cloned
/// exactly once. Yields to the scheduler on each epoch tick.
async fn run(
    engine: Engine,
    pre: ComponentInstancePre<WasiHost>,
    entry: ComponentExportIndex,
    caps: Capabilities,
    rt: Runtime,
    ctx: Context,
) {
    let pid = ctx.pid();
    let wasi = match caps.build_wasi() {
        Ok(wasi) => wasi,
        Err(_) => {
            rt.exit(pid, ExitReason::Crashed);
            return;
        }
    };
    let host = WasiHost {
        wasi,
        table: ResourceTable::new(),
        max_memory: caps.memory_limit(),
        pid: pid.raw(),
        rt,
        ctx: Some(ctx),
    };
    let mut store = Store::new(&engine, host);
    // Enforce the per-process memory ceiling (WasiHost is the ResourceLimiter).
    store.limiter(|host| host as &mut dyn ResourceLimiter);
    store.set_epoch_deadline(1);
    store.epoch_deadline_async_yield_and_update(1);

    let outcome = async {
        let instance = pre.instantiate_async(&mut store).await?;
        // Precomputed index — no per-spawn by-name export lookup.
        let func = instance.get_typed_func::<(), ()>(&mut store, entry)?;
        func.call_async(&mut store, ()).await
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

    // A minimal component exporting `run: func()` — no imports, no WASI use.
    const COMP_RUN: &str = r#"(component
        (core module $m (func (export "run")))
        (core instance $i (instantiate $m))
        (func (export "run") (canon lift (core func $i "run"))))"#;

    const COMP_TRAP: &str = r#"(component
        (core module $m (func (export "run") unreachable))
        (core instance $i (instantiate $m))
        (func (export "run") (canon lift (core func $i "run"))))"#;

    const COMP_SPIN: &str = r#"(component
        (core module $m (func (export "run") (loop (br 0))))
        (core instance $i (instantiate $m))
        (func (export "run") (canon lift (core func $i "run"))))"#;

    // Starts with one page (64 KiB) and tries to grow by two more (to 192 KiB);
    // if growth is denied (memory.grow returns -1) it traps. So a memory cap below
    // 192 KiB makes it crash, a cap at/above it lets it finish normally.
    const COMP_GROW: &str = r#"(component
        (core module $m
            (memory (export "mem") 1)
            (func (export "run")
                (if (i32.eq (memory.grow (i32.const 2)) (i32.const -1))
                    (then unreachable))))
        (core instance $i (instantiate $m))
        (func (export "run") (canon lift (core func $i "run"))))"#;

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
            rusm_otp::Received::Down { reason, .. } => reason,
            other => panic!("expected a Down, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn components_call_each_other_via_the_actor_abi() {
        // Two instances of one component: the first registers "responder" and
        // serves request/reply; the second finds it via whereis and calls it,
        // forwarding the reply to a native collector — component-to-component
        // "callbacks" with no new runtime API, just the actor ABI.
        const CALLBACK: &[u8] = include_bytes!("../../tests/fixtures/callback.wasm");
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr
            .prepare_component(&wr.compile_component(CALLBACK).unwrap(), "run")
            .unwrap();

        // Native collector receives the caller's final result.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let collector = rt.spawn(move |mut ctx| async move {
            let _ = tx.send(ctx.recv().await.message().unwrap());
        });
        assert!(rt.register("collector", collector.pid()));

        // Instance 1 → responder; wait until it has registered the name.
        let _responder = wr.spawn_component(&pre);
        for _ in 0..200 {
            if rt.whereis("responder").is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(rt.whereis("responder").is_some(), "responder must register");

        // Instance 2 → caller: calls the responder (21 -> doubled -> 42).
        let _caller = wr.spawn_component(&pre);
        assert_eq!(rx.await.unwrap(), vec![42]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_memory_cap_crashes_a_component_that_grows_past_it() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr
            .prepare_component(&wr.compile_component(COMP_GROW).unwrap(), "run")
            .unwrap();
        // Cap at one page: the two-page growth is denied → the guest traps.
        let caps = Capabilities::nothing().max_memory(64 << 10);
        let guest = wr.spawn_component_with(&pre, caps);
        assert_eq!(exit_reason_of(&rt, &guest).await, ExitReason::Crashed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_component_drives_the_whole_actor_abi() {
        // A real Rust component (built for wasm32-wasip2, checked in) that calls
        // every rusm:runtime actor op and reports which succeeded.
        const ECHO: &[u8] = include_bytes!("../../tests/fixtures/actor_echo.wasm");
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr
            .prepare_component(&wr.compile_component(ECHO).unwrap(), "run")
            .unwrap();

        // A victim the guest will kill via the ABI.
        let victim = rt.spawn(|_| std::future::pending::<()>());
        let victim_pid = victim.pid();

        let guest = wr.spawn_component(&pre);
        let guest_pid = guest.pid();

        // A native process pings the guest with [its pid][victim pid], then awaits
        // the guest's reply: [guest pid][flags].
        let (tx, rx) = tokio::sync::oneshot::channel();
        let ping_rt = rt.clone();
        rt.spawn(move |mut ctx| async move {
            let mut msg = ctx.pid().raw().to_le_bytes().to_vec();
            msg.extend_from_slice(&victim_pid.raw().to_le_bytes());
            ping_rt.send(guest_pid, msg);
            let _ = tx.send(ctx.recv().await.message().unwrap());
        });

        let reply = rx.await.unwrap();
        let reported_pid = u64::from_le_bytes(reply[0..8].try_into().unwrap());
        let flags = reply[8];
        assert_eq!(reported_pid, guest_pid.raw(), "own-pid via the ABI");
        // All seven ops (register, whereis, info, list, is-alive, kill, unregister)
        // succeeded from inside the component.
        assert_eq!(flags, 0b0111_1111, "every actor op should succeed");

        // Observable effects: the guest killed the victim and released the name.
        for _ in 0..200 {
            if !rt.is_alive(victim_pid) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            !rt.is_alive(victim_pid),
            "the guest killed the victim via kill"
        );
        assert_eq!(rt.whereis("echo"), None, "the guest released its name");
        guest.join().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_generous_memory_cap_lets_a_component_grow() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr
            .prepare_component(&wr.compile_component(COMP_GROW).unwrap(), "run")
            .unwrap();
        // Cap above the 192 KiB it grows to: growth succeeds → normal exit.
        let caps = Capabilities::nothing().max_memory(256 << 10);
        let guest = wr.spawn_component_with(&pre, caps);
        assert_eq!(exit_reason_of(&rt, &guest).await, ExitReason::Normal);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_component_runs_as_a_process_and_is_reaped() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let component = wr.compile_component(COMP_RUN).unwrap();
        let pre = wr.prepare_component(&component, "run").unwrap();
        wr.spawn_component(&pre).join().await;
        assert_eq!(rt.finished(), 1);
        assert_eq!(rt.process_count(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_trapping_component_crashes_the_process() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let component = wr.compile_component(COMP_TRAP).unwrap();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let watcher = rt
            .spawn(move |mut ctx| async move {
                let _ = tx.send(ctx.recv().await);
            })
            .pid();
        let pre = wr.prepare_component(&component, "run").unwrap();
        let guest = wr.spawn_component(&pre);
        let reference = rt.monitor(watcher, guest.pid());
        let guest_pid = guest.pid();

        assert_eq!(
            rx.await.unwrap(),
            rusm_otp::Received::Down {
                reference,
                pid: guest_pid,
                reason: ExitReason::Crashed,
            }
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_spinning_component_yields_and_stays_killable() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let component = wr.compile_component(COMP_SPIN).unwrap();

        // A bystander must still run alongside the spinning component — proof the
        // epoch preempts the component just as it does a core module.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let bystander = rt.spawn(move |_| async move {
            let _ = tx.send(());
        });

        let pre = wr.prepare_component(&component, "run").unwrap();
        let spinner = wr.spawn_component(&pre);
        let spinner_pid = spinner.pid();
        rx.await.unwrap();
        bystander.join().await;

        assert!(rt.is_alive(spinner_pid));
        spinner.kill();
        spinner.join().await;
        assert!(!rt.is_alive(spinner_pid));
    }
}
