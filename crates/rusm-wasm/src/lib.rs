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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use rusm_otp::{ExitReason, Pid, ProcessHandle, Runtime};
use wasmtime::{
    Caller, Config, Engine, InstanceAllocationStrategy, InstancePre, Linker, Module,
    PoolingAllocationConfig, Store,
};

mod actor;
mod bridges;
mod caps;

pub use bridges::wasip2::PreparedComponent;
pub use caps::{Capabilities, CapabilityProfile};

/// How often the epoch is bumped. A guest runs at most this long before it must
/// yield to the scheduler, so tight loops can't starve other processes.
const EPOCH_TICK: Duration = Duration::from_millis(10);
/// Pool slots: the most Wasm instances that may be live at once. The pooling
/// allocator pre-reserves their memory slabs so a spawn is a slab reuse, not an
/// mmap — the lever that makes instance-per-process cheap. The virtual reservation
/// is `MAX_INSTANCES` × `MAX_MEMORY` (lazy, copy-on-write), so the two trade off:
/// real components need MiBs of linear memory, so we keep the slot count modest.
/// A busy node tunes these up (made configurable when the benchmark lands).
const MAX_INSTANCES: u32 = 256;
/// Per-instance linear-memory ceiling (virtual, copy-on-write). Sized for real
/// components (a minimal Rust component already needs ~1 MiB); a per-process
/// capability `StoreLimiter` caps usage *below* this.
const MAX_MEMORY: usize = 16 << 20;

/// Counters shared by every instance of one [`WasmRuntime`], so host functions
/// can report aggregate activity (e.g. guest progress for the fairness scenario).
#[derive(Default)]
pub(crate) struct Counters {
    /// Total `notify` host-function calls across all guests.
    pub(crate) notifications: AtomicU64,
}

/// Per-instance host state reachable from host functions via `Caller::data`.
/// Opaque to callers — they only ever hold it inside an [`InstancePre`].
pub struct Host {
    /// The owning process's pid (0 for a bare instance with no process).
    pid: u64,
    /// Shared across the runtime's instances.
    shared: Arc<Counters>,
}

/// Runs Wasm guests as RUSM processes.
pub struct WasmRuntime {
    engine: Engine,
    rt: Runtime,
    /// Built once; host imports are resolved per *module* (into an [`InstancePre`]
    /// by [`prepare`](WasmRuntime::prepare)), never per spawn.
    linker: Linker<Host>,
    /// The component-model counterpart of `linker`, with WASI wired in. Used by
    /// the wasip2/p3 bridges to prepare and spawn components. Built once.
    component_linker: wasmtime::component::Linker<bridges::WasiHost>,
    shared: Arc<Counters>,
    epoch_stop: Arc<AtomicBool>,
    epoch_ticker: Option<JoinHandle<()>>,
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
        // Copy-on-write memory init (default, set explicit): a fresh instance
        // shares the module image until it writes — near-zero init cost.
        config.memory_init_cow(true);
        // Pooling allocator: reuse pre-reserved instance slabs instead of an mmap
        // per spawn. This is the instance-per-process efficiency win over a
        // naive on-demand allocator.
        let mut pool = PoolingAllocationConfig::default();
        pool.total_core_instances(MAX_INSTANCES);
        pool.total_memories(MAX_INSTANCES);
        pool.total_tables(MAX_INSTANCES);
        pool.max_memory_size(MAX_MEMORY);
        // Component guests also draw a *component-instance* slot from the pool (on
        // top of the core-instance/memory slots above); without this a component
        // spawn can't use the pooling allocator.
        pool.total_component_instances(MAX_INSTANCES);
        config.allocation_strategy(InstanceAllocationStrategy::Pooling(pool));
        let engine = Engine::new(&config)?;
        let linker = link_host(&engine)?;
        let component_linker = bridges::wasip2::build_linker(&engine)?;

        // Bump the epoch on a cadence — on a **dedicated OS thread**, not a Tokio
        // task. The whole point is to preempt guests that are pinning the Tokio
        // workers; a ticker that needed a worker itself would starve exactly when
        // it's needed (and deadlock once every worker runs a tight-loop guest).
        let ticker_engine = engine.clone();
        let epoch_stop = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&epoch_stop);
        let epoch_ticker = std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(EPOCH_TICK);
                ticker_engine.increment_epoch();
            }
        });

        Ok(Self {
            engine,
            rt,
            linker,
            component_linker,
            shared: Arc::new(Counters::default()),
            epoch_stop,
            epoch_ticker: Some(epoch_ticker),
        })
    }

    /// Compiles a module from Wasm bytes or `.wat` text.
    pub fn compile(&self, wasm: impl AsRef<[u8]>) -> Result<Module> {
        Ok(Module::new(&self.engine, wasm)?)
    }

    /// Total `notify` calls made by all guests so far — guest-reported progress.
    pub fn notifications(&self) -> u64 {
        self.shared.notifications.load(Ordering::Relaxed)
    }

    /// Resolves a module's host imports **once**, yielding a reusable
    /// [`InstancePre`]. Spawning from it skips import resolution entirely — the
    /// fast path for spawning the same module many times.
    pub fn prepare(&self, module: &Module) -> Result<InstancePre<Host>> {
        Ok(self.linker.instantiate_pre(module)?)
    }

    /// Spawns a prepared module as an isolated process running its `entry` export
    /// (a `() -> ()` function). A fresh instance is created per process; a trap
    /// exits the process with [`ExitReason::Crashed`].
    pub fn spawn(&self, prepared: &InstancePre<Host>, entry: impl Into<String>) -> ProcessHandle {
        let engine = self.engine.clone();
        let rt = self.rt.clone();
        let prepared = prepared.clone();
        let shared = Arc::clone(&self.shared);
        let entry = entry.into();
        self.rt.spawn(move |ctx| async move {
            let pid = ctx.pid();
            // The mailbox (in `ctx`) is unused until the messaging host ABI lands;
            // dropping it leaves the process alive, addressable via its abort handle.
            if run(&engine, &prepared, pid, shared, &entry).await.is_err() {
                rt.exit(pid, ExitReason::Crashed);
            }
        })
    }
}

impl Drop for WasmRuntime {
    fn drop(&mut self) {
        self.epoch_stop.store(true, Ordering::Relaxed);
        if let Some(ticker) = self.epoch_ticker.take() {
            let _ = ticker.join();
        }
    }
}

/// Host functions the guest imports under the `rusm` namespace.
fn link_host(engine: &Engine) -> Result<Linker<Host>> {
    let mut linker = Linker::new(engine);
    linker.func_wrap("rusm", "self_pid", |caller: Caller<'_, Host>| {
        caller.data().pid as i64
    })?;
    linker.func_wrap("rusm", "notify", |caller: Caller<'_, Host>| {
        caller
            .data()
            .shared
            .notifications
            .fetch_add(1, Ordering::Relaxed);
    })?;
    Ok(linker)
}

/// Instantiates a prepared module in a fresh store and runs its `entry` export,
/// yielding to the scheduler whenever the epoch deadline is reached.
async fn run(
    engine: &Engine,
    prepared: &InstancePre<Host>,
    pid: Pid,
    shared: Arc<Counters>,
    entry: &str,
) -> Result<()> {
    let host = Host {
        pid: pid.raw(),
        shared,
    };
    let mut store = Store::new(engine, host);
    // Yield to Tokio on each epoch tick, then run for one more tick.
    store.set_epoch_deadline(1);
    store.epoch_deadline_async_yield_and_update(1);

    let instance = prepared.instantiate_async(&mut store).await?;
    let func = instance.get_typed_func::<(), ()>(&mut store, entry)?;
    func.call_async(&mut store, ()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare store that shares the runtime's counters, with the epoch deadline
    /// set — for tests that instantiate directly instead of via `spawn`.
    fn bare_store(wr: &WasmRuntime, pid: u64) -> Store<Host> {
        let mut store = Store::new(
            &wr.engine,
            Host {
                pid,
                shared: Arc::clone(&wr.shared),
            },
        );
        store.set_epoch_deadline(1);
        store.epoch_deadline_async_yield_and_update(1);
        store
    }

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

    // Each run reports completion via `notify`, so the bench can assert every
    // instance truly ran (no silent pool-exhaustion crashes inflating the rate).
    const NOOP: &str = r#"(module
        (import "rusm" "notify" (func $notify))
        (memory 1)
        (func (export "run") (call $notify)))"#;

    #[ignore]
    #[tokio::test(flavor = "multi_thread")]
    async fn wasm_spawn_rate() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let module = wr.compile(NOOP).unwrap();
        let n = 50_000usize;
        let start = std::time::Instant::now();
        let pre = wr.prepare(&module).unwrap();
        let handles: Vec<_> = (0..n).map(|_| wr.spawn(&pre, "run")).collect();
        for h in handles {
            h.join().await;
        }
        let elapsed = start.elapsed();
        println!(
            "wasm instance-per-process: {n} in {elapsed:?} = {:.0}/s",
            n as f64 / elapsed.as_secs_f64()
        );
        assert_eq!(rt.finished(), n as u64); // every instance was reaped
        assert_eq!(wr.notifications(), n as u64); // ...and actually ran (no crashes)
    }

    #[tokio::test]
    async fn instantiates_and_calls_an_export() {
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        let module = wr.compile(ADD).unwrap();
        let mut store = bare_store(&wr, 0);
        let instance = wr
            .prepare(&module)
            .unwrap()
            .instantiate_async(&mut store)
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
        let mut store = bare_store(&wr, 0);
        let instance = wr
            .prepare(&module)
            .unwrap()
            .instantiate_async(&mut store)
            .await
            .unwrap();
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call_async(&mut store, ())
            .await
            .unwrap();
        assert_eq!(wr.notifications(), 2);
    }

    #[tokio::test]
    async fn a_guest_reads_its_pid_via_a_host_function() {
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        let module = wr.compile(REPORTS_PID).unwrap();
        let mut store = bare_store(&wr, 42);
        let instance = wr
            .prepare(&module)
            .unwrap()
            .instantiate_async(&mut store)
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
        let pre = wr.prepare(&module).unwrap();
        let handle = wr.spawn(&pre, "run");
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
        let pre = wr.prepare(&module).unwrap();
        let guest = wr.spawn(&pre, "run");
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

        let pre = wr.prepare(&module).unwrap();
        let spinner = wr.spawn(&pre, "run");
        let spinner_pid = spinner.pid();
        rx.await.unwrap(); // bystander ran despite the spinner
        bystander.join().await;

        assert!(rt.is_alive(spinner_pid));
        spinner.kill();
        spinner.join().await;
        assert!(!rt.is_alive(spinner_pid));
    }
}
