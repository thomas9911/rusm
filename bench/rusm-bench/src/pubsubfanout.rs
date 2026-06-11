use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rusm_otp::{Pid, ProcessHandle, Runtime};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use crate::sample::Sample;

/// Subscribers per publisher worker — the fan-out factor, so one publish becomes
/// this many deliveries. Keeps the "1 → N broadcast" story front and centre.
const FANOUT: usize = 8;
/// Stamp the publish→delivery latency on every Nth publish (the rest are plain
/// fan-out), bounding the sample stream.
const TIMED_EVERY: u64 = 64;
/// Most latency samples surfaced in a single tick.
const LATENCY_SAMPLE: usize = 64;
/// Payload tag: a plain fan-out message vs. one carrying a publish timestamp.
const NORMAL: u8 = 0;
const TIMED: u8 = 1;

/// A **real, continuous publish/subscribe fan-out** over `rusm-otp`: a publisher
/// process holds a set of subscriber pids and broadcasts to all of them in a tight
/// loop — exactly the mechanics of `rusm_rs::pubsub::Topics::publish` (`for sub in
/// subs { send(sub, msg) }`), the broker primitive guests embed. [`tick`](Self::tick)
/// reports the achieved *delivery* rate (Δdeliveries / Δt — one publish counts as
/// `FANOUT` deliveries) and the one-way publish→delivery latency.
///
/// The publisher is the bottleneck (it issues N sends per publish); each subscriber
/// just drains and counts, so mailboxes stay shallow and nothing grows unbounded —
/// the honest sustained fan-out throughput. Must be constructed inside a Tokio runtime.
pub struct PubSubFanoutEngine {
    runtime: Runtime,
    processes: Vec<ProcessHandle>,
    deliveries: Arc<AtomicU64>,
    latency_rx: UnboundedReceiver<u64>,
    last_deliveries: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl PubSubFanoutEngine {
    pub fn new(workers: usize, scheduler_count: usize) -> Self {
        let runtime = Runtime::new();
        let deliveries = Arc::new(AtomicU64::new(0));
        let (latency_tx, latency_rx) = unbounded_channel();
        // A shared monotonic epoch: the publisher stamps `epoch.elapsed()` into a timed
        // message and the subscriber subtracts it from its own `epoch.elapsed()` — a
        // valid one-way latency since both read the same clock.
        let epoch = Instant::now();
        let mut processes = Vec::new();

        for _ in 0..workers.max(1) {
            // Subscribers first, so the publisher can capture their pids. Subscriber 0
            // also reports latency; the rest only count.
            let mut subscribers: Vec<Pid> = Vec::with_capacity(FANOUT);
            for index in 0..FANOUT {
                let deliveries = Arc::clone(&deliveries);
                let latency_tx = latency_tx.clone();
                let sub = runtime.spawn(move |mut ctx| async move {
                    loop {
                        let msg = ctx
                            .recv()
                            .await
                            .message()
                            .expect("fan-out is a user message");
                        deliveries.fetch_add(1, Ordering::Relaxed);
                        if index == 0 && msg.first() == Some(&TIMED) {
                            let t0 = u64::from_le_bytes(
                                msg[1..9]
                                    .try_into()
                                    .expect("a timed publish carries 8 bytes"),
                            );
                            let now = epoch.elapsed().as_nanos() as u64;
                            let _ = latency_tx.send(now.saturating_sub(t0));
                        }
                    }
                });
                subscribers.push(sub.pid());
                processes.push(sub);
            }

            // Publisher: broadcast to every subscriber, forever — `Topics::publish`.
            let publisher_rt = runtime.clone();
            let publisher = runtime.spawn(move |_ctx| async move {
                let mut round: u64 = 0;
                loop {
                    let timed = round.is_multiple_of(TIMED_EVERY);
                    let payload = build_payload(timed, &epoch);
                    for &sub in &subscribers {
                        publisher_rt.send(sub, payload.clone());
                    }
                    round += 1;
                    // Cooperative: never monopolise a scheduler thread with the send loop.
                    tokio::task::yield_now().await;
                }
            });
            processes.push(publisher);
        }

        Self {
            runtime,
            processes,
            deliveries,
            latency_rx,
            last_deliveries: 0,
            last_at: Instant::now(),
            scheduler_count,
        }
    }

    pub fn tick(&mut self) -> Sample {
        let now = Instant::now();
        let deliveries = self.deliveries.load(Ordering::Relaxed);
        let dt = now
            .duration_since(self.last_at)
            .as_secs_f64()
            .max(f64::MIN_POSITIVE);
        let ops_per_sec = deliveries.saturating_sub(self.last_deliveries) as f64 / dt;
        self.last_deliveries = deliveries;
        self.last_at = now;

        let mut latencies_ns = Vec::new();
        while let Ok(ns) = self.latency_rx.try_recv() {
            latencies_ns.push(ns);
        }
        if latencies_ns.len() > LATENCY_SAMPLE {
            latencies_ns = latencies_ns.split_off(latencies_ns.len() - LATENCY_SAMPLE);
        }

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

/// A fan-out payload: `[tag]` then, when timed, an 8-byte publish timestamp.
fn build_payload(timed: bool, epoch: &Instant) -> Vec<u8> {
    if timed {
        let mut payload = Vec::with_capacity(9);
        payload.push(TIMED);
        payload.extend_from_slice(&(epoch.elapsed().as_nanos() as u64).to_le_bytes());
        payload
    } else {
        vec![NORMAL]
    }
}

impl Drop for PubSubFanoutEngine {
    fn drop(&mut self) {
        for process in &self.processes {
            process.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn publisher_fans_out_and_reports_rate_and_latency() {
        let mut engine = PubSubFanoutEngine::new(1, 4);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let sample = engine.tick();
        assert!(sample.ops_per_sec > 0.0, "deliveries should be flowing");
        // One publisher + FANOUT subscribers.
        assert_eq!(sample.process_count as usize, 1 + FANOUT);
        assert_eq!(sample.scheduler_load.len(), 4);
        assert!(
            !sample.latencies_ns.is_empty(),
            "publish→delivery should be timed"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_count_is_at_least_one() {
        let engine = PubSubFanoutEngine::new(0, 1);
        // One worker group: a publisher plus its FANOUT subscribers.
        assert_eq!(engine.processes.len(), 1 + FANOUT);
    }
}
