use std::sync::Arc;
use std::time::Instant;

use futures_util::stream::{FuturesUnordered, StreamExt};
use rusm_otp::Runtime;
use rusm_wasm::{PreparedComponent, WasmRuntime};
use tokio::task::JoinHandle;

use crate::sample::Sample;

/// Latency samples taken per tick (fresh, timed component spawns).
const LATENCY_SAMPLE: usize = 64;
/// Target total live component instances across all spawner workers — kept
/// comfortably below the `rusm-wasm` pooling allocator's slot count so a spawn
/// never exhausts the pool (which would crash that instance and inflate the
/// rate). Bounds the population so we measure *sustainable* instantiate-and-reap
/// throughput, and gives each worker its share of in-flight slots.
const MAX_LIVE: usize = 100;

/// A minimal but representative component: one page of linear memory (so each
/// spawn exercises the pooling allocator's memory slab, not a no-memory toy) and
/// a `run` export that returns immediately.
const COMPONENT: &str = r#"(component
    (core module $m (memory (export "mem") 1) (func (export "run")))
    (core instance $i (instantiate $m))
    (func (export "run") (canon lift (core func $i "run"))))"#;

/// A **real, continuous, multi-core component spawn storm**: background workers
/// instantiate WASM **components** as fast as the runtime allows, each as its own
/// isolated `rusm-otp` process. [`tick`](Self::tick) reports the achieved rate
/// (Δspawned / Δt) — the headline "components hosted per second" — plus a few
/// timed spawns for latency. This is the lever set proven live: pooling allocator,
/// copy-on-write, `InstancePre`, precomputed export index. Must be constructed
/// inside a Tokio runtime (it starts the Wasm epoch ticker).
pub struct ComponentStormEngine {
    runtime: Runtime,
    // Shared across spawner workers and `tick`; owns the engine + epoch ticker.
    wasm: Arc<WasmRuntime>,
    prepared: PreparedComponent,
    workers: Vec<JoinHandle<()>>,
    last_spawned: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl ComponentStormEngine {
    pub fn new(workers: usize, scheduler_count: usize) -> Self {
        let runtime = Runtime::new();
        let wasm = Arc::new(WasmRuntime::new(runtime.clone()).expect("wasm engine"));
        let prepared = wasm
            .prepare_component(&wasm.compile_component(COMPONENT).expect("compile"), "run")
            .expect("prepare");

        // Each worker keeps a bounded set of in-flight components and **parks** on
        // their completion (no busy-yield spin, no global counter polling) — the
        // backpressure is the await itself, so workers sleep instead of burning a
        // scheduler slot when at capacity.
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
                            let handle = wasm.spawn_component(&prepared);
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
        self.last_spawned = spawned;
        self.last_at = now;

        let latencies_ns = (0..LATENCY_SAMPLE)
            .map(|_| {
                let started = Instant::now();
                let _ = self.wasm.spawn_component(&self.prepared);
                started.elapsed().as_nanos() as u64
            })
            .collect();

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

impl Drop for ComponentStormEngine {
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
    async fn component_storm_spawns_continuously() {
        let mut engine = ComponentStormEngine::new(4, 4);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let sample = engine.tick();
        assert!(sample.ops_per_sec > 0.0, "components should be spawning");
        assert_eq!(sample.latencies_ns.len(), LATENCY_SAMPLE);
        assert_eq!(sample.scheduler_load.len(), 4);
        assert!(engine.runtime.spawned() > 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_population_stays_bounded() {
        let mut engine = ComponentStormEngine::new(4, 2);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Well below the pooling allocator's slot count — backpressure works.
        assert!(engine.tick().process_count < 256);
    }
}
