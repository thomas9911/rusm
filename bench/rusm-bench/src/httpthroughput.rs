use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusm_otp::Runtime;
use rusm_wasm::{CapabilityProfile, WasmRuntime};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::task::JoinHandle;

use crate::sample::Sample;
use crate::scenario::Guest;

/// Most latency samples surfaced in a single tick.
const LATENCY_SAMPLE: usize = 64;
/// Sample one request's latency every Nth, per client.
const LATENCY_EVERY: u64 = 32;

/// A lean raw-`wasi:http` Rust component — the host's serving ceiling (no per-request
/// reactor). The same fixture `http_bench` measures.
const HTTP_LEAN: &[u8] = include_bytes!("../../../crates/rusm-wasm/tests/fixtures/http_lean.wasm");
/// The TypeScript HTTP handler bundle (a request→response handler on the
/// js-http-runner). Same answer, written in TS.
const TS_HELLO: &str = include_str!("../../../crates/rusm-wasm/tests/fixtures/ts_http_hello.js");

/// A **real** HTTP serving storm: a WASM component (`wstd` `wasi:http`) hosted by
/// `WasmRuntime::http_server` (hyper + wasmtime-wasi-http, one sandboxed instance
/// per request), hammered by a pool of keep-alive clients. [`tick`](Self::tick)
/// samples requests/sec and per-request latency; the response is produced **by the
/// guest**, the host only moves bytes.
///
/// Must be constructed inside a Tokio runtime (it binds a listener and spawns tasks).
pub struct HttpThroughputEngine {
    // Held alive for the run; dropping it stops the server's epoch ticker.
    _wr: WasmRuntime,
    served: Arc<AtomicU64>,
    /// Keep-alive clients that actually connected (and are still up) — the real
    /// concurrency, not the configured target, so the tile never lies.
    alive: Arc<AtomicU64>,
    latency_rx: UnboundedReceiver<u64>,
    stop: Arc<AtomicBool>,
    clients_tasks: Vec<JoinHandle<()>>,
    server_task: JoinHandle<()>,
    last_served: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl HttpThroughputEngine {
    pub fn new(workers: usize, scheduler_count: usize, guest: Guest) -> Self {
        // A serious number of keep-alive clients, scaled by the resource profile —
        // hundreds in flight, not the tiny spawn-worker count (which clamped to 8).
        let clients = (workers * 96).clamp(64, 512);

        let wr = WasmRuntime::new(Runtime::new()).expect("wasm runtime");
        let caps = CapabilityProfile::Trusted.capabilities();
        // Same server, either guest: a lean Rust wasi:http component, or a TS handler
        // bundle on the js-http-runner (instance-per-request for both).
        let server = match guest {
            Guest::Rust => {
                let prepared = wr
                    .prepare_http(&wr.compile_component(HTTP_LEAN).expect("compile http_lean"))
                    .expect("prepare http component");
                wr.http_server(&prepared, caps)
            }
            Guest::Ts => wr.http_server_js(TS_HELLO, caps),
        };

        // Bind via std then adopt, so `new` stays synchronous like the other engines.
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        std_listener
            .set_nonblocking(true)
            .expect("listener non-blocking");
        let addr = std_listener.local_addr().expect("listener addr");
        let listener = TcpListener::from_std(std_listener).expect("adopt listener");
        let server_task = tokio::spawn(server.serve(listener));

        let served = Arc::new(AtomicU64::new(0));
        let alive = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (latency_tx, latency_rx) = unbounded_channel();

        let clients_tasks = (0..clients)
            .map(|_| {
                let served = Arc::clone(&served);
                let alive = Arc::clone(&alive);
                let stop = Arc::clone(&stop);
                let latency_tx = latency_tx.clone();
                tokio::spawn(async move {
                    let Ok(conn) = TcpStream::connect(addr).await else {
                        return;
                    };
                    conn.set_nodelay(true).ok();
                    // RST on close (no TIME_WAIT) so rapid run/stop cycles don't pin
                    // ephemeral source ports and starve the next run's connects.
                    let _ = socket2::SockRef::from(&conn).set_linger(Some(Duration::ZERO));
                    alive.fetch_add(1, Ordering::Relaxed); // counted only once connected
                    let mut reader = BufReader::new(conn);
                    let mut n = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        let started = Instant::now();
                        if request(&mut reader).await.is_err() {
                            break;
                        }
                        served.fetch_add(1, Ordering::Relaxed);
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
            served,
            alive,
            latency_rx,
            stop,
            clients_tasks,
            server_task,
            last_served: 0,
            last_at: Instant::now(),
            scheduler_count,
        }
    }

    pub fn tick(&mut self) -> Sample {
        let now = Instant::now();
        let served = self.served.load(Ordering::Relaxed);
        let dt = now
            .duration_since(self.last_at)
            .as_secs_f64()
            .max(f64::MIN_POSITIVE);
        let ops_per_sec = served.saturating_sub(self.last_served) as f64 / dt;
        self.last_served = served;
        self.last_at = now;

        let mut latencies_ns = Vec::new();
        while let Ok(ns) = self.latency_rx.try_recv() {
            latencies_ns.push(ns);
        }
        if latencies_ns.len() > LATENCY_SAMPLE {
            latencies_ns = latencies_ns.split_off(latencies_ns.len() - LATENCY_SAMPLE);
        }

        // The charted "concurrency" is the keep-alive clients actually connected (each
        // request runs on a transient instance, not a long-lived process).
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

impl Drop for HttpThroughputEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.server_task.abort();
        for task in &self.clients_tasks {
            task.abort();
        }
        // Abort any in-flight per-request instances so none linger into the next run.
        self._wr.shutdown();
        // `_wr` drops here → its epoch ticker thread stops.
    }
}

/// One keep-alive HTTP/1.1 request + a full response read (Content-Length *or*
/// chunked), so the connection stays in sync for the next request.
async fn request(reader: &mut BufReader<TcpStream>) -> std::io::Result<()> {
    reader
        .get_mut()
        .write_all(b"GET / HTTP/1.1\r\nHost: rusm\r\n\r\n")
        .await?;

    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Err(eof());
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        let lower = header.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().ok();
        } else if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
    }

    if chunked {
        loop {
            line.clear();
            if reader.read_line(&mut line).await? == 0 {
                return Err(eof());
            }
            let size = usize::from_str_radix(line.trim_end(), 16).unwrap_or(0);
            let mut chunk = vec![0u8; size + 2]; // data + trailing CRLF
            reader.read_exact(&mut chunk).await?;
            if size == 0 {
                break;
            }
        }
    } else {
        let mut body = vec![0u8; content_length.unwrap_or(0)];
        reader.read_exact(&mut body).await?;
    }
    Ok(())
}

fn eof() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connection closed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_wasm_component_serves_requests_under_load() {
        let _serial = crate::SERVING_TEST_GUARD.lock().await;
        let mut engine = HttpThroughputEngine::new(1, 4, Guest::Rust);
        // Poll until requests are flowing (the component instantiates per request).
        let mut sample = engine.tick();
        for _ in 0..400 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            sample = engine.tick();
            if sample.ops_per_sec > 0.0 && !sample.latencies_ns.is_empty() {
                break;
            }
        }
        assert!(
            sample.ops_per_sec > 0.0,
            "the WASM HTTP server served requests"
        );
        assert!(
            !sample.latencies_ns.is_empty(),
            "request latency is sampled"
        );
        assert_eq!(sample.scheduler_load.len(), 4);
    }
}
