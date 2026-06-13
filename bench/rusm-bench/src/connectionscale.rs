use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusm_otp::{ProcessHandle, Runtime};
use socket2::{Domain, Socket, Type};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::task::JoinHandle;

use crate::sample::Sample;

/// Sample connect latency every Nth (re)connection, per client.
const LATENCY_EVERY: u64 = 64;
/// Most latency samples surfaced in a single tick.
const LATENCY_SAMPLE: usize = 64;
/// Listener ports to shard destinations across — multiplied by the source-port
/// trick, this lifts the ephemeral wall so the fd ceiling is what binds.
const PORTS: usize = 34;
/// Client source ports start here (below the 49152+ ephemeral range the listeners
/// land in, so they never collide).
const SRC_BASE: u16 = 20_000;
/// Small socket buffers: held-idle connections move no data.
const SOCK_BUF: usize = 4 * 1024;
/// Held connections targeted per spawn worker (the throughput dial scales the
/// connection count too); capped by the fd ceiling below.
const PER_WORKER: usize = 6_000;

/// A **real** held-open connection storm at scale: ramps tens of thousands of
/// concurrent loopback connections — each its own `rusm-otp` process — and holds
/// them, recycling at the edge to keep a live reconnect rate. Where
/// `connection-storm` churns one port for connect/sec, this proves *concurrency*:
/// how many live connection processes coexist.
///
/// The ceiling is the **OS** (file descriptors — 2 per loopback connection), never
/// RUSM. The client sheds the ephemeral-port wall with the *4-tuple trick* (each
/// task owns a disjoint source-port stripe bound with `SO_REUSEADDR`, paired with all
/// `PORTS` destinations), so the only wall left is fds. [`tick`](Self::tick) charts
/// the live process count (held connections) and the reconnect rate.
///
/// Must be constructed inside a Tokio runtime (it binds listeners and spawns tasks).
pub struct ConnectionScaleEngine {
    runtime: Runtime,
    accepted: Arc<AtomicU64>,
    latency_rx: UnboundedReceiver<u64>,
    acceptors: Vec<ProcessHandle>,
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

impl ConnectionScaleEngine {
    pub fn new(workers: usize, scheduler_count: usize) -> Self {
        let workers = workers.max(1);
        // Raise the fd limit to the per-process cap, then keep headroom for the
        // dashboard's own sockets; the target is 80% of what's left.
        let _ = rlimit::increase_nofile_limit(u64::MAX);
        let soft = soft_limit();
        let ceiling = (soft / 2).saturating_sub(1_024);
        let target = (workers * PER_WORKER).min(ceiling * 4 / 5).max(workers);

        let runtime = Runtime::new();
        let accepted = Arc::new(AtomicU64::new(0));
        let (latency_tx, latency_rx) = unbounded_channel();

        // Server: PORTS listeners; each acceptor spawns a process per accepted
        // connection that parks on the socket (holding it open).
        let mut addrs = Vec::with_capacity(PORTS);
        let mut acceptors = Vec::with_capacity(PORTS);
        for _ in 0..PORTS {
            let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            std_listener
                .set_nonblocking(true)
                .expect("listener non-blocking");
            addrs.push(std_listener.local_addr().expect("listener addr"));
            let listener = tokio::net::TcpListener::from_std(std_listener).expect("adopt listener");
            let accept_rt = runtime.clone();
            let count = Arc::clone(&accepted);
            let acceptor = runtime.spawn(move |_ctx| async move {
                while let Ok((stream, _)) = listener.accept().await {
                    shrink(&stream);
                    count.fetch_add(1, Ordering::Relaxed);
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
            acceptors.push(acceptor);
        }
        let addrs = Arc::new(addrs);

        // Clients: a pool, each owning a disjoint source-port stripe. Each ramps its
        // share of the target, then recycles (RST + reopen the same 4-tuple) so held
        // stays at the target while the reconnect rate stays live.
        let client_tasks = (workers * 32).clamp(64, 512);
        let share = target.div_ceil(client_tasks).max(1);
        let clients = (0..client_tasks)
            .map(|t| {
                let addrs = Arc::clone(&addrs);
                let latency_tx = latency_tx.clone();
                tokio::spawn(async move {
                    let ports = addrs.len();
                    // This task's globally-unique 4-tuples.
                    let tuples: Vec<(u16, SocketAddr)> = (0..share)
                        .map(|j| {
                            let src = SRC_BASE as usize + t + (j / ports) * client_tasks;
                            (src as u16, addrs[j % ports])
                        })
                        .filter(|(src, _)| *src < 49_000)
                        .collect();
                    if tuples.is_empty() {
                        return;
                    }
                    let mut held: Vec<Option<TcpStream>> = Vec::with_capacity(tuples.len());
                    let mut n = 0u64;

                    // Ramp: open every tuple once.
                    for &(src, dst) in &tuples {
                        let started = Instant::now();
                        match connect(src, dst).await {
                            Ok(stream) => {
                                if n.is_multiple_of(LATENCY_EVERY) {
                                    let _ = latency_tx.send(started.elapsed().as_nanos() as u64);
                                }
                                held.push(Some(stream));
                                n += 1;
                            }
                            // fd pressure: ease off, then carry on.
                            Err(_) => {
                                held.push(None);
                                tokio::time::sleep(Duration::from_millis(5)).await;
                            }
                        }
                    }

                    // Recycle round-robin: RST-close one and reopen the same 4-tuple.
                    let mut cursor = 0usize;
                    loop {
                        let (src, dst) = tuples[cursor];
                        held[cursor] = None; // drop → RST (SO_LINGER 0) → frees the 4-tuple
                        let started = Instant::now();
                        if let Ok(stream) = connect(src, dst).await {
                            held[cursor] = Some(stream);
                            if n.is_multiple_of(LATENCY_EVERY) {
                                let _ = latency_tx.send(started.elapsed().as_nanos() as u64);
                            }
                            n += 1;
                        }
                        cursor = (cursor + 1) % tuples.len();
                    }
                })
            })
            .collect();

        Self {
            runtime,
            accepted,
            latency_rx,
            acceptors,
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

        // process_count ≈ live connection handlers (+ the acceptors): the
        // peak-concurrent connections the runner charts — the headline.
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

impl Drop for ConnectionScaleEngine {
    fn drop(&mut self) {
        for acceptor in &self.acceptors {
            acceptor.kill(); // stop accepting and drop the listeners
        }
        for client in &self.clients {
            client.abort(); // drop held client sockets, closing the connections
        }
    }
}

/// Connect from an explicit source port (bound with `SO_REUSEADDR` so source ports
/// pair with many destinations — the 4-tuple stays unique, dodging ephemeral
/// exhaustion), with small buffers and RST-on-close.
async fn connect(src_port: u16, addr: SocketAddr) -> std::io::Result<TcpStream> {
    let socket = Socket::new(Domain::IPV4, Type::STREAM, None)?;
    socket.set_reuse_address(true).ok();
    socket.set_send_buffer_size(SOCK_BUF).ok();
    socket.set_recv_buffer_size(SOCK_BUF).ok();
    socket.set_linger(Some(Duration::ZERO)).ok();
    let src: SocketAddr = (Ipv4Addr::LOCALHOST, src_port).into();
    socket.bind(&src.into())?;
    socket.set_nonblocking(true)?;
    match socket.connect(&addr.into()) {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(e) => return Err(e),
    }
    let stream = TcpStream::from_std(std::net::TcpStream::from(socket))?;
    stream.writable().await?;
    if let Some(err) = stream.take_error()? {
        return Err(err);
    }
    Ok(stream)
}

fn shrink(stream: &tokio::net::TcpStream) {
    let sock = socket2::SockRef::from(stream);
    sock.set_send_buffer_size(SOCK_BUF).ok();
    sock.set_recv_buffer_size(SOCK_BUF).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn holds_many_connections_each_its_own_process() {
        let mut engine = ConnectionScaleEngine::new(1, 4);
        // Poll until the ramp has clearly grown past the acceptors, then stop — we
        // don't wait for the full target (that's thousands of connections).
        let mut sample = engine.tick();
        for _ in 0..300 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            sample = engine.tick();
            if sample.process_count > PORTS as u64 + 100 && !sample.latencies_ns.is_empty() {
                break;
            }
        }
        assert!(
            sample.process_count > PORTS as u64 + 100,
            "many connections held, each its own process (got {})",
            sample.process_count
        );
        assert!(!sample.latencies_ns.is_empty(), "connect latency sampled");
        assert_eq!(sample.scheduler_load.len(), 4);
    }
}
