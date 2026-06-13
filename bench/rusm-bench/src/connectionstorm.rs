use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusm_otp::{ProcessHandle, Runtime};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::task::JoinHandle;

use crate::sample::Sample;

/// Connect latency is sampled every Nth new connection.
const LATENCY_EVERY: u64 = 64;
/// Most latency samples surfaced in a single tick.
const LATENCY_SAMPLE: usize = 64;
/// Held connections per client worker (the fd budget caps it further), kept under
/// the client's ephemeral-port range.
const PER_WORKER_TARGET: usize = 1536;

/// A **real** TCP connection storm over `rusm-otp`, the headline scenario.
///
/// A loopback listener serves **one process per connection**; client workers ramp
/// up to a held-open target (bounded by the open-file limit, which we raise first)
/// then continuously recycle connections at full speed. Each client socket is set
/// to `SO_LINGER(0)`, so closing sends a RST rather than a FIN — **no TIME_WAIT**,
/// so the churn never exhausts ephemeral ports. [`tick`](Self::tick) samples
/// connections/sec (server-side accepts) and connect latency; peak-concurrent is
/// the live process count.
///
/// The ceiling is the **OS** (file descriptors, connect/accept throughput), not
/// RUSM: minting a process per connection is near-free — the spawn storm does
/// 1.4M/s. Must be constructed inside a Tokio runtime.
pub struct ConnectionStormEngine {
    runtime: Runtime,
    accepted: Arc<AtomicU64>,
    latency_rx: UnboundedReceiver<u64>,
    acceptor: ProcessHandle,
    clients: Vec<JoinHandle<()>>,
    last_accepted: u64,
    last_at: Instant,
    scheduler_count: usize,
}

#[cfg(not(windows))]
fn soft_limit() -> usize {
    rlimit::Resource::NOFILE
        .get()
        .map(|(soft, _hard)| soft as usize)
        .unwrap_or(256);
}

#[cfg(windows)]
fn soft_limit() -> usize {
    256
}


impl ConnectionStormEngine {
    pub fn new(workers: usize, scheduler_count: usize) -> Self {
        let workers = workers.max(1);
        // Raise the soft open-file limit to the hard cap so we can hold many
        // sockets; the target is then a fraction of that budget (each in-process
        // connection costs ~2 fds: client + accepted server side).
        let _ = rlimit::increase_nofile_limit(u64::MAX);
        let soft = soft_limit();
        let budget = (soft / 2).saturating_sub(64);
        let target = budget.min(workers * PER_WORKER_TARGET).max(workers);
        let share = (target / workers).max(1);

        let runtime = Runtime::new();
        let accepted = Arc::new(AtomicU64::new(0));
        let (latency_tx, latency_rx) = unbounded_channel();

        let listener = {
            let std_listener =
                std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
            std_listener
                .set_nonblocking(true)
                .expect("set listener non-blocking");
            tokio::net::TcpListener::from_std(std_listener).expect("adopt std listener")
        };
        let addr = listener.local_addr().expect("listener address");

        // Acceptor: one process per accepted connection, each holding the socket
        // open (a read that parks until the client closes).
        let accept_rt = runtime.clone();
        let accept_count = Arc::clone(&accepted);
        let acceptor = runtime.spawn(move |_ctx| async move {
            while let Ok((stream, _peer)) = listener.accept().await {
                accept_count.fetch_add(1, Ordering::Relaxed);
                accept_rt.spawn(move |_ctx| async move {
                    let mut stream = stream;
                    let mut buf = [0u8; 1];
                    loop {
                        match stream.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                });
            }
        });

        let clients = (0..workers)
            .map(|_| {
                let client_rt = runtime.clone();
                let latency_tx = latency_tx.clone();
                tokio::spawn(async move {
                    let mut held: Vec<TcpStream> = Vec::with_capacity(share);
                    let mut opened: u64 = 0;
                    loop {
                        while held.len() < share {
                            let started = Instant::now();
                            match client_rt.connect(addr).await {
                                Ok(stream) => {
                                    // RST on close (no TIME_WAIT) → churn never
                                    // exhausts ephemeral ports.
                                    let _ = socket2::SockRef::from(&stream)
                                        .set_linger(Some(Duration::ZERO));
                                    held.push(stream);
                                    opened += 1;
                                    if opened.is_multiple_of(LATENCY_EVERY) {
                                        let _ =
                                            latency_tx.send(started.elapsed().as_nanos() as u64);
                                    }
                                }
                                // fd pressure: ease off, then retry.
                                Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
                            }
                        }
                        held.swap_remove(0); // RST-close the oldest; reopened next pass
                    }
                })
            })
            .collect();

        Self {
            runtime,
            accepted,
            latency_rx,
            acceptor,
            clients,
            last_accepted: 0,
            last_at: Instant::now(),
            scheduler_count,
        }
    }

    pub fn tick(&mut self) -> Sample {
        let now = Instant::now();
        let accepted = self.accepted.load(Ordering::Relaxed);
        let dt = now
            .duration_since(self.last_at)
            .as_secs_f64()
            .max(f64::MIN_POSITIVE);
        let ops_per_sec = accepted.saturating_sub(self.last_accepted) as f64 / dt;
        self.last_accepted = accepted;
        self.last_at = now;

        let mut latencies_ns = Vec::new();
        while let Ok(ns) = self.latency_rx.try_recv() {
            latencies_ns.push(ns);
        }
        if latencies_ns.len() > LATENCY_SAMPLE {
            latencies_ns = latencies_ns.split_off(latencies_ns.len() - LATENCY_SAMPLE);
        }

        // process_count ≈ live connection handlers (+ the acceptor): the
        // peak-concurrent connections the runner charts.
        let process_count = self.runtime.process_count() as u64;
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

impl Drop for ConnectionStormEngine {
    fn drop(&mut self) {
        self.acceptor.kill(); // stop accepting and drop the listener
        for client in &self.clients {
            client.abort(); // drop held client sockets, closing the connections
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn clients_open_connections_each_served_by_its_own_process() {
        let mut engine = ConnectionStormEngine::new(2, 4);
        // Poll until the storm has ramped, rather than betting on a fixed sleep:
        // robust to scheduling/IO timing, and bounded so it can't hang.
        let mut sample = engine.tick();
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            sample = engine.tick();
            if sample.process_count > 1 && !sample.latencies_ns.is_empty() {
                break;
            }
        }
        assert!(
            sample.process_count > 1,
            "each accepted connection is its own live process (+ the acceptor)"
        );
        assert!(
            !sample.latencies_ns.is_empty(),
            "connect latency is sampled"
        );
        assert_eq!(sample.scheduler_load.len(), 4);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn there_is_always_at_least_one_client() {
        let engine = ConnectionStormEngine::new(0, 1);
        assert_eq!(engine.clients.len(), 1);
    }
}
