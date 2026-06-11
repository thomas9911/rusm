use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rusm_kv::Store;
use rusm_otp::{ProcessHandle, Runtime};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use crate::sample::Sample;

/// Most latency samples surfaced in a single tick.
const LATENCY_SAMPLE: usize = 64;
/// The bucket all workers share — each worker owns a distinct key inside it, so
/// there is no logical write conflict, only redb's storage-level commit ordering.
const BUCKET: &str = "bench";

/// A **real, continuous durable-KV storm** over [`rusm_kv`]: `workers` processes each
/// drive a tight read-modify-write loop against a shared, on-disk [`Store`] (the
/// same redb-backed durable store a guest reaches through the storage capability).
/// Each op is `get` → increment → `set` — the canonical durable-counter unit of
/// work — and **every `set` is its own ACID commit**, so the headline is the
/// sustainable *durable* write rate, not an in-memory one.
///
/// redb serialises writers behind a single commit lock while readers run
/// concurrently (MVCC), so adding workers past the core count mostly deepens the
/// commit queue — this measures the honest durable-write ceiling. Latency is the
/// end-to-end read-modify-write time. Must be constructed inside a Tokio runtime.
pub struct KvStormEngine {
    runtime: Runtime,
    workers: Vec<ProcessHandle>,
    ops: Arc<AtomicU64>,
    latency_rx: UnboundedReceiver<u64>,
    // Keeps the store alive for the engine's lifetime; the temp dir is removed on Drop.
    _store: Store,
    _dir: tempfile::TempDir,
    last_ops: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl KvStormEngine {
    pub fn new(workers: usize, scheduler_count: usize) -> Self {
        let runtime = Runtime::new();
        let dir = tempfile::tempdir().expect("kv temp dir");
        let store = Store::open(dir.path().join("bench.redb")).expect("open kv store");

        let ops = Arc::new(AtomicU64::new(0));
        let (latency_tx, latency_rx) = unbounded_channel();
        let worker_count = workers.max(1);

        let handles = (0..worker_count)
            .map(|i| {
                let bucket = store.bucket(BUCKET);
                let key = format!("counter-{i}");
                let ops = Arc::clone(&ops);
                let latency_tx = latency_tx.clone();
                // Seed the key so the very first `get` reads a real value, never a miss.
                bucket.set(&key, &0u64.to_le_bytes()).expect("seed key");
                runtime.spawn(move |_ctx| async move {
                    loop {
                        let started = Instant::now();
                        // Read-modify-write: the durable-counter workload. The `set` is
                        // an ACID commit (the cost we are measuring).
                        let current = bucket
                            .get(&key)
                            .expect("get")
                            .map_or(0, |v| u64::from_le_bytes(v.try_into().unwrap_or_default()));
                        bucket
                            .set(&key, &current.wrapping_add(1).to_le_bytes())
                            .expect("set");
                        ops.fetch_add(1, Ordering::Relaxed);
                        // A durable commit (fsync) is the dominant cost, so each op is a
                        // worthwhile latency sample; the per-tick cap bounds the stream.
                        let _ = latency_tx.send(started.elapsed().as_nanos() as u64);
                        // Cooperative yield: blocking redb commits would otherwise pin a
                        // scheduler thread, starving kill/abort and other processes.
                        tokio::task::yield_now().await;
                    }
                })
            })
            .collect();

        Self {
            runtime,
            workers: handles,
            ops,
            latency_rx,
            _store: store,
            _dir: dir,
            last_ops: 0,
            last_at: Instant::now(),
            scheduler_count,
        }
    }

    pub fn tick(&mut self) -> Sample {
        let now = Instant::now();
        let ops = self.ops.load(Ordering::Relaxed);
        let dt = now
            .duration_since(self.last_at)
            .as_secs_f64()
            .max(f64::MIN_POSITIVE);
        let ops_per_sec = ops.saturating_sub(self.last_ops) as f64 / dt;
        self.last_ops = ops;
        self.last_at = now;

        let mut latencies_ns = Vec::new();
        while let Ok(ns) = self.latency_rx.try_recv() {
            latencies_ns.push(ns);
        }
        if latencies_ns.len() > LATENCY_SAMPLE {
            latencies_ns = latencies_ns.split_off(latencies_ns.len() - LATENCY_SAMPLE);
        }

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

impl Drop for KvStormEngine {
    fn drop(&mut self) {
        // The workers loop forever; stop them so they release the store before the
        // temp dir is removed.
        for worker in &self.workers {
            worker.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn workers_commit_durable_writes_and_report_rate_and_latency() {
        let mut engine = KvStormEngine::new(4, 4);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let sample = engine.tick();
        assert!(
            sample.ops_per_sec > 0.0,
            "durable writes should be committing"
        );
        assert_eq!(sample.process_count, 4); // four worker processes alive
        assert_eq!(sample.scheduler_load.len(), 4);
        assert!(
            !sample.latencies_ns.is_empty(),
            "read-modify-writes should be timed"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn writes_actually_persist_and_advance() {
        // The counter must genuinely climb in the durable store — proof the storm
        // commits real values, not a no-op rate.
        let engine = KvStormEngine::new(1, 2);
        let bucket = engine._store.bucket(BUCKET);
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let value = bucket
            .get("counter-0")
            .unwrap()
            .map(|v| u64::from_le_bytes(v.try_into().unwrap()))
            .unwrap();
        assert!(value > 0, "the durable counter advanced ({value})");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_count_is_at_least_one() {
        let engine = KvStormEngine::new(0, 1);
        assert_eq!(engine.workers.len(), 1);
    }
}
