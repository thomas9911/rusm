use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use rusm_otp::Runtime;
use rusm_wasm::{CapabilityProfile, WasmRuntime};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::sample::Sample;
use crate::scenario::Guest;

/// Most latency samples surfaced in a single tick.
const LATENCY_SAMPLE: usize = 64;
/// Sample one round-trip's latency every Nth, per client.
const LATENCY_EVERY: u64 = 32;

/// The Rust WS-handler component: echoes each frame from inside the sandbox (the
/// same fixture the rusm-wasm WS serve test uses).
const WS_ECHO: &[u8] = include_bytes!("../../../crates/rusm-wasm/tests/fixtures/rs_ws_echo.wasm");
/// The TypeScript WS-handler bundle (a worker on the js-runner), same echo behavior.
const TS_WS_ECHO: &str = include_str!("../../../crates/rusm-wasm/tests/fixtures/ts_ws_echo.js");

/// A **real** WebSocket echo storm: each connection is served by its own sandboxed
/// WASM **component process** (`WasmRuntime::ws_server`) — inbound frame → mailbox
/// message, reply via a Wasm-free writer process that owns the socket sink — while a
/// pool of clients hammers it with echo round-trips. [`tick`](Self::tick) samples
/// round-trips/sec and round-trip latency.
///
/// Must be constructed inside a Tokio runtime (it binds a listener and spawns tasks).
pub struct WsEchoEngine {
    // Held alive for the run; dropping it stops the server's epoch ticker.
    _wr: WasmRuntime,
    echoed: Arc<AtomicU64>,
    /// Connections that have actually established (and are still up) — the *real*
    /// concurrency, so the dashboard reports truth, not the configured target.
    alive: Arc<AtomicU64>,
    latency_rx: UnboundedReceiver<u64>,
    stop: Arc<AtomicBool>,
    client_tasks: Vec<JoinHandle<()>>,
    server_task: JoinHandle<()>,
    last_echoed: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl WsEchoEngine {
    pub fn new(workers: usize, scheduler_count: usize, guest: Guest) -> Self {
        // Hold a *serious* number of concurrent connections — each its own sandboxed
        // component process. Scaled by the resource profile (via `workers`), not the
        // tiny spawn-worker count itself.
        let connections = (workers * 128).clamp(64, 768);

        let wr = WasmRuntime::new(Runtime::new()).expect("wasm runtime");
        let caps = CapabilityProfile::Trusted.capabilities();
        // Same server, either guest: a Rust actor component, or a TS worker bundle on
        // the embedded js-runner (one instance per connection).
        let server = match guest {
            Guest::Rust => {
                let prepared = wr
                    .prepare_component(
                        &wr.compile_component(WS_ECHO).expect("compile ws echo"),
                        "run",
                    )
                    .expect("prepare ws component");
                wr.ws_server(&prepared, caps)
            }
            Guest::Ts => wr.ws_server_js(TS_WS_ECHO.as_bytes().to_vec(), caps),
        };

        // Bind via std then adopt, so `new` stays synchronous like the other engines.
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        std_listener
            .set_nonblocking(true)
            .expect("listener non-blocking");
        let addr = std_listener.local_addr().expect("listener addr");
        let listener = TcpListener::from_std(std_listener).expect("adopt listener");
        let server_task = tokio::spawn(server.serve(listener));

        let echoed = Arc::new(AtomicU64::new(0));
        let alive = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (latency_tx, latency_rx) = unbounded_channel();
        let url = format!("ws://{addr}/");

        let client_tasks = (0..connections)
            .map(|_| {
                let echoed = Arc::clone(&echoed);
                let alive = Arc::clone(&alive);
                let stop = Arc::clone(&stop);
                let latency_tx = latency_tx.clone();
                let url = url.clone();
                tokio::spawn(async move {
                    // Connect the TCP socket ourselves so we can RST on close (no
                    // TIME_WAIT) — otherwise rapid run/stop cycles exhaust ephemeral
                    // ports and the next run can't connect. Then run the WS handshake.
                    let Ok(tcp) = TcpStream::connect(addr).await else {
                        return;
                    };
                    let _ = socket2::SockRef::from(&tcp).set_linger(Some(Duration::ZERO));
                    let Ok((mut ws, _)) = tokio_tungstenite::client_async(&url, tcp).await else {
                        return;
                    };
                    alive.fetch_add(1, Ordering::Relaxed); // counted only once connected
                    let mut n = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        let started = Instant::now();
                        if ws.send(Message::binary(b"ping".as_slice())).await.is_err() {
                            break;
                        }
                        match ws.next().await {
                            Some(Ok(_)) => {}
                            _ => break,
                        }
                        echoed.fetch_add(1, Ordering::Relaxed);
                        if n.is_multiple_of(LATENCY_EVERY) {
                            let _ = latency_tx.send(started.elapsed().as_nanos() as u64);
                        }
                        n += 1;
                    }
                    alive.fetch_sub(1, Ordering::Relaxed);
                })
            })
            .collect();

        Self {
            _wr: wr,
            echoed,
            alive,
            latency_rx,
            stop,
            client_tasks,
            server_task,
            last_echoed: 0,
            last_at: Instant::now(),
            scheduler_count,
        }
    }

    pub fn tick(&mut self) -> Sample {
        let now = Instant::now();
        let echoed = self.echoed.load(Ordering::Relaxed);
        let dt = now
            .duration_since(self.last_at)
            .as_secs_f64()
            .max(f64::MIN_POSITIVE);
        let ops_per_sec = echoed.saturating_sub(self.last_echoed) as f64 / dt;
        self.last_echoed = echoed;
        self.last_at = now;

        let mut latencies_ns = Vec::new();
        while let Ok(ns) = self.latency_rx.try_recv() {
            latencies_ns.push(ns);
        }
        if latencies_ns.len() > LATENCY_SAMPLE {
            latencies_ns = latencies_ns.split_off(latencies_ns.len() - LATENCY_SAMPLE);
        }

        // The charted concurrency is the live WS connections actually established
        // (each backed by a component process + a writer process).
        let process_count = self.alive.load(Ordering::Relaxed);
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

impl Drop for WsEchoEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.server_task.abort();
        for task in &self.client_tasks {
            task.abort();
        }
        // Kill every per-connection component + writer process — they're parked on
        // `receive`, so without this they'd leak past the engine and hold pool slots
        // (starving the next run). The runtime is this engine's alone, so this is safe.
        self._wr.shutdown();
        // `_wr` drops here → its epoch ticker thread stops.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_wasm_component_echoes_websockets_under_load() {
        let _serial = crate::SERVING_TEST_GUARD.lock().await;
        let mut engine = WsEchoEngine::new(1, 4, Guest::Rust);
        // Poll until echoes are flowing (each connection is a component process).
        let mut sample = engine.tick();
        for _ in 0..400 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            sample = engine.tick();
            if sample.ops_per_sec > 0.0 && !sample.latencies_ns.is_empty() {
                break;
            }
        }
        assert!(sample.ops_per_sec > 0.0, "the WASM WS server echoed frames");
        assert!(
            !sample.latencies_ns.is_empty(),
            "round-trip latency sampled"
        );
        assert_eq!(sample.scheduler_load.len(), 4);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_typescript_worker_echoes_websockets() {
        let _serial = crate::SERVING_TEST_GUARD.lock().await;
        // The TS path: each connection is a TypeScript worker on the js-runner.
        let mut engine = WsEchoEngine::new(1, 4, Guest::Ts);
        let mut sample = engine.tick();
        for _ in 0..800 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            sample = engine.tick();
            if sample.ops_per_sec > 0.0 {
                break;
            }
        }
        assert!(sample.ops_per_sec > 0.0, "the TS WS worker echoed frames");
    }
}
