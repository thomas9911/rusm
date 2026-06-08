//! Connection-establishment storm: how fast can a `rusm serve` node **accept new
//! connections**, each becoming its own sandboxed process? Out-of-process, against a
//! real WS port (the WS server spawns a component process *per connection*), so the
//! number earned here is "sandboxed-process-per-connection establishments/sec" — a
//! richer claim than a raw TCP accept, measured fairly from a separate process.
//!
//! `connections` concurrent slots each loop: connect → (RUSM spawns the per-connection
//! processes) → close → repeat. We count successful establishments per second.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::TcpStream;

use crate::Opts;

/// The `host:port` a `ws://host:port/...` URL connects to (we open the TCP socket
/// ourselves to set `SO_LINGER(0)`, so the churn doesn't accumulate TIME_WAIT).
fn host_port(url: &str) -> String {
    url.trim_start_matches("ws://")
        .trim_start_matches("wss://")
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}

/// CLI entry: churn connections for `opts.duration` across `opts.connections` slots,
/// printing live establishments/sec and a final summary.
pub fn run(opts: Opts, url: String) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        println!(
            "conn: churning connections across {} slots for {:?} → {url}",
            opts.connections, opts.duration
        );
        let established = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let addr = host_port(&url);
        let slots: Vec<_> = (0..opts.connections)
            .map(|_| {
                let established = Arc::clone(&established);
                let stop = Arc::clone(&stop);
                let url = url.clone();
                let addr = addr.clone();
                tokio::spawn(async move {
                    while !stop.load(Ordering::Relaxed) {
                        // Open the TCP ourselves so we can RST on close (SO_LINGER 0) —
                        // no TIME_WAIT, so churn doesn't starve the ephemeral pool.
                        let Ok(tcp) = TcpStream::connect(&addr).await else {
                            tokio::time::sleep(Duration::from_millis(2)).await;
                            continue;
                        };
                        let _ = socket2::SockRef::from(&tcp).set_linger(Some(Duration::ZERO));
                        match tokio_tungstenite::client_async(&url, tcp).await {
                            Ok((ws, _)) => {
                                established.fetch_add(1, Ordering::Relaxed);
                                drop(ws); // RST-close immediately — we measure the accept rate
                            }
                            Err(_) => tokio::time::sleep(Duration::from_millis(2)).await,
                        }
                    }
                })
            })
            .collect();

        // Live establishments/sec, tracking the peak.
        let start = Instant::now();
        let mut last = 0u64;
        let mut peak = 0u64;
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.tick().await;
        while start.elapsed() < opts.duration {
            interval.tick().await;
            let total = established.load(Ordering::Relaxed);
            let rate = total - last;
            peak = peak.max(rate);
            println!(
                "[t={:>2}s] conn: {} established/s",
                start.elapsed().as_secs(),
                rate
            );
            last = total;
        }
        stop.store(true, Ordering::Relaxed);
        for s in &slots {
            s.abort();
        }
        let total = established.load(Ordering::Relaxed);
        println!("\n── result ── {url}");
        println!("  peak             {peak} connections/s");
        println!(
            "  sustained        {} connections/s",
            total / opts.duration.as_secs().max(1)
        );
        println!("  total            {total} connections established");
        println!(
            "  slots            {} concurrent connectors",
            opts.connections
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_strips_scheme_and_path() {
        assert_eq!(host_port("ws://127.0.0.1:8081/"), "127.0.0.1:8081");
        assert_eq!(host_port("wss://example.com:443/echo"), "example.com:443");
        assert_eq!(host_port("127.0.0.1:9000"), "127.0.0.1:9000");
    }
}
