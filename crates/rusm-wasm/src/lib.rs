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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use rusm_otp::{ProcessHandle, Runtime};
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, Linker, Module, PoolingAllocationConfig,
};

mod actor;
mod bridges;
mod caps;

pub use bridges::http::{HttpServer, PreparedHttp};
pub use bridges::wasip1::PreparedModule;
pub use bridges::wasip2::PreparedComponent;
pub use caps::{Capabilities, CapabilityProfile};

/// How often the epoch is bumped. A guest runs at most this long before it must
/// yield to the scheduler, so tight loops can't starve other processes.
const EPOCH_TICK: Duration = Duration::from_millis(10);
/// Default pool slots: the most Wasm instances that may be live at once. The
/// pooling allocator pre-reserves their memory slabs so a spawn is a slab reuse,
/// not an mmap — the lever that makes instance-per-process cheap. The reservation
/// (`max_instances` × `max_memory`) is **lazy, copy-on-write virtual address
/// space** (e.g. 1024 × 16 MiB = 16 GiB virtual), so real RSS scales only with
/// *live* instances. A busy node raises this via [`WasmRuntime::with_limits`].
/// (A true "millions of Wasm processes" tier needs an on-demand fallback above
/// the pool — see the roadmap.)
pub const DEFAULT_MAX_INSTANCES: u32 = 1024;
/// Default per-instance linear-memory ceiling (virtual, copy-on-write). Sized for
/// real components (a minimal Rust component needs ~1 MiB; the rquickjs js-runner
/// a few); a per-process capability `StoreLimiter` caps usage *below* this.
pub const DEFAULT_MAX_MEMORY: usize = 16 << 20;
/// The prebuilt rquickjs **js-runner** component (rusm-ts), embedded so a TS/JS
/// bundle can be spawned with no extra artifacts. Built from `js-runner/` with
/// wasi-sdk (QuickJS is C); see that crate's README to regenerate.
const JS_RUNNER_WASM: &[u8] = include_bytes!("../js-runner/js_runner.wasm");

/// Counters shared by every instance of one [`WasmRuntime`], so host functions
/// can report aggregate activity (e.g. guest progress for the fairness scenario).
#[derive(Default)]
pub(crate) struct Counters {
    /// Total `notify` host-function calls across all guests.
    pub(crate) notifications: AtomicU64,
}

/// The spawn core shared between the [`WasmRuntime`] and every **running guest** —
/// held behind an `Arc` so a component's host context can spawn siblings without a
/// back-reference to the whole runtime. It carries exactly what a spawn needs (the
/// engine, the process runtime, and a name → prepared-component registry) and
/// nothing the prepare-time linkers own, keeping the per-spawn path lean.
/// A component registered for spawn-by-name: the prepared component plus an
/// optional first message to deliver on spawn. For a **TS service** the prepared
/// component is the shared js-runner and `bundle` is the JS source (replayed as
/// message 1, the runner's protocol); for a Rust component `bundle` is `None`.
#[derive(Clone)]
pub(crate) struct Registered {
    pub(crate) prepared: PreparedComponent,
    /// `Arc` so a lookup clone is cheap; the bytes copy once, on the actual send.
    pub(crate) bundle: Option<Arc<Vec<u8>>>,
}

pub(crate) struct Spawner {
    pub(crate) engine: Engine,
    pub(crate) rt: Runtime,
    /// Components registered by name so a guest may `spawn` them. Read-mostly
    /// (written when an app loads, read per guest-initiated spawn), so a plain
    /// `RwLock` — uncontended reads, no extra dependency.
    components: RwLock<HashMap<String, Registered>>,
    /// The **on-demand overflow engine** (opt-in — `None` by default). When the
    /// pooled tier is full, a spawn instantiates here instead, so the live count is
    /// bounded by memory rather than the fixed pool size. See
    /// [`WasmRuntime::with_overflow`].
    pub(crate) overflow: Option<Engine>,
    /// Live instances occupying the pooled tier (only tracked when `overflow` is
    /// set; the default path adds no atomic). A spawn reserves a slot here; if the
    /// pool is full it falls to the overflow engine.
    pub(crate) pooled_live: AtomicU32,
    /// The pooled-tier capacity (`pooled_live`'s ceiling).
    pub(crate) pooled_cap: u32,
}

impl Spawner {
    /// Registers a component under `name` so guests may `spawn` it by that name.
    pub(crate) fn register(&self, name: impl Into<String>, entry: Registered) {
        self.components
            .write()
            .expect("component registry is never poisoned")
            .insert(name.into(), entry);
    }

    /// Looks up a registered component by name (cloned out so no lock is held
    /// across the spawn — both fields are `Arc`-backed, so this is cheap).
    pub(crate) fn lookup(&self, name: &str) -> Option<Registered> {
        self.components
            .read()
            .expect("component registry is never poisoned")
            .get(name)
            .cloned()
    }
}

/// Runs Wasm guests as RUSM processes.
pub struct WasmRuntime {
    /// The spawn core (engine + process runtime + component registry), shared with
    /// running guests so they can spawn by name. The runtime reads the engine and
    /// process runtime through it rather than duplicating those handles.
    spawner: Arc<Spawner>,
    /// The core-module linker (preview1 WASI + the raw `rusm::*` actor ABI). Built
    /// once; a module's imports resolve into a `PreparedModule` at `prepare`, never
    /// per spawn. The component counterpart is `component_linker`.
    linker: Linker<bridges::wasip1::CoreHost>,
    /// The component-model counterpart of `linker`, with WASI wired in. Used by
    /// the wasip2/p3 bridges to prepare and spawn components. Built once.
    component_linker: wasmtime::component::Linker<bridges::WasiHost>,
    /// The component linker bound to the **overflow** engine (`None` unless built
    /// with [`with_overflow`](Self::with_overflow)); used to prepare a component's
    /// overflow `InstancePre` alongside its pooled one.
    pub(crate) overflow_component_linker: Option<wasmtime::component::Linker<bridges::WasiHost>>,
    /// The prebuilt rquickjs **js-runner** component (for `spawn_js`), compiled +
    /// prepared lazily on first use so non-JS nodes pay nothing.
    js_runner: std::sync::OnceLock<PreparedComponent>,
    shared: Arc<Counters>,
    epoch_stop: Arc<AtomicBool>,
    epoch_ticker: Option<JoinHandle<()>>,
}

impl WasmRuntime {
    /// Builds a backend over an existing process [`Runtime`], with the default pool
    /// limits ([`DEFAULT_MAX_INSTANCES`] live instances × [`DEFAULT_MAX_MEMORY`] each).
    /// Must run inside a Tokio runtime (it starts the epoch ticker).
    pub fn new(rt: Runtime) -> Result<Self> {
        Self::with_limits(rt, DEFAULT_MAX_INSTANCES, DEFAULT_MAX_MEMORY)
    }

    /// Like [`new`](Self::new) but with explicit pool limits — raise
    /// `max_instances` for a node that hosts many concurrent Wasm processes (the
    /// reservation is lazy virtual memory; real RSS tracks live instances), and
    /// `max_memory` for components that need larger heaps.
    pub fn with_limits(rt: Runtime, max_instances: u32, max_memory: usize) -> Result<Self> {
        let engine = Engine::new(&Self::pooled_config(max_instances, max_memory))?;
        Self::assemble(rt, engine, None, max_instances)
    }

    /// Like [`with_limits`](Self::with_limits) but adds an **on-demand overflow
    /// tier**: once the pooled `max_instances` are all live, further spawns
    /// instantiate on a second (on-demand) engine instead of failing — so the live
    /// Wasm-process count is bounded by available memory, not the fixed pool size.
    /// The pooled tier stays the fast path; overflow only engages past capacity.
    pub fn with_overflow(rt: Runtime, max_instances: u32, max_memory: usize) -> Result<Self> {
        let engine = Engine::new(&Self::pooled_config(max_instances, max_memory))?;
        // Same compile config, but the default (on-demand) allocator: no fixed cap.
        let overflow = Engine::new(&Self::base_config())?;
        Self::assemble(rt, engine, Some(overflow), max_instances)
    }

    /// The compile/runtime config shared by both engines: epoch interruption (the
    /// preemption lever), the async component model (WASI p3), and copy-on-write
    /// memory init.
    fn base_config() -> Config {
        let mut config = Config::new();
        config.epoch_interruption(true);
        config.wasm_component_model_async(true);
        config.memory_init_cow(true);
        config
    }

    /// [`base_config`](Self::base_config) plus the pooling allocator — pre-reserved
    /// instance slabs reused per spawn (the instance-per-process efficiency win).
    fn pooled_config(max_instances: u32, max_memory: usize) -> Config {
        let mut config = Self::base_config();
        let mut pool = PoolingAllocationConfig::default();
        pool.total_core_instances(max_instances);
        pool.total_memories(max_instances);
        pool.total_tables(max_instances);
        pool.max_memory_size(max_memory);
        // Component guests also draw a *component-instance* slot from the pool.
        pool.total_component_instances(max_instances);
        config.allocation_strategy(InstanceAllocationStrategy::Pooling(pool));
        config
    }

    fn assemble(
        rt: Runtime,
        engine: Engine,
        overflow: Option<Engine>,
        max_instances: u32,
    ) -> Result<Self> {
        let linker = bridges::wasip1::build_linker(&engine)?;
        let component_linker = bridges::wasip2::build_linker(&engine)?;
        let overflow_component_linker = overflow
            .as_ref()
            .map(bridges::wasip2::build_linker)
            .transpose()?;

        // Bump the epoch on a cadence — on a **dedicated OS thread**, not a Tokio
        // task. The whole point is to preempt guests that are pinning the Tokio
        // workers; a ticker that needed a worker itself would starve exactly when
        // it's needed (and deadlock once every worker runs a tight-loop guest).
        // Every engine (pooled + overflow) is ticked, so overflow guests are
        // preempted too.
        let mut ticker_engines = vec![engine.clone()];
        ticker_engines.extend(overflow.clone());
        let epoch_stop = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&epoch_stop);
        let epoch_ticker = std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(EPOCH_TICK);
                for engine in &ticker_engines {
                    engine.increment_epoch();
                }
            }
        });

        Ok(Self {
            spawner: Arc::new(Spawner {
                engine,
                rt,
                components: RwLock::new(HashMap::new()),
                overflow,
                pooled_live: AtomicU32::new(0),
                pooled_cap: max_instances,
            }),
            linker,
            component_linker,
            overflow_component_linker,
            js_runner: std::sync::OnceLock::new(),
            shared: Arc::new(Counters::default()),
            epoch_stop,
            epoch_ticker: Some(epoch_ticker),
        })
    }

    /// Registers a prepared component under `name` so a **running guest** may
    /// `spawn` it by that name through the actor ABI (capability-gated). The app
    /// loader registers each manifest component so siblings can spawn one another.
    pub fn register_component(&self, name: impl Into<String>, prepared: PreparedComponent) {
        self.spawner.register(
            name,
            Registered {
                prepared,
                bundle: None,
            },
        );
    }

    /// Registers a **TypeScript service** under `name`: a guest `spawn`ing it gets a
    /// fresh js-runner instance fed this Bun-built bundle (the runner's protocol).
    /// Lets a TS commander reach a TS service via the concealed typed client.
    pub fn register_js_component(&self, name: impl Into<String>, bundle: impl Into<Vec<u8>>) {
        let prepared = self.js_runner().clone();
        self.spawner.register(
            name,
            Registered {
                prepared,
                bundle: Some(Arc::new(bundle.into())),
            },
        );
    }

    /// Spawns a **JavaScript/TypeScript** bundle as a sandboxed process via the
    /// embedded rquickjs js-runner (default-deny `Sandboxed`). The bundle is the
    /// Bun-built JS source; it runs with the `Process` actor API + Web API
    /// polyfills. Use [`spawn_js_with`](Self::spawn_js_with) to grant capabilities.
    pub fn spawn_js(&self, bundle: impl Into<Vec<u8>>) -> ProcessHandle {
        self.spawn_js_with(bundle, CapabilityProfile::Sandboxed.capabilities())
    }

    /// Like [`spawn_js`](Self::spawn_js) but under explicit [`Capabilities`]. The
    /// js-runner component is compiled + prepared once (lazily) and shared across
    /// all JS processes; each spawn is a fresh, isolated instance fed the bundle.
    pub fn spawn_js_with(&self, bundle: impl Into<Vec<u8>>, caps: Capabilities) -> ProcessHandle {
        let handle = self.spawn_component_with(self.js_runner(), caps);
        // The runner's protocol: its first message is the JS bundle to execute.
        self.spawner.rt.send(handle.pid(), bundle.into());
        handle
    }

    /// The shared, embedded rquickjs js-runner — compiled + prepared once (lazily)
    /// so non-JS nodes pay nothing. Backs `spawn_js` and TS-service registration.
    fn js_runner(&self) -> &PreparedComponent {
        self.js_runner.get_or_init(|| {
            // The runner is a known-good embedded artifact; a failure here is a
            // build bug, not a runtime condition.
            self.prepare_component(
                &self
                    .compile_component(JS_RUNNER_WASM)
                    .expect("embedded js-runner compiles"),
                "run",
            )
            .expect("embedded js-runner prepares")
        })
    }

    /// Compiles a module from Wasm bytes or `.wat` text.
    pub fn compile(&self, wasm: impl AsRef<[u8]>) -> Result<Module> {
        Ok(Module::new(&self.spawner.engine, wasm)?)
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
