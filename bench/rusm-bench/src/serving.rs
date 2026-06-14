//! Live dashboard engines for the serving scenarios (HTTP / WS / SSE, Rust + TS),
//! driving the **per-request** servers — a fresh sandboxed instance per request
//! (HTTP/SSE via `http_server` / `http_server_js`, `wasi:http`) or per connection
//! (WS via `ws_server` / `ws_server_js`). This is the shape RUSM standardizes serving
//! on: stateless, isolated, no head-of-line blocking by construction.
//!
//! Load is generated through the shared [`rusm_loadtest`] path — a **closed-loop** driver
//! for HTTP (a fixed set of outstanding requests, sized to the guest's capacity), the
//! connection-capacity harness for WS/SSE held connections. Closed-loop self-limits to the
//! real ceiling, so the live tile holds rock-steady and never floods or collapses,
//! whatever the guest's speed. (The *fair* out-of-process headline still uses balter's
//! rate sweep — a deliberate max-rate measurement, [`rusm_loadtest::http::run`].)
//!
//! These remain **co-resident live demos** (load + server in the node process); the
//! *fair* headline numbers come from the out-of-process `rusm-loadtest` binary against a
//! real `rusm serve` port. No leaks: the closed-loop driver stops on `stop`, `CapacityLoad`
//! stops on drop, and each engine `shutdown()`s its runtime.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rusm_loadtest::capacity::{CapacityLoad, Protocol};
use rusm_otp::Runtime;
use rusm_wasm::{CapabilityProfile, WasmRuntime};
use tokio::net::TcpListener;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

use crate::sample::Sample;
use crate::scenario::Guest;

// Per-request serving fixtures. HTTP/SSE are `wasi:http` components (RS) or
// `export default { fetch }` bundles (TS) instantiated per request; WS is one
// sandboxed component process per connection (RS actor / TS `websocket` worker).
const RS_HTTP: &[u8] = include_bytes!("../../../crates/rusm-wasm/tests/fixtures/http_lean.wasm");
const TS_HTTP: &str = include_str!("../../../crates/rusm-wasm/tests/fixtures/ts_http_hello.js");
const RS_WS: &[u8] = include_bytes!("../../../crates/rusm-wasm/tests/fixtures/rs_ws_echo.wasm");
// The TS **echo** worker (matches the RS echo: one send per frame), so ws-echo-ts
// measures the same O(N) per-connection workload as ws-echo.
const TS_WS: &str = include_str!("../../../crates/rusm-wasm/tests/fixtures/ts_ws_echo.js");
// A `wasi:http` component / bundle that streams an endless `text/event-stream`.
const RS_SSE: &[u8] = include_bytes!("../../../crates/rusm-wasm/tests/fixtures/sse_firehose.wasm");
const TS_SSE: &str = include_str!("../../../crates/rusm-wasm/tests/fixtures/ts_sse_firehose.js");

/// At most this many latency samples surfaced per tick.
const LATENCY_SAMPLE: usize = 64;

/// Trusted's resources (roomy heap) but no inherited stdio: serving guests don't
/// print, and this keeps any on-disconnect traps off the node's stderr.
fn serving_caps() -> rusm_wasm::Capabilities {
    CapabilityProfile::Trusted
        .capabilities()
        .inherit_stdio(false)
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

/// **HTTP throughput** (Rust or TS): a **closed-loop** driver holds a fixed set of
/// outstanding requests (sized to the guest's capacity) against a **per-request** HTTP
/// handler (a fresh sandboxed instance per request), so the tile holds steady at the real
/// ceiling and never collapses. Charts the achieved req/s and request latency.
pub struct HttpServingEngine {
    // Held for the run; dropping it stops the server's epoch ticker + reclaims processes.
    _wr: WasmRuntime,
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
        let (addr, listener) = bind_loopback();

        // Build the per-request HTTP server and drive its accept loop on a task.
        let server_task = match guest {
            Guest::Rust => {
                let prepared = wr
                    .prepare_http(&wr.compile_component(RS_HTTP).expect("compile http"))
                    .expect("prepare wasi:http");
                tokio::spawn(wr.http_server(&prepared, caps).serve(listener))
            }
            Guest::Ts => tokio::spawn(wr.http_server_js(TS_HTTP.to_string(), caps).serve(listener)),
        };

        // Closed-loop load: a fixed set of outstanding requests, sized to the guest's real
        // capacity. The Rust path instantiates a component per request cheaply, so it
        // sustains a high concurrency. The (wizer-warmed) TS path is CPU-bound, so its
        // throughput ceiling is at the scheduler parallelism — concurrency = cores. Past
        // that, more in-flight requests don't raise req/s, they only add queue latency
        // (measured on an 8-core box: 8→~5.3k/s @1.4ms, 16→~5.6k/s @2.6ms, 32→~6k/s
        // @5.7ms), so there's no reason to chart more processes than the cores can run.
        // Closed-loop self-limits to the true capacity: it holds **rock-steady** at the
        // ceiling and can never flood or collapse to zero (no open-loop rate chase, no
        // balter global state to wedge across scenario switches).
        let concurrency = match guest {
            Guest::Rust => (workers * 12).clamp(24, 256),
            Guest::Ts => scheduler_count.clamp(4, 32),
        };

        rusm_loadtest::http::set_target(format!("http://{addr}/"));
        rusm_loadtest::http::reset_counter();
        let lat_rx = rusm_loadtest::http::install_latency_sink();

        let stop = Arc::new(AtomicBool::new(false));
        tokio::spawn(rusm_loadtest::http::run_closed_loop(
            concurrency,
            Arc::clone(&stop),
        ));

        Self {
            _wr: wr,
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

        // The charted concurrency: the per-request handler instances running concurrently
        // right now. HTTP throughput serves on the `wasi:http` path (a wasm instance per
        // request on a Tokio task, not an `rt.spawn`'d process), so `rt.process_count()`
        // is structurally 0 here; the real count is the load's in-flight requests (one
        // in-flight request == one handler instance in flight). Small by nature — at this
        // rate each handler lives microseconds, so only a handful overlap (Little's law).
        let process_count = rusm_loadtest::http::inflight();
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
        // Reclaim any in-flight per-request work so none lingers into the next run.
        self._wr.shutdown();
    }
}

/// Which connection-capacity workload a [`CapacityServingEngine`] serves.
#[derive(Clone, Copy)]
pub enum CapacityKind {
    Ws,
    Sse,
}

/// **WS echo** or **SSE fan-out** (Rust or TS) against a **per-request** server (one
/// sandboxed process per connection / a fresh instance per SSE stream), with many held
/// connections driven by the shared capacity harness. Charts round-trips/sec (WS) or
/// events/sec (SSE) and live concurrency.
pub struct CapacityServingEngine {
    _wr: WasmRuntime,
    // Live process count: a process per held connection or stream (plus its writer/
    // responder) — the logical concurrency actually spun.
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
        let (addr, listener) = bind_loopback();

        // Build the matching per-request server (a WS process per connection, or a
        // `wasi:http` SSE instance per stream); the capacity harness holds the connections.
        let (server_task, url, proto) = match (kind, guest) {
            (CapacityKind::Ws, Guest::Rust) => {
                let prepared = wr
                    .prepare_component(&wr.compile_component(RS_WS).expect("compile ws"), "run")
                    .expect("prepare ws");
                let task = tokio::spawn(wr.ws_server(&prepared, caps).serve(listener));
                (task, format!("ws://{addr}/"), Protocol::Ws)
            }
            (CapacityKind::Ws, Guest::Ts) => {
                let task = tokio::spawn(wr.ws_server_js(TS_WS.to_string(), caps).serve(listener));
                (task, format!("ws://{addr}/"), Protocol::Ws)
            }
            (CapacityKind::Sse, Guest::Rust) => {
                let prepared = wr
                    .prepare_http(&wr.compile_component(RS_SSE).expect("compile sse"))
                    .expect("prepare wasi:http sse");
                let task = tokio::spawn(wr.http_server(&prepared, caps).serve(listener));
                (task, format!("http://{addr}/"), Protocol::Sse)
            }
            (CapacityKind::Sse, Guest::Ts) => {
                let task =
                    tokio::spawn(wr.http_server_js(TS_SSE.to_string(), caps).serve(listener));
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

        // Real live RUSM processes (a process per connection/stream plus its writer/
        // responder) — the logical concurrency, not the client-side connection count.
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
    use std::time::Duration;

    /// Warm up until throughput appears, then verify it's **sustained** — every tick
    /// over a ~1.5s window keeps producing. Returns the minimum *nonzero* sustained
    /// rate, or `0.0` if it stalled mid-run. A first-nonzero check would miss the
    /// silent-zero class (a finite stream the harness holds as if infinite: it spikes
    /// once, then drops to 0 **and stays there**). We catch exactly that — a permanent
    /// stall — by failing on **two consecutive** zero ticks, while tolerating a single
    /// transient dip (CPU starvation when the whole suite runs in parallel): a real
    /// collapse is all-zero, so it always trips the consecutive check.
    async fn sustained_throughput(mut tick: impl FnMut() -> Sample) -> f64 {
        // Warm-up: engines ramp (connect, instantiate, fill the closed loop) — wait for first ops.
        let mut warmed = false;
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if tick().ops_per_sec > 0.0 {
                warmed = true;
                break;
            }
        }
        if !warmed {
            return 0.0;
        }
        let mut min = f64::INFINITY;
        let mut prev_zero = false;
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let ops = tick().ops_per_sec;
            if ops == 0.0 {
                if prev_zero {
                    return 0.0; // two ticks running with no output → a genuine stall
                }
                prev_zero = true;
                continue;
            }
            prev_zero = false;
            min = min.min(ops);
        }
        min
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn http_engine_sustains_throughput_rust() {
        let mut engine = HttpServingEngine::new(1, 4, Guest::Rust);
        let min = sustained_throughput(|| engine.tick()).await;
        assert!(
            min > 0.0,
            "per-request HTTP (RS) sustained throughput (min {min:.0}/s)"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn http_engine_sustains_throughput_ts() {
        let mut engine = HttpServingEngine::new(1, 4, Guest::Ts);
        let min = sustained_throughput(|| engine.tick()).await;
        assert!(
            min > 0.0,
            "per-request HTTP (TS) sustained throughput (min {min:.0}/s)"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ws_engine_sustains_throughput_rust() {
        let mut engine = CapacityServingEngine::new(1, 4, CapacityKind::Ws, Guest::Rust);
        let min = sustained_throughput(|| engine.tick()).await;
        assert!(
            min > 0.0,
            "per-request WS echo (RS) sustained round-trips (min {min:.0}/s)"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ws_engine_sustains_throughput_ts() {
        let mut engine = CapacityServingEngine::new(1, 4, CapacityKind::Ws, Guest::Ts);
        let min = sustained_throughput(|| engine.tick()).await;
        assert!(
            min > 0.0,
            "per-request WS echo (TS) sustained round-trips (min {min:.0}/s)"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sse_engine_sustains_throughput_rust() {
        // The exact regression that bit us: a finite SSE burst held as an
        // infinite stream collapses to 0. The sustained window must stay nonzero.
        let mut engine = CapacityServingEngine::new(1, 4, CapacityKind::Sse, Guest::Rust);
        let min = sustained_throughput(|| engine.tick()).await;
        assert!(
            min > 0.0,
            "per-request SSE (RS) sustained events (min {min:.0}/s)"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sse_engine_sustains_throughput_ts() {
        let mut engine = CapacityServingEngine::new(1, 4, CapacityKind::Sse, Guest::Ts);
        let min = sustained_throughput(|| engine.tick()).await;
        assert!(
            min > 0.0,
            "per-request SSE (TS) sustained events (min {min:.0}/s)"
        );
    }
}
