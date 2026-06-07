use std::sync::Arc;
use std::time::Instant;

use futures_util::stream::{FuturesUnordered, StreamExt};
use rusm_otp::Runtime;
use rusm_wasm::{PreparedModule, WasmRuntime};
use tokio::task::JoinHandle;

use crate::sample::Sample;

/// Latency samples taken per tick (fresh, timed core-module spawns).
const LATENCY_SAMPLE: usize = 64;
/// Target total live instances across all workers — kept below the pooling
/// allocator's slot count so a spawn never exhausts the pool. Mirrors the
/// component storm's bound (see `componentstorm.rs`).
const MAX_LIVE: usize = 100;

/// A minimal but representative **wasip1 core module**: one page of linear memory
/// (so each spawn exercises the pooling allocator's memory slab, not a no-memory
/// toy) and a `run` export that returns immediately.
const MODULE: &str = r#"(module
    (memory (export "memory") 1)
    (func (export "run")))"#;

/// A **real, continuous, multi-core wasip1 core-module spawn storm** — the direct
/// head-to-head with Lunatic, which hosts only wasip1 core modules. Each spawn is a
/// fresh, isolated `rusm-otp` process running a core module; [`tick`](Self::tick)
/// reports the achieved rate (Δspawned / Δt) — "core modules hosted per second" —
/// plus a few timed spawns for latency. Same lever set as the component storm:
/// pooling allocator, copy-on-write, `InstancePre`, precomputed export index. Must
/// be constructed inside a Tokio runtime (it starts the Wasm epoch ticker).
pub struct ModuleStormEngine {
    runtime: Runtime,
    // Shared across spawner workers and `tick`; owns the engine + epoch ticker.
    wasm: Arc<WasmRuntime>,
    prepared: PreparedModule,
    workers: Vec<JoinHandle<()>>,
    last_spawned: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl ModuleStormEngine {
    pub fn new(workers: usize, scheduler_count: usize) -> Self {
        let runtime = Runtime::new();
        let wasm = Arc::new(WasmRuntime::new(runtime.clone()).expect("wasm engine"));
        let prepared = wasm
            .prepare(&wasm.compile(MODULE).expect("compile"), "run")
            .expect("prepare");

        // Each worker keeps a bounded set of in-flight modules and **parks** on
        // their completion (no busy-yield, no global counter polling) — the
        // backpressure is the await itself.
        let worker_count = workers.max(1);
        let per_worker = (MAX_LIVE / worker_count).max(1);
        let workers = (0..worker_count)
            .map(|_| {
                let wasm = Arc::clone(&wasm);
                let prepared = prepared.clone();
                tokio::spawn(async move {
                    let mut inflight = FuturesUnordered::new();
                    loop {
                        while inflight.len() < per_worker {
                            let handle = wasm.spawn(&prepared);
                            inflight.push(async move { handle.join().await });
                        }
                        inflight.next().await; // park until one finishes, then refill
                    }
                })
            })
            .collect();

        Self {
            runtime,
            wasm,
            prepared,
            workers,
            last_spawned: 0,
            last_at: Instant::now(),
            scheduler_count,
        }
    }

    pub fn tick(&mut self) -> Sample {
        let now = Instant::now();
        let spawned = self.runtime.spawned();
        let dt = now
            .duration_since(self.last_at)
            .as_secs_f64()
            .max(f64::MIN_POSITIVE);
        let ops_per_sec = spawned.saturating_sub(self.last_spawned) as f64 / dt;
        self.last_at = now;

        let latencies_ns = (0..LATENCY_SAMPLE)
            .map(|_| {
                let started = Instant::now();
                let _ = self.wasm.spawn(&self.prepared);
                started.elapsed().as_nanos() as u64
            })
            .collect();

        // Baseline from the counter *after* sampling so this tick's synthetic
        // latency-sample spawns never inflate the next tick's measured rate.
        self.last_spawned = self.runtime.spawned();
        let process_count = self.runtime.process_count() as u64;
        Sample {
            ops_per_sec,
            process_count,
            running: process_count,
            waiting: 0,
            total_memory_bytes: 0,
            latencies_ns,
            processes: Vec::new(),
            scheduler_load: vec![0.0; self.scheduler_count],
        }
    }
}

impl Drop for ModuleStormEngine {
    fn drop(&mut self) {
        for worker in &self.workers {
            worker.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn module_storm_spawns_continuously() {
        let mut engine = ModuleStormEngine::new(4, 4);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let sample = engine.tick();
        assert!(sample.ops_per_sec > 0.0, "core modules should be spawning");
        assert_eq!(sample.latencies_ns.len(), LATENCY_SAMPLE);
        assert_eq!(sample.scheduler_load.len(), 4);
        assert!(engine.runtime.spawned() > 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_population_stays_bounded() {
        let mut engine = ModuleStormEngine::new(4, 2);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Well below the pooling allocator's slot count — backpressure works.
        assert!(engine.tick().process_count < 256);
    }
}
