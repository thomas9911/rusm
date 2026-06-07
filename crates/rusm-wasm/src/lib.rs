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
use rusm_otp::Runtime;
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, Linker, Module, PoolingAllocationConfig,
};

mod actor;
mod bridges;
mod caps;

pub use bridges::wasip1::PreparedModule;
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

/// Runs Wasm guests as RUSM processes.
pub struct WasmRuntime {
    engine: Engine,
    rt: Runtime,
    /// The core-module linker (preview1 WASI + the raw `rusm::*` actor ABI). Built
    /// once; a module's imports resolve into a `PreparedModule` at `prepare`, never
    /// per spawn. The component counterpart is `component_linker`.
    linker: Linker<bridges::wasip1::CoreHost>,
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
        let linker = bridges::wasip1::build_linker(&engine)?;
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
}

impl Drop for WasmRuntime {
    fn drop(&mut self) {
        self.epoch_stop.store(true, Ordering::Relaxed);
        if let Some(ticker) = self.epoch_ticker.take() {
            let _ = ticker.join();
        }
    }
}
