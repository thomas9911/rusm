//! Live dashboard engines for the serving scenarios (HTTP / WS / SSE, Rust + TS),
//! driving the **resident** servers — a supervised pool of *long-lived* component
//! instances that hold state across requests (`resident_http_server` /
//! `resident_ws_server` + the `*_js` twins), not a fresh instance per request. This
//! is the "real server" deployment: warm instances, no per-request instantiation,
//! per-instance restart isolation.
//!
//! The pool is sized to the resource profile (`instances = workers`), so it's the
//! resident model without being artificially bottlenecked to a single instance.
//! Load is generated through the shared [`rusm_loadtest`] path — **balter** for HTTP
//! request rate, the connection-capacity harness for WS/SSE held connections.
//!
//! These remain **co-resident live demos** (load + server in the node process); the
//! *fair* headline numbers come from the out-of-process `rusm-loadtest` binary
//! against a real `rusm serve` port. No leaks: the balter window loop self-exits on
//! stop, `CapacityLoad` stops on drop, and each engine `shutdown()`s its runtime.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusm_loadtest::capacity::{CapacityLoad, Protocol};
use rusm_otp::Runtime;
use rusm_wasm::{CapabilityProfile, WasmRuntime};
use tokio::net::TcpListener;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

use crate::sample::Sample;
use crate::scenario::Guest;

// Resident handler fixtures: each component's `run` drives a serving loop (RS via
// `rusm_rs::{http,ws}::serve`; TS via `export default` on the persistent js-runner,
// switched by `RUSM_SERVE_ROLE`). They hold state across requests.
const RS_HTTP: &[u8] =
    include_bytes!("../../../crates/rusm-wasm/tests/fixtures/rs_resident_count.wasm");
const TS_HTTP: &str = include_str!("../../../crates/rusm-wasm/tests/fixtures/ts_resident_count.js");
const RS_WS: &[u8] =
    include_bytes!("../../../crates/rusm-wasm/tests/fixtures/rs_resident_ws_echo.wasm");
// The TS **echo** handler (matches the RS echo: one send per frame), not the
// broadcast chat room — so ws-echo-ts measures the same O(N) workload as ws-echo.
const TS_WS: &str = include_str!("../../../crates/rusm-wasm/tests/fixtures/ts_resident_ws_echo.js");
const RS_SSE: &[u8] =
    include_bytes!("../../../crates/rusm-wasm/tests/fixtures/rs_resident_sse.wasm");
const TS_SSE: &str = include_str!("../../../crates/rusm-wasm/tests/fixtures/ts_resident_sse.js");

/// At most this many latency samples surfaced per tick.
const LATENCY_SAMPLE: usize = 64;
/// Each balter window completes before the next, so its workers shut down cleanly
/// (balter only aborts workers on completion, never on drop) — no task leak.
const HTTP_WINDOW: Duration = Duration::from_secs(3);

/// Trusted's resources (roomy heap) but no inherited stdio: serving guests don't
/// print, and this keeps any on-disconnect traps off the node's stderr.
fn serving_caps() -> rusm_wasm::Capabilities {
    CapabilityProfile::Trusted
        .capabilities()
        .inherit_stdio(false)
}

/// Resident pool size — a small warm pool scaled by the resource profile (≥2). A
/// genuine resident deployment (fixed long-lived instances), not instance-per-request.
fn pool_size(workers: usize) -> usize {
    workers.max(2)
}

/// Binds an ephemeral loopback port and returns it plus the adopted listener.
fn bind_loopback() -> (std::net::SocketAddr, TcpListener) {
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    std_listener
        .set_nonblocking(true)
        .expect("listener non-blocking");
    let addr = std_listener.local_addr().expect("listener addr");
    let listener = TcpListener::from_std(std_listener).expect("adopt listener");
    (addr, listener)
}

/// **HTTP throughput** (Rust or TS), driven live by **balter** against a **resident**
/// HTTP handler (a warm pool that holds state across requests). The tile charts the
/// achieved req/s and request latency.
pub struct HttpServingEngine {
    // Held for the run; dropping it stops the server's epoch ticker + reclaims processes.
    _wr: WasmRuntime,
    // The live process count — the resident pool + per-request responder processes
    // (the logical concurrency actually spun), reported on the tile.
    rt: Runtime,
    server_task: JoinHandle<()>,
    stop: Arc<AtomicBool>,
    lat_rx: UnboundedReceiver<u64>,
    last_achieved: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl HttpServingEngine {
    pub fn new(workers: usize, scheduler_count: usize, guest: Guest) -> Self {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).expect("wasm runtime");
        let caps = serving_caps();
        let instances = pool_size(workers);
        let (addr, listener) = bind_loopback();

        // Build the resident HTTP server and drive its accept loop on a task.
        let server_task = match guest {
            Guest::Rust => {
                let prepared = wr
                    .prepare_component(
                        &wr.compile_component(RS_HTTP)
                            .expect("compile resident http"),
                        "run",
                    )
                    .expect("prepare resident http");
                tokio::spawn(
                    wr.resident_http_server(&prepared, caps, instances)
                        .serve(listener),
                )
            }
            Guest::Ts => tokio::spawn(
                wr.resident_http_server_js(TS_HTTP.as_bytes().to_vec(), caps, instances)
                    .serve(listener),
            ),
        };

        // A steady, sustainable offered rate scaled by the profile; balter paces it
        // and seeds enough concurrency to hit it without a long ramp.
        let target_tps = (workers as u32 * 5_000).clamp(8_000, 50_000);
        let concurrency = (workers * 12).clamp(24, 256);

        rusm_loadtest::http::set_target(format!("http://{addr}/"));
        rusm_loadtest::http::reset_counter();
        let lat_rx = rusm_loadtest::http::install_latency_sink();

        let stop = Arc::new(AtomicBool::new(false));
        // Bounded-window loop: each window completes (clean balter shutdown), then we
        // re-check `stop`. Detached on purpose — never aborted mid-window (that would
        // leak balter's workers); it self-exits within one window of `stop`.
        let loop_stop = Arc::clone(&stop);
        tokio::spawn(async move {
            while !loop_stop.load(Ordering::Relaxed) {
                let _ = rusm_loadtest::http::run_window(target_tps, concurrency, HTTP_WINDOW).await;
            }
        });

        Self {
            _wr: wr,
            rt,
            server_task,
            stop,
            lat_rx,
            last_achieved: 0,
            last_at: Instant::now(),
            scheduler_count,
        }
    }

    pub fn tick(&mut self) -> Sample {
        let now = Instant::now();
        let achieved = rusm_loadtest::http::achieved();
        let dt = now
            .duration_since(self.last_at)
            .as_secs_f64()
            .max(f64::MIN_POSITIVE);
        let ops_per_sec = achieved.saturating_sub(self.last_achieved) as f64 / dt;
        self.last_achieved = achieved;
        self.last_at = now;

        let mut latencies_ns = Vec::new();
        while let Ok(ns) = self.lat_rx.try_recv() {
            latencies_ns.push(ns);
        }
        if latencies_ns.len() > LATENCY_SAMPLE {
            latencies_ns = latencies_ns.split_off(latencies_ns.len() - LATENCY_SAMPLE);
        }

        // The charted concurrency is the real live RUSM process count — the resident
        // pool plus the per-request responder processes currently in flight.
        let process_count = self.rt.process_count() as u64;
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

impl Drop for HttpServingEngine {
    fn drop(&mut self) {
        // The window loop self-exits within one window (never aborted mid-window).
        self.stop.store(true, Ordering::Relaxed);
        rusm_loadtest::http::clear_latency_sink();
        self.server_task.abort();
        // Reclaim the resident pool + any in-flight work so none lingers into the next run.
        self._wr.shutdown();
    }
}

/// Which connection-capacity workload a [`CapacityServingEngine`] serves.
#[derive(Clone, Copy)]
pub enum CapacityKind {
    Ws,
    Sse,
}

/// **WS echo** or **SSE fan-out** (Rust or TS) against a **resident** server (a warm
/// pool holding connection/stream state), with many held connections driven by the
/// shared capacity harness. Charts round-trips/sec (WS) or events/sec (SSE) and live
/// concurrency.
pub struct CapacityServingEngine {
    _wr: WasmRuntime,
    // Live process count: the resident pool + a writer/responder process per held
    // connection or stream — the logical concurrency actually spun.
    rt: Runtime,
    server_task: JoinHandle<()>,
    load: CapacityLoad,
    last_ops: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl CapacityServingEngine {
    pub fn new(workers: usize, scheduler_count: usize, kind: CapacityKind, guest: Guest) -> Self {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).expect("wasm runtime");
        let caps = serving_caps();
        let instances = pool_size(workers);
        let (addr, listener) = bind_loopback();

        // Build the matching resident server (WS gateway, or HTTP for SSE streaming)
        // and drive its accept loop; the capacity harness then holds the connections.
        let (server_task, url, proto) = match (kind, guest) {
            (CapacityKind::Ws, Guest::Rust) => {
                let prepared = wr
                    .prepare_component(
                        &wr.compile_component(RS_WS).expect("compile resident ws"),
                        "run",
                    )
                    .expect("prepare resident ws");
                let task = tokio::spawn(
                    wr.resident_ws_server(&prepared, caps, instances)
                        .serve(listener),
                );
                (task, format!("ws://{addr}/"), Protocol::Ws)
            }
            (CapacityKind::Ws, Guest::Ts) => {
                let task = tokio::spawn(
                    wr.resident_ws_server_js(TS_WS.as_bytes().to_vec(), caps, instances)
                        .serve(listener),
                );
                (task, format!("ws://{addr}/"), Protocol::Ws)
            }
            (CapacityKind::Sse, Guest::Rust) => {
                let prepared = wr
                    .prepare_component(
                        &wr.compile_component(RS_SSE).expect("compile resident sse"),
                        "run",
                    )
                    .expect("prepare resident sse");
                let task = tokio::spawn(
                    wr.resident_http_server(&prepared, caps, instances)
                        .serve(listener),
                );
                (task, format!("http://{addr}/"), Protocol::Sse)
            }
            (CapacityKind::Sse, Guest::Ts) => {
                let task = tokio::spawn(
                    wr.resident_http_server_js(TS_SSE.as_bytes().to_vec(), caps, instances)
                        .serve(listener),
                );
                (task, format!("http://{addr}/"), Protocol::Sse)
            }
        };

        let connections = (workers * 128).clamp(64, 768);
        let load = CapacityLoad::start(proto, url, connections);

        Self {
            _wr: wr,
            rt,
            server_task,
            load,
            last_ops: 0,
            last_at: Instant::now(),
            scheduler_count,
        }
    }

    pub fn tick(&mut self) -> Sample {
        let now = Instant::now();
        let ops = self.load.ops();
        let dt = now
            .duration_since(self.last_at)
            .as_secs_f64()
            .max(f64::MIN_POSITIVE);
        let ops_per_sec = ops.saturating_sub(self.last_ops) as f64 / dt;
        self.last_ops = ops;
        self.last_at = now;

        let mut latencies_ns = self.load.drain_latencies();
        if latencies_ns.len() > LATENCY_SAMPLE {
            latencies_ns = latencies_ns.split_off(latencies_ns.len() - LATENCY_SAMPLE);
        }

        // Real live RUSM processes (resident pool + per-connection writer/responder
        // processes) — the logical concurrency, not the client-side connection count.
        let process_count = self.rt.process_count() as u64;
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

impl Drop for CapacityServingEngine {
    fn drop(&mut self) {
        // `load` drops here too (stopping + aborting its held connections); be explicit.
        self.load.stop();
        self.server_task.abort();
        self._wr.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Polls an engine until it reports throughput (or times out).
    async fn until_throughput(mut tick: impl FnMut() -> Sample) -> f64 {
        let mut max = 0.0_f64;
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            max = max.max(tick().ops_per_sec);
            if max > 0.0 {
                break;
            }
        }
        max
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn http_engine_serves_balter_driven_throughput() {
        let mut engine = HttpServingEngine::new(1, 4, Guest::Rust);
        let max = until_throughput(|| engine.tick()).await;
        assert!(
            max > 0.0,
            "balter-driven resident HTTP produced throughput (max {max:.0}/s)"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ws_engine_echoes_under_held_connections() {
        let mut engine = CapacityServingEngine::new(1, 4, CapacityKind::Ws, Guest::Rust);
        let max = until_throughput(|| engine.tick()).await;
        assert!(
            max > 0.0,
            "resident WS echo produced round-trips (max {max:.0}/s)"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sse_engine_streams_events() {
        let mut engine = CapacityServingEngine::new(1, 4, CapacityKind::Sse, Guest::Rust);
        let max = until_throughput(|| engine.tick()).await;
        assert!(max > 0.0, "resident SSE produced events (max {max:.0}/s)");
    }
}
