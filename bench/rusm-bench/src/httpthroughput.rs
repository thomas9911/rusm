use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rusm_otp::Runtime;
use rusm_wasm::{CapabilityProfile, WasmRuntime};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::task::JoinHandle;

use crate::sample::Sample;

/// Most latency samples surfaced in a single tick.
const LATENCY_SAMPLE: usize = 64;
/// Sample one request's latency every Nth, per client.
const LATENCY_EVERY: u64 = 32;

/// A minimal `wasi:http` component (built with `wstd`): answers every request, in
/// the guest, with `200 hello`. The same fixture the rusm-wasm HTTP serve test uses.
const HTTP_HELLO: &[u8] =
    include_bytes!("../../../crates/rusm-wasm/tests/fixtures/http_hello.wasm");

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
    clients: usize,
    latency_rx: UnboundedReceiver<u64>,
    stop: Arc<AtomicBool>,
    clients_tasks: Vec<JoinHandle<()>>,
    server_task: JoinHandle<()>,
    last_served: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl HttpThroughputEngine {
    pub fn new(workers: usize, scheduler_count: usize) -> Self {
        let clients = workers.clamp(8, 256);

        let wr = WasmRuntime::new(Runtime::new()).expect("wasm runtime");
        let prepared = wr
            .prepare_http(
                &wr.compile_component(HTTP_HELLO)
                    .expect("compile http_hello"),
            )
            .expect("prepare http component");
        let server = wr.http_server(&prepared, CapabilityProfile::Trusted.capabilities());

        // Bind via std then adopt, so `new` stays synchronous like the other engines.
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        std_listener
            .set_nonblocking(true)
            .expect("listener non-blocking");
        let addr = std_listener.local_addr().expect("listener addr");
        let listener = TcpListener::from_std(std_listener).expect("adopt listener");
        let server_task = tokio::spawn(server.serve(listener));

        let served = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (latency_tx, latency_rx) = unbounded_channel();

        let clients_tasks = (0..clients)
            .map(|_| {
                let served = Arc::clone(&served);
                let stop = Arc::clone(&stop);
                let latency_tx = latency_tx.clone();
                tokio::spawn(async move {
                    let Ok(conn) = TcpStream::connect(addr).await else {
                        return;
                    };
                    conn.set_nodelay(true).ok();
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
                })
            })
            .collect();

        Self {
            _wr: wr,
            served,
            clients,
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

        // The charted "concurrency" is the live keep-alive client connections (each
        // request runs on a transient instance, not a long-lived process).
        let process_count = self.clients as u64;
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
        let mut engine = HttpThroughputEngine::new(8, 4);
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
