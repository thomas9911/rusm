use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusm_cluster::{ClusterNode, Identity};
use rusm_otp::{ProcessHandle, Runtime};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::sample::Sample;

/// Sample one round-trip's latency every Nth completion per sender.
const LATENCY_EVERY: u64 = 64;
/// Most latency samples surfaced in a single tick.
const LATENCY_SAMPLE: usize = 64;

/// A **real** cross-node fan-out over `rusm-cluster` — distributed messaging on
/// loopback QUIC + TLS, the Phase-9 headline.
///
/// One **hub** node and a handful of **worker** nodes (each its own QUIC endpoint,
/// a faithful in-process stand-in for separate machines). A pool of senders on the
/// hub each keep **one round-trip in flight**: send a 2-byte tag to a worker's
/// `echo`, which bounces it to the hub's `collector`, which signals that sender to
/// go again. Bounding in-flight to one-per-sender keeps latency representative of
/// the transport rather than of an unbounded backlog, while the sender pool drives
/// real throughput. [`tick`](Self::tick) samples cross-node round-trips/sec and
/// round-trip latency; peak-concurrent is the live process count across all nodes.
///
/// Must be constructed inside a Tokio runtime (it binds endpoints and spawns tasks).
pub struct DistributedFanoutEngine {
    nodes: Vec<ClusterNode>,
    processes: Vec<ProcessHandle>,
    delivered: Arc<AtomicU64>,
    latency_rx: UnboundedReceiver<u64>,
    stop: Arc<AtomicBool>,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    boot: JoinHandle<()>,
    last_delivered: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl DistributedFanoutEngine {
    pub fn new(workers: usize, scheduler_count: usize) -> Self {
        // A few worker nodes (QUIC endpoints are heavier than a process); many
        // senders for concurrency, scaled with the resource profile.
        let worker_nodes = (workers / 4).clamp(1, 6);
        let senders = workers.clamp(8, 128);

        let id = Identity::generate().expect("cluster identity");
        let local: SocketAddr = "127.0.0.1:0".parse().expect("loopback addr");
        let delivered = Arc::new(AtomicU64::new(0));
        let (latency_tx, latency_rx) = unbounded_channel();

        // Per-sender signal channels: the collector pings sender `sid` when its
        // in-flight round-trip completes, so the sender can fire the next.
        let mut signals: Vec<UnboundedSender<()>> = Vec::with_capacity(senders);
        let mut sender_rxs: Vec<UnboundedReceiver<()>> = Vec::with_capacity(senders);
        for _ in 0..senders {
            let (tx, rx) = unbounded_channel();
            signals.push(tx);
            sender_rxs.push(rx);
        }

        // Hub with a collector that counts replies and signals the right sender.
        let hub = ClusterNode::bind("hub", Runtime::new(), local, &id).expect("bind hub");
        let mut processes = Vec::with_capacity(worker_nodes + 1);
        {
            let delivered = Arc::clone(&delivered);
            let collector = hub.runtime().spawn(move |mut ctx| async move {
                while let Some(m) = ctx.recv().await.message() {
                    delivered.fetch_add(1, Ordering::Relaxed);
                    if m.len() >= 2 {
                        let sid = u16::from_le_bytes([m[0], m[1]]) as usize;
                        if let Some(sig) = signals.get(sid) {
                            let _ = sig.send(());
                        }
                    }
                }
            });
            hub.runtime().register("collector", collector.pid());
            processes.push(collector);
        }

        // Worker nodes, each bouncing every message back to the hub's collector.
        let mut nodes = vec![hub.clone()];
        let mut worker_addrs: Vec<SocketAddr> = Vec::with_capacity(worker_nodes);
        for i in 0..worker_nodes {
            let worker = ClusterNode::bind(format!("w{i}"), Runtime::new(), local, &id)
                .expect("bind worker");
            let echo_node = worker.clone();
            let echo = worker.runtime().spawn(move |mut ctx| async move {
                while let Some(m) = ctx.recv().await.message() {
                    let _ = echo_node.send("hub", "collector", &m).await;
                }
            });
            worker.runtime().register("echo", echo.pid());
            worker_addrs.push(worker.local_addr().expect("worker addr"));
            processes.push(echo);
            nodes.push(worker);
        }

        // Boot: connect the hub to every worker, then start the sender pool. Done
        // off-thread because `connect` is async; throughput ramps as links form.
        let stop = Arc::new(AtomicBool::new(false));
        let tasks: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let boot = {
            let hub = hub.clone();
            let stop = Arc::clone(&stop);
            let tasks = Arc::clone(&tasks);
            tokio::spawn(async move {
                for addr in &worker_addrs {
                    let _ = hub.connect(*addr).await;
                }
                let handles: Vec<JoinHandle<()>> = sender_rxs
                    .into_iter()
                    .enumerate()
                    .map(|(sid, mut rx)| {
                        let hub = hub.clone();
                        let worker = format!("w{}", sid % worker_nodes);
                        let stop = Arc::clone(&stop);
                        let latency_tx = latency_tx.clone();
                        tokio::spawn(async move {
                            let tag = (sid as u16).to_le_bytes();
                            let mut n: u64 = 0;
                            while !stop.load(Ordering::Relaxed) {
                                let started = Instant::now();
                                if hub.send(&worker, "echo", &tag).await.is_err() {
                                    // link not up yet (or torn down): back off, retry.
                                    tokio::time::sleep(Duration::from_millis(5)).await;
                                    continue;
                                }
                                if rx.recv().await.is_none() {
                                    break; // engine dropped
                                }
                                if n.is_multiple_of(LATENCY_EVERY) {
                                    let _ = latency_tx.send(started.elapsed().as_nanos() as u64);
                                }
                                n += 1;
                            }
                        })
                    })
                    .collect();
                tasks.lock().unwrap().extend(handles);
            })
        };

        Self {
            nodes,
            processes,
            delivered,
            latency_rx,
            stop,
            tasks,
            boot,
            last_delivered: 0,
            last_at: Instant::now(),
            scheduler_count,
        }
    }

    pub fn tick(&mut self) -> Sample {
        let now = Instant::now();
        let delivered = self.delivered.load(Ordering::Relaxed);
        let dt = now
            .duration_since(self.last_at)
            .as_secs_f64()
            .max(f64::MIN_POSITIVE);
        let ops_per_sec = delivered.saturating_sub(self.last_delivered) as f64 / dt;
        self.last_delivered = delivered;
        self.last_at = now;

        let mut latencies_ns = Vec::new();
        while let Ok(ns) = self.latency_rx.try_recv() {
            latencies_ns.push(ns);
        }
        if latencies_ns.len() > LATENCY_SAMPLE {
            latencies_ns = latencies_ns.split_off(latencies_ns.len() - LATENCY_SAMPLE);
        }

        // Live processes across the whole cluster (hub collector + each worker echo
        // + the per-peer routing tasks' processes), the peak-concurrent we chart.
        let process_count = self
            .nodes
            .iter()
            .map(|n| n.runtime().process_count() as u64)
            .sum();

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

impl Drop for DistributedFanoutEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.boot.abort();
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
        for process in &self.processes {
            process.kill(); // end the echo/collector loops, freeing their node clones
        }
        for node in &self.nodes {
            node.shutdown(); // close endpoints → accept/peer loops drain
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn senders_drive_cross_node_round_trips() {
        let mut engine = DistributedFanoutEngine::new(4, 4);
        // Poll until the cluster has formed and round-trips are flowing, rather
        // than betting on a fixed sleep (robust to scheduling, bounded so it can't
        // hang).
        let mut sample = engine.tick();
        for _ in 0..300 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            sample = engine.tick();
            if sample.ops_per_sec > 0.0 && !sample.latencies_ns.is_empty() {
                break;
            }
        }
        assert!(
            sample.ops_per_sec > 0.0,
            "cross-node round-trips are flowing"
        );
        assert!(
            !sample.latencies_ns.is_empty(),
            "round-trip latency is sampled"
        );
        assert!(
            sample.process_count >= 2,
            "hub + worker nodes run live processes"
        );
        assert_eq!(sample.scheduler_load.len(), 4);
    }
}
