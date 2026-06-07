use std::time::Instant;

use rusm_otp::Runtime;
use tokio::task::JoinHandle;

use crate::sample::Sample;

/// Latency samples taken per tick (fresh, timed spawns).
const LATENCY_SAMPLE: usize = 64;
/// Background spawners yield this often so the trivial processes they create get
/// scheduled and reaped, and so the in-flight cap is re-checked.
const YIELD_EVERY: u32 = 256;

/// A **real, continuous, multi-core** spawn storm: one background spawner task
/// per core hammers `rusm-otp` as fast as possible; [`tick`](Self::tick) samples
/// the achieved rate (Δspawned / Δt) plus a few timed spawns for latency.
///
/// A sequential per-tick loop is capped by one core; a real storm uses them all.
/// `max_in_flight` is backpressure: spawners pause until the live population
/// drains below it, so memory stays bounded and we measure *sustainable*
/// create-and-reap throughput, not an unbounded pile-up. `total_memory_bytes` is
/// 0 — native processes have no per-instance footprint until the Wasm backend
/// (Phase 6). Must be constructed inside a Tokio runtime.
pub struct SpawnStormEngine {
    runtime: Runtime,
    workers: Vec<JoinHandle<()>>,
    last_spawned: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl SpawnStormEngine {
    pub fn new(workers: usize, scheduler_count: usize, max_in_flight: usize) -> Self {
        let runtime = Runtime::new();
        let workers = (0..workers.max(1))
            .map(|_| {
                let rt = runtime.clone();
                tokio::spawn(async move {
                    let mut n: u32 = 0;
                    loop {
                        let _ = rt.spawn(|_| async {});
                        n = n.wrapping_add(1);
                        if n % YIELD_EVERY == 0 {
                            tokio::task::yield_now().await;
                            // Backpressure: let the live population drain so memory
                            // stays bounded (sustainable create-and-reap).
                            while rt.process_count() > max_in_flight {
                                tokio::task::yield_now().await;
                            }
                        }
                    }
                })
            })
            .collect();
        Self {
            runtime,
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
                let _ = self.runtime.spawn(|_| async {});
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

impl Drop for SpawnStormEngine {
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
    async fn storm_spawns_continuously_across_workers() {
        let mut engine = SpawnStormEngine::new(4, 4, 50_000);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let sample = engine.tick();
        assert!(sample.ops_per_sec > 0.0);
        assert_eq!(sample.latencies_ns.len(), LATENCY_SAMPLE);
        assert_eq!(sample.scheduler_load.len(), 4);
        assert_eq!(sample.total_memory_bytes, 0);
        assert!(engine.runtime.spawned() > 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn backpressure_keeps_the_population_bounded() {
        // A tiny cap forces the drain loop; the live count must stay near it,
        // not run away into the millions.
        let mut engine = SpawnStormEngine::new(4, 2, 200);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(engine.tick().process_count < 20_000);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_count_is_at_least_one() {
        let engine = SpawnStormEngine::new(0, 1, 50_000);
        assert_eq!(engine.workers.len(), 1);
    }
}
