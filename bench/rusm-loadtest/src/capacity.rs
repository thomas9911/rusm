//! Connection-capacity load for WS and SSE: hold `--connections` long-lived
//! connections open at once and measure the *sustained* work they carry — echo
//! round-trips (WS) or events drained (SSE). This is the honest metric for these
//! workloads (held connections + throughput + latency), which a request-rate model
//! would misrepresent as connection churn.
//!
//! Connections are opened once and kept for the whole run, so there is no
//! open/close storm and no ephemeral-port / TIME_WAIT pressure — the driver simply
//! holds them and counts. Throughput is reported live each second; latency
//! percentiles are computed from sampled observations at the end.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
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

pub fn run(proto: Protocol, opts: Opts, url: String) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(drive(proto, opts, url));
}

async fn drive(proto: Protocol, opts: Opts, url: String) {
    println!(
        "{}: holding {} connection(s) for {:?} → {url}",
        proto.label(),
        opts.connections,
        opts.duration
    );
    let shared = Arc::new(Shared {
        ops: AtomicU64::new(0),
        alive: AtomicU64::new(0),
        stop: AtomicBool::new(false),
    });
    let (lat_tx, lat_rx) = unbounded_channel();

    let workers: Vec<_> = (0..opts.connections)
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
    drop(lat_tx); // workers hold the only senders now

    report(proto, &shared, opts.duration).await;
    // Capture the live connection count *before* stopping — workers decrement it as
    // they exit, so reading it after the join would always show zero.
    let held = shared.alive.load(Ordering::Relaxed);
    shared.stop.store(true, Ordering::Relaxed);
    for w in workers {
        let _ = w.await;
    }
    summary(proto, &shared, held, lat_rx, opts.duration);
}

/// Live per-second throughput, then return after `duration`.
async fn report(proto: Protocol, shared: &Arc<Shared>, duration: Duration) {
    let start = Instant::now();
    let mut last = 0u64;
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.tick().await; // fire immediately; skip the zero sample
    while start.elapsed() < duration {
        interval.tick().await;
        let total = shared.ops.load(Ordering::Relaxed);
        let alive = shared.alive.load(Ordering::Relaxed);
        println!(
            "[t={:>2}s] {}: {} conns · {} {}/s",
            start.elapsed().as_secs(),
            proto.label(),
            alive,
            with_commas(total - last),
            proto.unit(),
        );
        last = total;
    }
}

fn summary(
    proto: Protocol,
    shared: &Arc<Shared>,
    held: u64,
    mut lat_rx: UnboundedReceiver<u64>,
    dur: Duration,
) {
    let total = shared.ops.load(Ordering::Relaxed);
    let mut samples = Vec::new();
    while let Ok(ns) = lat_rx.try_recv() {
        samples.push(ns);
    }
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
        with_commas((total as f64 / dur.as_secs_f64()) as u64),
        proto.unit()
    );
    println!(
        "  {} latency  p50 {:?} · p99 {:?}",
        proto.latency_kind(),
        pct(0.50),
        pct(0.99)
    );
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
    for _ in 0..50 {
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

/// One SSE stream: GET the endpoint and drain `text/event-stream` events forever,
/// counting `\n\n` frame terminators and sampling the inter-event gap.
async fn sse_connection(shared: Arc<Shared>, url: String, lat_tx: UnboundedSender<u64>) {
    let resp = match reqwest::Client::new().get(&url).send().await {
        Ok(resp) => resp,
        Err(_) => return,
    };
    shared.alive.fetch_add(1, Ordering::Relaxed);
    let mut stream = resp.bytes_stream();
    let mut prev_newline = false;
    let mut n = 0u64;
    let mut last_event = Instant::now();
    while !shared.stop.load(Ordering::Relaxed) {
        let Some(Ok(chunk)) = stream.next().await else {
            break;
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

    #[test]
    fn commas_group_thousands() {
        assert_eq!(with_commas(0), "0");
        assert_eq!(with_commas(192_344), "192,344");
        assert_eq!(with_commas(1_500_000), "1,500,000");
    }
}
