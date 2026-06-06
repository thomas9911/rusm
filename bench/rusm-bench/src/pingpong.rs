use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rusm_otp::{Pid, ProcessHandle, Runtime};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use crate::sample::Sample;

/// Round-trip latency is recorded every Nth bounce, keeping the sample stream
/// bounded no matter how fast the pairs run.
const LATENCY_EVERY: u64 = 64;
/// Most latency samples surfaced in a single tick.
const LATENCY_SAMPLE: usize = 64;

/// A **real, continuous** message ping-pong over `rusm-otp`: `pairs` pinger/
/// ponger process pairs bounce messages as fast as the runtime allows.
/// [`tick`](Self::tick) samples the achieved message rate (Δmessages / Δt) and
/// round-trip latency.
///
/// Each ping carries the sender's pid in its first 8 bytes, so the ponger knows
/// whom to reply to — the byte-level form of Erlang's `send(peer, {self(), :ping})`.
/// Must be constructed inside a Tokio runtime.
pub struct PingPongEngine {
    runtime: Runtime,
    pairs: Vec<ProcessHandle>,
    messages: Arc<AtomicU64>,
    latency_rx: UnboundedReceiver<u64>,
    last_messages: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl PingPongEngine {
    pub fn new(pairs: usize, scheduler_count: usize) -> Self {
        let runtime = Runtime::new();
        let messages = Arc::new(AtomicU64::new(0));
        let (latency_tx, latency_rx) = unbounded_channel();
        let mut handles = Vec::new();

        for _ in 0..pairs.max(1) {
            // Ponger: reply to whoever's pid leads the message, forever.
            let ponger_rt = runtime.clone();
            let ponger = runtime.spawn(move |mut ctx| async move {
                loop {
                    let msg = ctx.recv().await;
                    let reply_to = Pid::from_raw(u64::from_le_bytes(
                        msg[..8].try_into().expect("a ping carries an 8-byte pid"),
                    ));
                    ponger_rt.send(reply_to, Vec::new());
                }
            });
            let ponger_pid = ponger.pid();

            // Pinger: bounce against its ponger forever, counting and timing.
            let pinger_rt = runtime.clone();
            let messages = Arc::clone(&messages);
            let latency_tx = latency_tx.clone();
            let pinger = runtime.spawn(move |mut ctx| async move {
                let ping = ctx.pid().raw().to_le_bytes().to_vec();
                let mut round: u64 = 0;
                loop {
                    let started = Instant::now();
                    pinger_rt.send(ponger_pid, ping.clone());
                    let _pong = ctx.recv().await;
                    messages.fetch_add(2, Ordering::Relaxed); // the ping and the pong
                    round += 1;
                    if round % LATENCY_EVERY == 0 {
                        let _ = latency_tx.send(started.elapsed().as_nanos() as u64);
                    }
                }
            });

            handles.push(ponger);
            handles.push(pinger);
        }

        Self {
            runtime,
            pairs: handles,
            messages,
            latency_rx,
            last_messages: 0,
            last_at: Instant::now(),
            scheduler_count,
        }
    }

    pub fn tick(&mut self) -> Sample {
        let now = Instant::now();
        let messages = self.messages.load(Ordering::Relaxed);
        let dt = now
            .duration_since(self.last_at)
            .as_secs_f64()
            .max(f64::MIN_POSITIVE);
        let ops_per_sec = messages.saturating_sub(self.last_messages) as f64 / dt;
        self.last_messages = messages;
        self.last_at = now;

        // Drain everything queued since the last tick (so the channel can't grow),
        // then keep the most recent window for the histogram.
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

impl Drop for PingPongEngine {
    fn drop(&mut self) {
        // The pairs loop forever; stop them so they don't outlive the engine.
        for process in &self.pairs {
            process.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pairs_bounce_messages_and_report_rate_and_latency() {
        let mut engine = PingPongEngine::new(2, 4);
        // Warm up so messages flow and latency samples (every 64th bounce) surface.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let sample = engine.tick();
        assert!(sample.ops_per_sec > 0.0, "messages should be flowing");
        assert_eq!(sample.process_count, 4); // two pairs, all alive
        assert_eq!(sample.scheduler_load.len(), 4);
        assert!(
            !sample.latencies_ns.is_empty(),
            "round-trips should be timed"
        );
        assert_eq!(sample.total_memory_bytes, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pair_count_is_at_least_one() {
        let engine = PingPongEngine::new(0, 1);
        assert_eq!(engine.pairs.len(), 2); // one pair = one ponger + one pinger
    }
}
