use std::time::Instant;

use rusm_otp::{ProcessHandle, Runtime};
use rusm_wasm::WasmRuntime;

use crate::sample::Sample;

/// A CPU-bound guest: a tight loop that never returns and never yields on its
/// own. Without preemption it would pin a scheduler thread forever.
const SPINNER: &str = r#"(module (func (export "run") (loop (br 0))))"#;

/// A "bystander" guest: loops calling the `notify` host function, which bumps the
/// runtime's shared counter — its call rate is how much progress it's making.
const BYSTANDER: &str = r#"(module
    (import "rusm" "notify" (func $notify))
    (func (export "run") (loop (call $notify) (br 0))))"#;

/// A **real** fairness workload over `rusm-wasm`: tight-loop Wasm spinners run
/// alongside Wasm bystanders. Epoch preemption forces the spinners to yield, so
/// the bystanders keep making progress instead of being starved — Wasmtime
/// epochs standing in for the BEAM's reduction counting.
///
/// [`tick`](Self::tick) reports bystander progress (notify calls/sec). A nonzero
/// rate *is* the proof: without preemption, spinners filling every scheduler
/// thread would pin them to zero. Must be constructed inside a Tokio runtime.
pub struct FairnessEngine {
    runtime: Runtime,
    // Owns the epoch ticker (preemption) and the guest-progress counter; kept
    // alive for the engine's lifetime.
    wasm: WasmRuntime,
    processes: Vec<ProcessHandle>,
    last_progress: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl FairnessEngine {
    pub fn new(workers: usize, scheduler_count: usize) -> Self {
        let bystanders = workers.max(1);
        // One spinner per CPU core, so they genuinely fill the scheduler threads —
        // the bystanders can then *only* make progress if preemption is yielding
        // the spinners. (Anything less and there'd be a spare core and no real test.)
        let spinners = std::thread::available_parallelism().map_or(4, |n| n.get());

        let runtime = Runtime::new();
        let wasm = WasmRuntime::new(runtime.clone()).expect("wasm engine");

        let spinner = wasm.compile(SPINNER).expect("compile spinner");
        let bystander = wasm.compile(BYSTANDER).expect("compile bystander");
        let spinner = wasm.prepare(&spinner).expect("prepare spinner");
        let bystander = wasm.prepare(&bystander).expect("prepare bystander");

        let mut processes = Vec::with_capacity(spinners + bystanders);
        for _ in 0..spinners {
            processes.push(wasm.spawn(&spinner, "run"));
        }
        for _ in 0..bystanders {
            processes.push(wasm.spawn(&bystander, "run"));
        }

        Self {
            runtime,
            wasm,
            processes,
            last_progress: 0,
            last_at: Instant::now(),
            scheduler_count,
        }
    }

    pub fn tick(&mut self) -> Sample {
        let now = Instant::now();
        let progress = self.wasm.notifications();
        let dt = now
            .duration_since(self.last_at)
            .as_secs_f64()
            .max(f64::MIN_POSITIVE);
        let ops_per_sec = progress.saturating_sub(self.last_progress) as f64 / dt;
        self.last_progress = progress;
        self.last_at = now;

        let process_count = self.runtime.process_count() as u64;
        Sample {
            ops_per_sec,
            process_count,
            running: process_count,
            waiting: 0,
            total_memory_bytes: 0,
            latencies_ns: Vec::new(),
            processes: Vec::new(),
            scheduler_load: vec![0.0; self.scheduler_count],
        }
    }
}

impl Drop for FairnessEngine {
    fn drop(&mut self) {
        for process in &self.processes {
            process.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bystanders_progress_despite_spinners() {
        let mut engine = FairnessEngine::new(2, 4);
        // Spinners saturate the workers; epoch preemption must still let the
        // bystanders run. Poll until we see progress (robust to scheduling and
        // parallel-test load) rather than betting on one fixed window — but
        // bounded, so a genuine starvation bug fails instead of hanging.
        let mut sample = engine.tick();
        let mut progressed = false;
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            sample = engine.tick();
            if sample.ops_per_sec > 0.0 {
                progressed = true;
                break;
            }
        }
        assert!(
            progressed,
            "bystanders must keep progressing under tight-loop spinners"
        );
        assert!(sample.process_count >= 2);
        assert_eq!(sample.scheduler_load.len(), 4);
    }
}
