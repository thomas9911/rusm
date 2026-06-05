use std::time::Instant;

use rusm_otp::Runtime;

use crate::sample::Sample;

/// A **real** spawn-storm: spawns a batch of `rusm-otp` processes per tick and
/// measures the achieved spawn rate and per-spawn latency.
///
/// Unlike the synthetic source, the numbers here are measured — `ops_per_sec` is
/// real spawn throughput and `latencies_ns` are real per-spawn timings. Native
/// processes have no per-instance linear memory, so `total_memory_bytes` is 0
/// until the Wasm backend (Phase 6) gives processes a measurable footprint.
///
/// `tick` must be called from within a Tokio runtime (it uses `tokio::spawn`).
pub struct SpawnStormEngine {
    runtime: Runtime,
    batch: usize,
    scheduler_count: usize,
}

impl SpawnStormEngine {
    pub fn new(batch: usize, scheduler_count: usize) -> Self {
        Self {
            runtime: Runtime::new(),
            batch: batch.max(1),
            scheduler_count,
        }
    }

    pub fn tick(&mut self) -> Sample {
        let start = Instant::now();
        let mut latencies_ns = Vec::with_capacity(self.batch);
        for _ in 0..self.batch {
            let spawned_at = Instant::now();
            // A trivial process that completes immediately; the handle is dropped
            // (detached) — spawn-storm measures creation throughput, not lifetime.
            let _ = self.runtime.spawn(|_| async {});
            latencies_ns.push(spawned_at.elapsed().as_nanos() as u64);
        }
        let elapsed = start.elapsed().as_secs_f64().max(f64::MIN_POSITIVE);
        let ops_per_sec = self.batch as f64 / elapsed;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tick_spawns_a_real_batch_and_measures_it() {
        let mut engine = SpawnStormEngine::new(64, 4);
        let sample = engine.tick();
        // Let the runtime drive the spawned (trivial) processes to completion.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        assert_eq!(sample.latencies_ns.len(), 64);
        assert!(sample.ops_per_sec > 0.0);
        assert_eq!(sample.scheduler_load.len(), 4);
        // Native processes report no per-instance memory yet.
        assert_eq!(sample.total_memory_bytes, 0);
    }

    #[tokio::test]
    async fn batch_is_at_least_one() {
        let mut engine = SpawnStormEngine::new(0, 1);
        assert_eq!(engine.tick().latencies_ns.len(), 1);
    }

    #[tokio::test]
    async fn spawn_count_accumulates_across_ticks() {
        let mut engine = SpawnStormEngine::new(10, 2);
        engine.tick();
        engine.tick();
        assert_eq!(engine.runtime.spawned(), 20);
    }
}
