//! Connection-capacity load for WS and SSE: hold many long-lived connections open
//! at once and measure the *sustained* work they carry — echo round-trips (WS) or
//! events drained (SSE). This is the honest metric for these workloads (held
//! connections + throughput + latency), which a request-rate model would
//! misrepresent as connection churn.
//!
//! Connections are opened once and kept for the whole run, so there is no
//! open/close storm and no ephemeral-port / TIME_WAIT pressure — the driver simply
//! holds them and counts.
//!
//! [`CapacityLoad`] is the reusable handle: [`start`](CapacityLoad::start) spawns
//! the held connections and exposes live counters, so both the CLI ([`run`]) and the
//! dashboard's serving engines drive the *same* load path. Dropping it (or calling
//! [`stop`](CapacityLoad::stop)) sets a flag every worker checks, so no task leaks.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::Opts;

#[derive(Clone, Copy)]
pub enum Protocol {
    Ws,
    Sse,
}

impl Protocol {
    fn label(self) -> &'static str {
        match self {
            Protocol::Ws => "ws",
            Protocol::Sse => "sse",
        }
    }
    /// What one counted unit is, and what a latency sample measures.
    fn unit(self) -> &'static str {
        match self {
            Protocol::Ws => "round-trips",
            Protocol::Sse => "events",
        }
    }
    fn latency_kind(self) -> &'static str {
        match self {
            Protocol::Ws => "round-trip",
            Protocol::Sse => "inter-event",
        }
    }
}

/// Sample one latency observation every Nth, per connection — enough for stable
/// percentiles without unbounded memory on a high-rate stream.
const LATENCY_EVERY: u64 = 64;

struct Shared {
    ops: AtomicU64,
    alive: AtomicU64,
    stop: AtomicBool,
}

/// A running connection-capacity load: holds the worker tasks and live counters.
/// Stops cleanly on drop (every worker polls the shared stop flag), so it never
/// leaks tasks — drive it live (read [`ops`](Self::ops)/[`alive`](Self::alive) per
/// tick) or one-shot via [`run`].
pub struct CapacityLoad {
    shared: Arc<Shared>,
    lat_rx: UnboundedReceiver<u64>,
    // Held so the tasks live as long as the load; they also self-exit on `stop`.
    workers: Vec<JoinHandle<()>>,
}

impl CapacityLoad {
    /// Spawns `connections` held connections against `url`. Must be called inside a
    /// Tokio runtime (it spawns tasks).
    pub fn start(proto: Protocol, url: String, connections: usize) -> Self {
        let shared = Arc::new(Shared {
            ops: AtomicU64::new(0),
            alive: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        });
        let (lat_tx, lat_rx) = unbounded_channel();
        let workers = (0..connections)
            .map(|_| {
                let shared = Arc::clone(&shared);
                let url = url.clone();
                let lat_tx = lat_tx.clone();
                match proto {
                    Protocol::Ws => tokio::spawn(ws_connection(shared, url, lat_tx)),
                    Protocol::Sse => tokio::spawn(sse_connection(shared, url, lat_tx)),
                }
            })
            .collect();
        // The handle keeps no sender, so `lat_rx` closes once every worker exits.
        Self {
            shared,
            lat_rx,
            workers,
        }
    }

    /// Operations counted so far (round-trips for WS, events for SSE).
    pub fn ops(&self) -> u64 {
        self.shared.ops.load(Ordering::Relaxed)
    }

    /// Connections currently established and live.
    pub fn alive(&self) -> u64 {
        self.shared.alive.load(Ordering::Relaxed)
    }

    /// Drains the latency samples observed since the last call (nanoseconds).
    pub fn drain_latencies(&mut self) -> Vec<u64> {
        let mut out = Vec::new();
        while let Ok(ns) = self.lat_rx.try_recv() {
            out.push(ns);
        }
        out
    }

    /// Signals every worker to stop; they exit on their next poll of the flag.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for CapacityLoad {
    fn drop(&mut self) {
        self.stop();
        // Workers observe the flag and return; abort as a backstop so nothing lingers.
        for w in &self.workers {
            w.abort();
        }
    }
}

/// CLI one-shot: hold `connections` for `duration`, print live throughput, then a
/// final summary with latency percentiles.
pub fn run(proto: Protocol, opts: Opts, url: String) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        println!(
            "{}: holding {} connection(s) for {:?} → {url}",
            proto.label(),
            opts.connections,
            opts.duration
        );
        let mut load = CapacityLoad::start(proto, url, opts.connections);

        // Live per-second throughput for the run's duration.
        let start = Instant::now();
        let mut last = 0u64;
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.tick().await; // fire immediately; skip the zero sample
        while start.elapsed() < opts.duration {
            interval.tick().await;
            let total = load.ops();
            println!(
                "[t={:>2}s] {}: {} conns · {} {}/s",
                start.elapsed().as_secs(),
                proto.label(),
                load.alive(),
                with_commas(total - last),
                proto.unit(),
            );
            last = total;
        }

        // Capture the held count before stopping (workers decrement `alive` as they exit).
        let held = load.alive();
        let total = load.ops();
        let mut samples = load.drain_latencies();
        load.stop();

        samples.sort_unstable();
        let pct = |q: f64| -> Duration {
            if samples.is_empty() {
                return Duration::ZERO;
            }
            let idx = ((samples.len() as f64 * q) as usize).min(samples.len() - 1);
            Duration::from_nanos(samples[idx])
        };
        println!("\n── result ── {} ({held} held)", proto.label());
        println!("  total            {} {}", with_commas(total), proto.unit());
        println!(
            "  sustained        {} {}/s",
            with_commas((total as f64 / opts.duration.as_secs_f64()) as u64),
            proto.unit()
        );
        println!(
            "  {} latency  p50 {:?} · p99 {:?}",
            proto.latency_kind(),
            pct(0.50),
            pct(0.99)
        );
    });
}

/// One WebSocket connection: ping → wait for echo, forever, sampling round-trips.
async fn ws_connection(shared: Arc<Shared>, url: String, lat_tx: UnboundedSender<u64>) {
    let Some(mut ws) = connect_ws(&url, &shared).await else {
        return;
    };
    shared.alive.fetch_add(1, Ordering::Relaxed);
    let mut n = 0u64;
    while !shared.stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        if ws.send(Message::binary(b"ping".as_slice())).await.is_err() {
            break;
        }
        match ws.next().await {
            Some(Ok(_)) => {}
            _ => break,
        }
        shared.ops.fetch_add(1, Ordering::Relaxed);
        if n % LATENCY_EVERY == 0 {
            let _ = lat_tx.send(started.elapsed().as_nanos() as u64);
        }
        n += 1;
    }
    shared.alive.fetch_sub(1, Ordering::Relaxed);
}

/// Connect a WS client, retrying transient failures during the initial ramp.
async fn connect_ws(
    url: &str,
    shared: &Arc<Shared>,
) -> Option<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    // A generous budget: hundreds of simultaneous handshakes against a small
    // resident pool (especially the heavier TS js-runner) serialize, so a short
    // budget would leave many connections unestablished and the held count short.
    for _ in 0..600 {
        if shared.stop.load(Ordering::Relaxed) {
            return None;
        }
        match tokio_tungstenite::connect_async(url).await {
            Ok((ws, _)) => return Some(ws),
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    None
}

/// One SSE slot: keep an `event-stream` draining, counting `\n\n` frame terminators
/// and sampling the inter-event gap. **Reconnects when a stream ends** — an infinite
/// firehose never ends (so this never re-loops), but a resident handler may emit a
/// finite burst per request, and reusing the kept-alive connection keeps the slot
/// busy without churn. The slot stays "alive" for its whole lifetime.
async fn sse_connection(shared: Arc<Shared>, url: String, lat_tx: UnboundedSender<u64>) {
    let client = reqwest::Client::new();
    shared.alive.fetch_add(1, Ordering::Relaxed);
    let mut n = 0u64;
    while !shared.stop.load(Ordering::Relaxed) {
        let Ok(resp) = client.get(&url).send().await else {
            tokio::time::sleep(Duration::from_millis(5)).await;
            continue;
        };
        let mut stream = resp.bytes_stream();
        let mut prev_newline = false;
        // Per connection, so a reconnect gap isn't mis-sampled as inter-event latency.
        let mut last_event = Instant::now();
        while !shared.stop.load(Ordering::Relaxed) {
            let Some(Ok(chunk)) = stream.next().await else {
                break; // stream ended (finite resident burst) → reconnect
            };
            for &b in chunk.iter() {
                if b == b'\n' {
                    if prev_newline {
                        shared.ops.fetch_add(1, Ordering::Relaxed);
                        if n % LATENCY_EVERY == 0 {
                            let now = Instant::now();
                            let _ = lat_tx.send(now.duration_since(last_event).as_nanos() as u64);
                            last_event = now;
                        }
                        n += 1;
                        prev_newline = false;
                    } else {
                        prev_newline = true;
                    }
                } else {
                    prev_newline = false;
                }
            }
        }
    }
    shared.alive.fetch_sub(1, Ordering::Relaxed);
}

/// Group thousands with commas for readable throughput lines.
fn with_commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn commas_group_thousands() {
        assert_eq!(with_commas(0), "0");
        assert_eq!(with_commas(192_344), "192,344");
        assert_eq!(with_commas(1_500_000), "1,500,000");
    }

    /// A minimal `text/event-stream` server. Each GET gets `data:` frames; when
    /// `infinite`, it streams until the client disconnects, otherwise it writes a
    /// finite burst of `events` and **closes** — modelling a resident handler.
    async fn sse_server(infinite: bool, events: usize) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024]; // drain the request line + headers
                    let _ = sock.read(&mut buf).await;
                    if sock
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n")
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let mut n = 0usize;
                    while infinite || n < events {
                        if sock.write_all(b"data: tick\n\n").await.is_err() {
                            return; // client hung up
                        }
                        n += 1;
                        if infinite {
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                    }
                    // finite: drop `sock` here → close, so the client must reconnect.
                });
            }
        });
        addr
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn capacity_counts_an_infinite_sse_stream() {
        let addr = sse_server(true, 0).await;
        let load = CapacityLoad::start(Protocol::Sse, format!("http://{addr}/"), 4);
        tokio::time::sleep(Duration::from_millis(800)).await;
        let ops = load.ops();
        load.stop();
        assert!(ops > 100, "infinite SSE streams counted ({ops} events)");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn capacity_reconnects_a_finite_sse_burst_instead_of_collapsing() {
        // The exact regression: a finite burst (a few events, then close) must NOT
        // stall the slot — the harness reconnects, so the sustained count far exceeds
        // a single burst and slots stay alive. A non-reconnecting harness would count
        // ~`events * slots` once and then drop to 0 (the SSE-0 bug).
        let (events, slots) = (5usize, 4usize);
        let addr = sse_server(false, events).await;
        let load = CapacityLoad::start(Protocol::Sse, format!("http://{addr}/"), slots);
        tokio::time::sleep(Duration::from_millis(800)).await;
        let ops = load.ops();
        let alive = load.alive();
        load.stop();
        assert!(
            alive >= 1,
            "slots stay alive across reconnects (alive {alive})"
        );
        assert!(
            ops as usize > events * slots,
            "finite SSE reconnected past one burst ({ops} > {})",
            events * slots
        );
    }
}
