use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rusm_otp::{Context, ExitReason, ProcessHandle, Received, Runtime};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::sample::Sample;

/// Children each supervisor keeps alive. Small on purpose: restart throughput is
/// driven by how fast a supervisor detects-and-replaces, not by a huge population.
const CHILDREN_PER_SUPERVISOR: usize = 4;
/// Recovery latency is timed every Nth restart, bounding the sample stream.
const LATENCY_EVERY: u64 = 64;
/// Most latency samples surfaced in a single tick.
const LATENCY_SAMPLE: usize = 64;

/// A **real** "let it crash" workload over `rusm-otp`: each supervisor traps
/// exits, `spawn_link`s a handful of children, and restarts any that die. The
/// children crash immediately (via `exit(self, Crashed)` — an abnormal exit with
/// no Rust panic, so the restart storm doesn't flood stderr), so the system runs
/// a continuous crash→detect→restart cycle.
///
/// [`tick`](Self::tick) samples restarts/sec (Δrestarts / Δt) and recovery
/// latency (how long a supervisor takes to bring a replacement up). Must be
/// constructed inside a Tokio runtime.
pub struct FaultRecoveryEngine {
    runtime: Runtime,
    supervisors: Vec<ProcessHandle>,
    restarts: Arc<AtomicU64>,
    latency_rx: UnboundedReceiver<u64>,
    last_restarts: u64,
    last_at: Instant,
    scheduler_count: usize,
}

/// Spawns a child linked to `supervisor` that crashes the instant it runs.
fn spawn_crashing_child(runtime: &Runtime, supervisor: rusm_otp::Pid) {
    let child_rt = runtime.clone();
    runtime.spawn_link(supervisor, move |ctx: Context| async move {
        // Abnormal exit, but not a panic — keeps the crash storm quiet and fast.
        child_rt.exit(ctx.pid(), ExitReason::Crashed);
    });
}

impl FaultRecoveryEngine {
    pub fn new(supervisors: usize, scheduler_count: usize) -> Self {
        let runtime = Runtime::new();
        let restarts = Arc::new(AtomicU64::new(0));
        let (latency_tx, latency_rx) = unbounded_channel();
        let mut handles = Vec::new();

        for _ in 0..supervisors.max(1) {
            let sup_rt = runtime.clone();
            let restarts = Arc::clone(&restarts);
            let latency_tx: UnboundedSender<u64> = latency_tx.clone();
            let supervisor = runtime.spawn(move |mut ctx| async move {
                let me = ctx.pid();
                // Trap first, so a child can never crash before we'd hear it.
                sup_rt.set_trap_exit(me, true);
                for _ in 0..CHILDREN_PER_SUPERVISOR {
                    spawn_crashing_child(&sup_rt, me);
                }
                let mut restarted: u64 = 0;
                loop {
                    // A dead child arrives as a trapped Exit; anything else is ignored.
                    if let Received::Exit { .. } = ctx.recv().await {
                        let started = Instant::now();
                        spawn_crashing_child(&sup_rt, me);
                        restarts.fetch_add(1, Ordering::Relaxed);
                        restarted += 1;
                        if restarted.is_multiple_of(LATENCY_EVERY) {
                            let _ = latency_tx.send(started.elapsed().as_nanos() as u64);
                        }
                    }
                }
            });
            handles.push(supervisor);
        }

        Self {
            runtime,
            supervisors: handles,
            restarts,
            latency_rx,
            last_restarts: 0,
            last_at: Instant::now(),
            scheduler_count,
        }
    }

    pub fn tick(&mut self) -> Sample {
        let now = Instant::now();
        let restarts = self.restarts.load(Ordering::Relaxed);
        let dt = now
            .duration_since(self.last_at)
            .as_secs_f64()
            .max(f64::MIN_POSITIVE);
        let ops_per_sec = restarts.saturating_sub(self.last_restarts) as f64 / dt;
        self.last_restarts = restarts;
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

impl Drop for FaultRecoveryEngine {
    fn drop(&mut self) {
        // Killing each supervisor cascades down its links to its children.
        for supervisor in &self.supervisors {
            supervisor.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn supervisors_restart_crashing_children() {
        let mut engine = FaultRecoveryEngine::new(2, 4);
        // Warm up: the crash→restart cycle should turn over many times.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let sample = engine.tick();
        assert!(sample.ops_per_sec > 0.0, "restarts should be happening");
        assert!(
            !sample.latencies_ns.is_empty(),
            "recovery latency should be timed"
        );
        assert_eq!(sample.scheduler_load.len(), 4);
        // Population is self-regulating around supervisors + their children.
        assert!(sample.process_count >= 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervisor_count_is_at_least_one() {
        let engine = FaultRecoveryEngine::new(0, 1);
        assert_eq!(engine.supervisors.len(), 1);
    }
}
