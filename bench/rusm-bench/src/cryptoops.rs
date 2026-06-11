use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rusm_otp::{ProcessHandle, Runtime};
use rusm_wasm::WasmRuntime;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use crate::sample::Sample;

/// Record a digest round-trip's latency every Nth op, bounding the sample stream.
const LATENCY_EVERY: u64 = 16;
/// Most latency samples surfaced in a single tick.
const LATENCY_SAMPLE: usize = 64;

/// A persistent **TypeScript crypto service** on the embedded rquickjs runner: it
/// receives its driver's pid once, then for every request hashes a fixed 256-byte
/// payload with `crypto.subtle.digest("SHA-256", …)` (native RustCrypto behind the
/// Web Crypto ABI) and replies. Sandboxed — `crypto.subtle` needs no capability.
const SERVICE: &str = r#"
    module.exports.default = async function () {
        const driver = await Process.receiveText();
        const data = new Uint8Array(256);
        for (let i = 0; i < data.length; i++) data[i] = i & 255;
        for (;;) {
            await Process.receiveText();                  // a "go" token
            await crypto.subtle.digest("SHA-256", data);  // the real work
            Process.send(driver, "ok");
        }
    };
"#;

/// A **real, continuous crypto.subtle storm** through TypeScript guests: `instances`
/// sandboxed rquickjs services each hash a payload per request while a Rust driver
/// keeps one request in flight and counts the replies. [`tick`](Self::tick) reports
/// SHA-256 digests/sec and the per-digest round-trip latency.
///
/// The number is the honest rate at which a sandboxed TS guest can *serve* crypto —
/// it includes the rquickjs call and the message round-trip, the true cost of
/// offering Web Crypto from a JS guest. Must be constructed inside a Tokio runtime.
pub struct CryptoOpsEngine {
    runtime: Runtime,
    // Owns the Wasm engine + epoch ticker for the js-runner instances.
    _wasm: Arc<WasmRuntime>,
    processes: Vec<ProcessHandle>,
    ops: Arc<AtomicU64>,
    latency_rx: UnboundedReceiver<u64>,
    last_ops: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl CryptoOpsEngine {
    pub fn new(instances: usize, scheduler_count: usize) -> Self {
        let runtime = Runtime::new();
        let wasm = Arc::new(WasmRuntime::new(runtime.clone()).expect("wasm engine"));
        let ops = Arc::new(AtomicU64::new(0));
        let (latency_tx, latency_rx) = unbounded_channel();
        let mut processes = Vec::new();

        for _ in 0..instances.max(1) {
            let guest = wasm.spawn_js(SERVICE.as_bytes());
            let guest_pid = guest.pid();
            processes.push(guest);

            // Driver: hand the guest our pid, then loop request → reply, counting and
            // timing. One request in flight at a time (the round-trip is the unit).
            let driver_rt = runtime.clone();
            let ops = Arc::clone(&ops);
            let latency_tx = latency_tx.clone();
            let driver = runtime.spawn(move |mut ctx| async move {
                driver_rt.send(guest_pid, ctx.pid().raw().to_string().into_bytes());
                let mut round: u64 = 0;
                loop {
                    let started = Instant::now();
                    driver_rt.send(guest_pid, b"go".to_vec());
                    let _ = ctx.recv().await; // "ok" — one digest done
                    ops.fetch_add(1, Ordering::Relaxed);
                    round += 1;
                    if round.is_multiple_of(LATENCY_EVERY) {
                        let _ = latency_tx.send(started.elapsed().as_nanos() as u64);
                    }
                }
            });
            processes.push(driver);
        }

        Self {
            runtime,
            _wasm: wasm,
            processes,
            ops,
            latency_rx,
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

impl Drop for CryptoOpsEngine {
    fn drop(&mut self) {
        for process in &self.processes {
            process.kill();
        }
        // Catch-all: tear down any js-runner instance still on the runtime.
        self.runtime.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ts_guests_hash_and_report_rate_and_latency() {
        let mut engine = CryptoOpsEngine::new(2, 4);
        // rquickjs start-up + warm-up so digests flow and samples surface.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let sample = engine.tick();
        assert!(sample.ops_per_sec > 0.0, "digests should be flowing");
        assert!(sample.process_count >= 2, "guests + drivers are alive");
        assert_eq!(sample.scheduler_load.len(), 4);
        assert!(
            !sample.latencies_ns.is_empty(),
            "digest round-trips should be timed"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn instance_count_is_at_least_one() {
        let engine = CryptoOpsEngine::new(0, 1);
        assert_eq!(engine.processes.len(), 2); // one guest + one driver
    }
}
