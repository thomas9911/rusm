//! `rusm-wasm` — the Wasmtime backend for RUSM.
//!
//! Each process runs as an **isolated Wasm instance**: [`WasmRuntime::spawn`]
//! starts a fresh instance as a [`rusm_otp`] process (one instance = one process
//! = one Tokio task). A guest trap becomes a process crash (so links/monitors
//! fire), and an **epoch** ticker preempts even an infinite-loop guest so it
//! yields cooperatively and stays killable — the BEAM's reduction counting, done
//! with Wasmtime epochs.
//!
//! Wasm lives *only* here; `rusm-otp` never references Wasmtime.

use std::time::Duration;

use anyhow::Result;
use rusm_otp::{ExitReason, Pid, ProcessHandle, Runtime};
use tokio::task::JoinHandle;
use wasmtime::{Caller, Config, Engine, Linker, Module, Store};

/// How often the epoch is bumped. A guest runs at most this long before it must
/// yield to the scheduler, so tight loops can't starve other processes.
const EPOCH_TICK: Duration = Duration::from_millis(10);

/// Per-instance host state reachable from host functions via `Caller::data`.
#[derive(Default)]
struct Host {
    /// The owning process's pid (0 for a bare instance with no process).
    pid: u64,
    /// Times the guest called the `notify` host function (host-ABI test hook).
    host_calls: u64,
}

/// Runs Wasm guests as RUSM processes.
pub struct WasmRuntime {
    engine: Engine,
    rt: Runtime,
    epoch_ticker: JoinHandle<()>,
}

impl WasmRuntime {
    /// Builds a backend over an existing process [`Runtime`]. Must run inside a
    /// Tokio runtime (it starts the epoch ticker).
    pub fn new(rt: Runtime) -> Result<Self> {
        let mut config = Config::new();
        // Epoch interruption: the preemption lever (see `EPOCH_TICK`). Async
        // support (fibers — a guest's "blocking" host call suspends the whole
        // call stack and yields the Tokio worker) is always available in
        // Wasmtime; we drive guests with `call_async`.
        config.epoch_interruption(true);
        let engine = Engine::new(&config)?;

        // Bump the epoch on a cadence; each increment is what lets a running guest
        // hit its deadline and yield.
        let ticker_engine = engine.clone();
        let epoch_ticker = tokio::spawn(async move {
            let mut interval = tokio::time::interval(EPOCH_TICK);
            loop {
                interval.tick().await;
                ticker_engine.increment_epoch();
            }
        });

        Ok(Self {
            engine,
            rt,
            epoch_ticker,
        })
    }

    /// Compiles a module from Wasm bytes or `.wat` text.
    pub fn compile(&self, wasm: impl AsRef<[u8]>) -> Result<Module> {
        Ok(Module::new(&self.engine, wasm)?)
    }

    /// Spawns `module` as an isolated process running its `entry` export (a
    /// `() -> ()` function). A fresh instance is created per process; a trap exits
    /// the process with [`ExitReason::Crashed`].
    pub fn spawn(&self, module: Module, entry: impl Into<String>) -> ProcessHandle {
        let engine = self.engine.clone();
        let rt = self.rt.clone();
        let entry = entry.into();
        self.rt.spawn(move |ctx| async move {
            let pid = ctx.pid();
            // The mailbox (in `ctx`) is unused until the messaging host ABI lands;
            // dropping it leaves the process alive, addressable via its abort handle.
            if run(&engine, &module, pid, &entry).await.is_err() {
                rt.exit(pid, ExitReason::Crashed);
            }
        })
    }
}

impl Drop for WasmRuntime {
    fn drop(&mut self) {
        self.epoch_ticker.abort();
    }
}

/// Host functions the guest imports under the `rusm` namespace.
fn link_host(engine: &Engine) -> Result<Linker<Host>> {
    let mut linker = Linker::new(engine);
    linker.func_wrap("rusm", "self_pid", |caller: Caller<'_, Host>| {
        caller.data().pid as i64
    })?;
    linker.func_wrap("rusm", "notify", |mut caller: Caller<'_, Host>| {
        caller.data_mut().host_calls += 1;
    })?;
    Ok(linker)
}

/// Instantiates `module` in a fresh store and runs its `entry` export, yielding
/// to the scheduler whenever the epoch deadline is reached.
async fn run(engine: &Engine, module: &Module, pid: Pid, entry: &str) -> Result<()> {
    let host = Host {
        pid: pid.raw(),
        host_calls: 0,
    };
    let mut store = Store::new(engine, host);
    // Yield to Tokio on each epoch tick, then run for one more tick.
    store.set_epoch_deadline(1);
    store.epoch_deadline_async_yield_and_update(1);

    let linker = link_host(engine)?;
    let instance = linker.instantiate_async(&mut store, module).await?;
    let func = instance.get_typed_func::<(), ()>(&mut store, entry)?;
    func.call_async(&mut store, ()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADD: &str = r#"(module
        (func (export "add") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.add))"#;

    const CALLS_NOTIFY_TWICE: &str = r#"(module
        (import "rusm" "notify" (func $notify))
        (func (export "run") (call $notify) (call $notify)))"#;

    const REPORTS_PID: &str = r#"(module
        (import "rusm" "self_pid" (func $pid (result i64)))
        (func (export "run") (result i64) (call $pid)))"#;

    const TRAPS: &str = r#"(module (func (export "run") unreachable))"#;

    const SPINS: &str = r#"(module (func (export "run") (loop (br 0))))"#;

    #[tokio::test]
    async fn instantiates_and_calls_an_export() {
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        let module = wr.compile(ADD).unwrap();
        let mut store = Store::new(&wr.engine, Host::default());
        store.set_epoch_deadline(1);
        store.epoch_deadline_async_yield_and_update(1);
        let instance = link_host(&wr.engine)
            .unwrap()
            .instantiate_async(&mut store, &module)
            .await
            .unwrap();
        let add = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "add")
            .unwrap();
        assert_eq!(add.call_async(&mut store, (2, 3)).await.unwrap(), 5);
    }

    #[tokio::test]
    async fn a_guest_can_call_a_host_function() {
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        let module = wr.compile(CALLS_NOTIFY_TWICE).unwrap();
        let mut store = Store::new(&wr.engine, Host::default());
        store.set_epoch_deadline(1);
        store.epoch_deadline_async_yield_and_update(1);
        let instance = link_host(&wr.engine)
            .unwrap()
            .instantiate_async(&mut store, &module)
            .await
            .unwrap();
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call_async(&mut store, ())
            .await
            .unwrap();
        assert_eq!(store.data().host_calls, 2);
    }

    #[tokio::test]
    async fn a_guest_reads_its_pid_via_a_host_function() {
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        let module = wr.compile(REPORTS_PID).unwrap();
        let mut store = Store::new(
            &wr.engine,
            Host {
                pid: 42,
                host_calls: 0,
            },
        );
        store.set_epoch_deadline(1);
        store.epoch_deadline_async_yield_and_update(1);
        let instance = link_host(&wr.engine)
            .unwrap()
            .instantiate_async(&mut store, &module)
            .await
            .unwrap();
        let run = instance
            .get_typed_func::<(), i64>(&mut store, "run")
            .unwrap();
        assert_eq!(run.call_async(&mut store, ()).await.unwrap(), 42);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_wasm_guest_runs_as_a_process_and_is_reaped() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let module = wr.compile(CALLS_NOTIFY_TWICE).unwrap();
        let handle = wr.spawn(module, "run");
        handle.join().await;
        assert_eq!(rt.finished(), 1);
        assert_eq!(rt.process_count(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_trapping_guest_crashes_the_process() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let module = wr.compile(TRAPS).unwrap();

        // Watch the guest process; its exit reason must be Crashed.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let watcher = rt
            .spawn(move |mut ctx| async move {
                let _ = tx.send(ctx.recv().await);
            })
            .pid();
        let guest = wr.spawn(module, "run");
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
    async fn an_infinite_loop_guest_yields_and_stays_killable() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let module = wr.compile(SPINS).unwrap();

        // A second, ordinary process must still make progress alongside the
        // spinner — proof the spinner yields rather than hogging a worker.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let bystander = rt.spawn(move |_| async move {
            let _ = tx.send(());
        });

        let spinner = wr.spawn(module, "run");
        let spinner_pid = spinner.pid();
        rx.await.unwrap(); // bystander ran despite the spinner
        bystander.join().await;

        assert!(rt.is_alive(spinner_pid));
        spinner.kill();
        spinner.join().await;
        assert!(!rt.is_alive(spinner_pid));
    }
}
