//! **WebSocket stress benchmark** — hold many concurrent WS connections against the
//! RUSM host echo and hammer each with echo round-trips. Reports messages/sec,
//! round-trip p50/p99, and how many of the connections stayed up — the stability
//! story (each connection is an isolated supervised task; one dropping never
//! touches the others or the listener).
//!
//! ```sh
//! cargo run --release -p rusm-bench --example ws_bench -- [seconds] [connections]
//! ```

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use rusm_wasm::serve_ws_echo;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secs: u64 = arg(1).unwrap_or(5);
    let connections: usize = arg(2).unwrap_or(256);
    println!("WebSocket stress: {connections} concurrent connections, {secs}s\n");

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let server = tokio::spawn(serve_ws_echo(listener));
    let url = format!("ws://{addr}/");

    let stop = Arc::new(AtomicBool::new(false));
    let total = Arc::new(AtomicU64::new(0));
    let alive = Arc::new(AtomicU64::new(0));
    let latencies = Arc::new(Mutex::new(Vec::<u64>::new()));

    let clients: Vec<_> = (0..connections)
        .map(|_| {
            let (url, stop, total, alive, latencies) = (
                url.clone(),
                stop.clone(),
                total.clone(),
                alive.clone(),
                latencies.clone(),
            );
            tokio::spawn(async move {
                let Ok((mut ws, _)) = tokio_tungstenite::connect_async(url).await else {
                    return;
                };
                alive.fetch_add(1, Ordering::Relaxed);
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
                    total.fetch_add(1, Ordering::Relaxed);
                    if n.is_multiple_of(32) {
                        latencies
                            .lock()
                            .unwrap()
                            .push(started.elapsed().as_nanos() as u64);
                    }
                    n += 1;
                }
            })
        })
        .collect();

    let start = Instant::now();
    tokio::time::sleep(Duration::from_secs(secs)).await;
    let still_up = alive.load(Ordering::Relaxed);
    stop.store(true, Ordering::Relaxed);
    for client in clients {
        let _ = client.await;
    }
    server.abort();

    let elapsed = start.elapsed().as_secs_f64();
    let rate = total.load(Ordering::Relaxed) as f64 / elapsed;
    let mut lat = latencies.lock().unwrap().clone();
    lat.sort_unstable();
    let pct = |p: f64| -> f64 {
        if lat.is_empty() {
            0.0
        } else {
            lat[((lat.len() - 1) as f64 * p) as usize] as f64 / 1000.0
        }
    };

    println!("connections held: {still_up}/{connections}   (each an isolated supervised task)");
    println!(
        "echo round-trips:  {:.0}/sec   p50 {:.1}µs  p99 {:.1}µs",
        rate,
        pct(0.50),
        pct(0.99)
    );
    Ok(())
}

fn arg<T: std::str::FromStr>(n: usize) -> Option<T> {
    std::env::args().nth(n).and_then(|s| s.parse().ok())
}
