//! Live dashboard engines for the serving scenarios (HTTP / WS / SSE, Rust + TS).
//!
//! Each engine spins up the **real** in-process server (`WasmRuntime::http_server` /
//! `ws_server` and the `*_js` variants) on a loopback port and drives it through the
//! shared [`rusm_loadtest`] load path — **balter** for HTTP request rate, the
//! connection-capacity harness for WS/SSE held connections. The dashboard tile reads
//! the achieved rate live each tick.
//!
//! This is a **co-resident live demo** (load + server share the node process), so the
//! *fair* headline numbers come from the out-of-process `rusm-loadtest` binary against
//! a real `rusm serve` port — not from these tiles. The engines exist to show the
//! serving path working live; they never leak (balter runs in bounded windows that
//! complete, and the capacity harness stops on drop).

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

/// A lean Rust `wasi:http` component (the host's HTTP ceiling fixture).
const HTTP_LEAN: &[u8] = include_bytes!("../../../crates/rusm-wasm/tests/fixtures/http_lean.wasm");
/// The TypeScript HTTP handler bundle (runs on the js-http-runner).
const TS_HELLO: &str = include_str!("../../../crates/rusm-wasm/tests/fixtures/ts_http_hello.js");
/// A Rust WS-echo component (echoes each frame from inside the sandbox).
const RS_WS_ECHO: &[u8] =
    include_bytes!("../../../crates/rusm-wasm/tests/fixtures/rs_ws_echo.wasm");
/// The TypeScript WS-echo worker bundle.
const TS_WS_ECHO: &str = include_str!("../../../crates/rusm-wasm/tests/fixtures/ts_ws_echo.js");
/// An endless Rust `wasi:http` SSE stream.
const FIREHOSE: &[u8] =
    include_bytes!("../../../crates/rusm-wasm/tests/fixtures/sse_firehose.wasm");
/// The TypeScript endless-SSE bundle.
const TS_FIREHOSE: &str =
    include_str!("../../../crates/rusm-wasm/tests/fixtures/ts_sse_firehose.js");

/// At most this many latency samples surfaced per tick.
const LATENCY_SAMPLE: usize = 64;
/// Each balter window completes before the next, so its workers shut down cleanly
/// (balter only aborts workers on completion, never on drop) — no task leak.
const HTTP_WINDOW: Duration = Duration::from_secs(3);

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

/// **HTTP throughput** (Rust or TS), driven live by **balter**: a real `wasi:http`
/// server hosted in-process, hammered through bounded balter windows at a steady
/// offered rate. The tile charts the achieved req/s and request latency.
pub struct HttpServingEngine {
    // Held for the run; dropping it stops the server's epoch ticker + reclaims processes.
    _wr: WasmRuntime,
    server_task: JoinHandle<()>,
    stop: Arc<AtomicBool>,
    lat_rx: UnboundedReceiver<u64>,
    concurrency: usize,
    last_achieved: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl HttpServingEngine {
    pub fn new(workers: usize, scheduler_count: usize, guest: Guest) -> Self {
        let wr = WasmRuntime::new(Runtime::new()).expect("wasm runtime");
        // Trusted's resources (roomy heap) but no inherited stdio: serving guests
        // don't print, and this keeps their on-disconnect traps (isolated → Crashed)
        // off the node's stderr instead of flooding it at connection teardown.
        let caps = CapabilityProfile::Trusted
            .capabilities()
            .inherit_stdio(false);
        let server = match guest {
            Guest::Rust => {
                let prepared = wr
                    .prepare_http(&wr.compile_component(HTTP_LEAN).expect("compile http_lean"))
                    .expect("prepare http component");
                wr.http_server(&prepared, caps)
            }
            Guest::Ts => wr.http_server_js(TS_HELLO, caps),
        };
        let (addr, listener) = bind_loopback();
        let server_task = tokio::spawn(server.serve(listener));

        // A steady, sustainable offered rate scaled by the resource profile; balter
        // paces it and seeds enough concurrency to hit it without a long ramp.
        let target_tps = (workers as u32 * 5_000).clamp(8_000, 50_000);
        let concurrency = (workers * 12).clamp(24, 256);

        rusm_loadtest::http::set_target(format!("http://{addr}/"));
        rusm_loadtest::http::reset_counter();
        let lat_rx = rusm_loadtest::http::install_latency_sink();

        let stop = Arc::new(AtomicBool::new(false));
        // Bounded-window loop: each window completes (clean balter shutdown), then we
        // re-check `stop`. Detached on purpose — we never abort it mid-window (that
        // would leak balter's workers); it self-exits within one window of `stop`.
        let loop_stop = Arc::clone(&stop);
        tokio::spawn(async move {
            while !loop_stop.load(Ordering::Relaxed) {
                let _ = rusm_loadtest::http::run_window(target_tps, concurrency, HTTP_WINDOW).await;
            }
        });

        Self {
            _wr: wr,
            server_task,
            stop,
            lat_rx,
            concurrency,
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

        // The charted "concurrency" is balter's offered in-flight count (the live
        // sandboxed instances handling those requests are transient, per request).
        let process_count = self.concurrency as u64;
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
        // Reclaim any in-flight per-request instances so none linger into the next run.
        self._wr.shutdown();
    }
}

/// Which connection-capacity workload a [`CapacityServingEngine`] serves.
#[derive(Clone, Copy)]
pub enum CapacityKind {
    Ws,
    Sse,
}

/// **WS echo** or **SSE fan-out** (Rust or TS): the real component server hosted
/// in-process, with many held connections driven by the shared capacity harness.
/// The tile charts round-trips/sec (WS) or events/sec (SSE) and live concurrency.
pub struct CapacityServingEngine {
    _wr: WasmRuntime,
    server_task: JoinHandle<()>,
    load: CapacityLoad,
    last_ops: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl CapacityServingEngine {
    pub fn new(workers: usize, scheduler_count: usize, kind: CapacityKind, guest: Guest) -> Self {
        let wr = WasmRuntime::new(Runtime::new()).expect("wasm runtime");
        // Trusted's resources (roomy heap) but no inherited stdio: serving guests
        // don't print, and this keeps their on-disconnect traps (isolated → Crashed)
        // off the node's stderr instead of flooding it at connection teardown.
        let caps = CapabilityProfile::Trusted
            .capabilities()
            .inherit_stdio(false);
        let (addr, listener) = bind_loopback();

        let (server_task, url, proto) = match kind {
            CapacityKind::Ws => {
                let server = match guest {
                    Guest::Rust => {
                        let prepared = wr
                            .prepare_component(
                                &wr.compile_component(RS_WS_ECHO).expect("compile ws echo"),
                                "run",
                            )
                            .expect("prepare ws component");
                        wr.ws_server(&prepared, caps)
                    }
                    Guest::Ts => wr.ws_server_js(TS_WS_ECHO.as_bytes().to_vec(), caps),
                };
                (
                    tokio::spawn(server.serve(listener)),
                    format!("ws://{addr}/"),
                    Protocol::Ws,
                )
            }
            CapacityKind::Sse => {
                let server = match guest {
                    Guest::Rust => {
                        let prepared = wr
                            .prepare_http(&wr.compile_component(FIREHOSE).expect("compile sse"))
                            .expect("prepare sse component");
                        wr.http_server(&prepared, caps)
                    }
                    Guest::Ts => wr.http_server_js(TS_FIREHOSE, caps),
                };
                (
                    tokio::spawn(server.serve(listener)),
                    format!("http://{addr}/"),
                    Protocol::Sse,
                )
            }
        };

        let connections = (workers * 128).clamp(64, 768);
        let load = CapacityLoad::start(proto, url, connections);

        Self {
            _wr: wr,
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

        let process_count = self.load.alive();
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
            "balter-driven HTTP produced throughput (max {max:.0}/s)"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ws_engine_echoes_under_held_connections() {
        let mut engine = CapacityServingEngine::new(1, 4, CapacityKind::Ws, Guest::Rust);
        let max = until_throughput(|| engine.tick()).await;
        assert!(max > 0.0, "WS echo produced round-trips (max {max:.0}/s)");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sse_engine_streams_events() {
        let mut engine = CapacityServingEngine::new(1, 4, CapacityKind::Sse, Guest::Rust);
        let max = until_throughput(|| engine.tick()).await;
        assert!(max > 0.0, "SSE produced events (max {max:.0}/s)");
    }
}
