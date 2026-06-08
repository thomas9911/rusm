use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures_util::{SinkExt, StreamExt};
use rusm_otp::Runtime;
use rusm_wasm::{CapabilityProfile, WasmRuntime};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::sample::Sample;

/// Most latency samples surfaced in a single tick.
const LATENCY_SAMPLE: usize = 64;
/// Sample one round-trip's latency every Nth, per client.
const LATENCY_EVERY: u64 = 32;

/// The WS-handler component: echoes each frame from inside the sandbox (the same
/// fixture the rusm-wasm WS serve test uses).
const WS_ECHO: &[u8] = include_bytes!("../../../crates/rusm-wasm/tests/fixtures/rs_ws_echo.wasm");

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
    connections: usize,
    latency_rx: UnboundedReceiver<u64>,
    stop: Arc<AtomicBool>,
    client_tasks: Vec<JoinHandle<()>>,
    server_task: JoinHandle<()>,
    last_echoed: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl WsEchoEngine {
    pub fn new(workers: usize, scheduler_count: usize) -> Self {
        // Hold a *visible* number of concurrent connections — each its own sandboxed
        // component process. Scaled by the resource profile (via `workers`), not the
        // tiny spawn-worker count itself.
        let connections = (workers * 64).clamp(64, 512);

        let wr = WasmRuntime::new(Runtime::new()).expect("wasm runtime");
        let prepared = wr
            .prepare_component(
                &wr.compile_component(WS_ECHO).expect("compile ws echo"),
                "run",
            )
            .expect("prepare ws component");
        let server = wr.ws_server(&prepared, CapabilityProfile::Trusted.capabilities());

        // Bind via std then adopt, so `new` stays synchronous like the other engines.
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        std_listener
            .set_nonblocking(true)
            .expect("listener non-blocking");
        let addr = std_listener.local_addr().expect("listener addr");
        let listener = TcpListener::from_std(std_listener).expect("adopt listener");
        let server_task = tokio::spawn(server.serve(listener));

        let echoed = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (latency_tx, latency_rx) = unbounded_channel();
        let url = format!("ws://{addr}/");

        let client_tasks = (0..connections)
            .map(|_| {
                let echoed = Arc::clone(&echoed);
                let stop = Arc::clone(&stop);
                let latency_tx = latency_tx.clone();
                let url = url.clone();
                tokio::spawn(async move {
                    let Ok((mut ws, _)) = tokio_tungstenite::connect_async(url).await else {
                        return;
                    };
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
                })
            })
            .collect();

        Self {
            _wr: wr,
            echoed,
            connections,
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

        // The charted concurrency is the live WS connections; each is a component
        // process + a writer process behind the scenes.
        let process_count = self.connections as u64;
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
        // `_wr` drops here → its epoch ticker thread stops.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_wasm_component_echoes_websockets_under_load() {
        let mut engine = WsEchoEngine::new(1, 4);
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
}
