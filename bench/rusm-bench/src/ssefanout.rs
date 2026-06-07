use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rusm_otp::Runtime;
use rusm_wasm::{CapabilityProfile, WasmRuntime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::task::JoinHandle;

use crate::sample::Sample;

/// Most latency samples surfaced in a single tick.
const LATENCY_SAMPLE: usize = 64;
/// Sample one inter-event gap every Nth event, per stream.
const LATENCY_EVERY: u64 = 256;

/// An endless `wasi:http` SSE stream — yields events as fast as the client reads
/// (the same fixture the sse_bench example uses).
const FIREHOSE: &[u8] =
    include_bytes!("../../../crates/rusm-wasm/tests/fixtures/sse_firehose.wasm");

/// A **real** SSE fan-out: many long-lived `text/event-stream` connections, each
/// served by its own `wasi:http` component instance (`WasmRuntime::http_server`)
/// streaming events as fast as the client drains them. [`tick`](Self::tick) samples
/// events/sec and the inter-event cadence — the "many concurrent streaming
/// responses, all held" story. The event producer is the sandboxed guest; the host
/// only moves bytes.
///
/// Must be constructed inside a Tokio runtime (it binds a listener and spawns tasks).
pub struct SseFanoutEngine {
    // Held alive for the run; dropping it stops the server's epoch ticker.
    _wr: WasmRuntime,
    events: Arc<AtomicU64>,
    streams: usize,
    latency_rx: UnboundedReceiver<u64>,
    stop: Arc<AtomicBool>,
    client_tasks: Vec<JoinHandle<()>>,
    server_task: JoinHandle<()>,
    last_events: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl SseFanoutEngine {
    pub fn new(workers: usize, scheduler_count: usize) -> Self {
        let streams = workers.clamp(8, 256);

        let wr = WasmRuntime::new(Runtime::new()).expect("wasm runtime");
        let prepared = wr
            .prepare_http(
                &wr.compile_component(FIREHOSE)
                    .expect("compile sse firehose"),
            )
            .expect("prepare sse component");
        let server = wr.http_server(&prepared, CapabilityProfile::Trusted.capabilities());

        // Bind via std then adopt, so `new` stays synchronous like the other engines.
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        std_listener
            .set_nonblocking(true)
            .expect("listener non-blocking");
        let addr = std_listener.local_addr().expect("listener addr");
        let listener = TcpListener::from_std(std_listener).expect("adopt listener");
        let server_task = tokio::spawn(server.serve(listener));

        let events = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (latency_tx, latency_rx) = unbounded_channel();

        let client_tasks = (0..streams)
            .map(|_| {
                let events = Arc::clone(&events);
                let stop = Arc::clone(&stop);
                let latency_tx = latency_tx.clone();
                tokio::spawn(async move {
                    let Ok(mut conn) = TcpStream::connect(addr).await else {
                        return;
                    };
                    conn.set_nodelay(true).ok();
                    if conn
                        .write_all(b"GET / HTTP/1.1\r\nHost: rusm\r\n\r\n")
                        .await
                        .is_err()
                    {
                        return;
                    }
                    // Count events by their `\n\n` frame terminators, carrying the last
                    // byte across reads; sample the inter-event gap occasionally.
                    let mut buf = [0u8; 16 * 1024];
                    let mut prev_newline = false;
                    let mut seen = 0u64;
                    let mut last_event = Instant::now();
                    while !stop.load(Ordering::Relaxed) {
                        let Ok(n) = conn.read(&mut buf).await else {
                            break;
                        };
                        if n == 0 {
                            break;
                        }
                        let mut count = 0u64;
                        for &b in &buf[..n] {
                            if b == b'\n' {
                                if prev_newline {
                                    count += 1;
                                    prev_newline = false;
                                } else {
                                    prev_newline = true;
                                }
                            } else {
                                prev_newline = false;
                            }
                        }
                        if count > 0 {
                            events.fetch_add(count, Ordering::Relaxed);
                            seen += count;
                            if seen >= LATENCY_EVERY {
                                let now = Instant::now();
                                let gap = now.duration_since(last_event).as_nanos() as u64;
                                let _ = latency_tx.send(gap / seen.max(1));
                                last_event = now;
                                seen = 0;
                            }
                        }
                    }
                })
            })
            .collect();

        Self {
            _wr: wr,
            events,
            streams,
            latency_rx,
            stop,
            client_tasks,
            server_task,
            last_events: 0,
            last_at: Instant::now(),
            scheduler_count,
        }
    }

    pub fn tick(&mut self) -> Sample {
        let now = Instant::now();
        let events = self.events.load(Ordering::Relaxed);
        let dt = now
            .duration_since(self.last_at)
            .as_secs_f64()
            .max(f64::MIN_POSITIVE);
        let ops_per_sec = events.saturating_sub(self.last_events) as f64 / dt;
        self.last_events = events;
        self.last_at = now;

        let mut latencies_ns = Vec::new();
        while let Ok(ns) = self.latency_rx.try_recv() {
            latencies_ns.push(ns);
        }
        if latencies_ns.len() > LATENCY_SAMPLE {
            latencies_ns = latencies_ns.split_off(latencies_ns.len() - LATENCY_SAMPLE);
        }

        // The charted concurrency is the live SSE streams (each its own instance).
        let process_count = self.streams as u64;
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

impl Drop for SseFanoutEngine {
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
    async fn a_wasm_component_streams_events_to_many_subscribers() {
        let mut engine = SseFanoutEngine::new(8, 4);
        let mut sample = engine.tick();
        for _ in 0..400 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            sample = engine.tick();
            if sample.ops_per_sec > 0.0 {
                break;
            }
        }
        assert!(
            sample.ops_per_sec > 0.0,
            "the WASM SSE server streamed events"
        );
        assert_eq!(sample.process_count, 8, "eight concurrent streams held");
        assert_eq!(sample.scheduler_load.len(), 4);
    }
}
