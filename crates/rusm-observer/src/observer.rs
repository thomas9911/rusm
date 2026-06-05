use std::sync::atomic::{AtomicBool, Ordering};

use rusm_metrics::Counter;

use crate::types::{ObserverSnapshot, ProcessInfo, ProcessStatus};

/// Aggregates a node's live state into [`ObserverSnapshot`]s.
///
/// Lifetime counters (`spawned`/`finished`/`messages`) are relaxed atomics bumped
/// on the hot path. Building a snapshot is a cheap read of those plus a fold over
/// the supplied process slice — the runtime owns the process table; the observer
/// never holds a lock on it.
///
/// The per-instance detail table is the only expensive part of a snapshot (it is
/// cloned and serialised). Disabling detail skips it entirely, which is what
/// makes "observer on vs off" benchmarks a fair comparison.
#[derive(Debug)]
pub struct Observer {
    spawned: Counter,
    finished: Counter,
    messages: Counter,
    scheduler_count: usize,
    max_detail: usize,
    detail_enabled: AtomicBool,
}

impl Observer {
    /// `scheduler_count` of 0 is treated as 1. `max_detail` caps how many
    /// processes appear in a snapshot's detail table.
    pub fn new(scheduler_count: usize, max_detail: usize) -> Self {
        Self {
            spawned: Counter::new(),
            finished: Counter::new(),
            messages: Counter::new(),
            scheduler_count: scheduler_count.max(1),
            max_detail,
            detail_enabled: AtomicBool::new(true),
        }
    }

    pub fn record_spawn(&self) {
        self.spawned.incr();
    }

    pub fn record_finish(&self) {
        self.finished.incr();
    }

    pub fn record_message(&self) {
        self.messages.incr();
    }

    pub fn record_messages(&self, n: u64) {
        self.messages.add(n);
    }

    pub fn set_detail_enabled(&self, enabled: bool) {
        self.detail_enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn detail_enabled(&self) -> bool {
        self.detail_enabled.load(Ordering::Relaxed)
    }

    pub fn scheduler_count(&self) -> usize {
        self.scheduler_count
    }

    pub fn snapshot(
        &self,
        uptime_ms: u64,
        processes: &[ProcessInfo],
        scheduler_load: &[f32],
    ) -> ObserverSnapshot {
        let mut running = 0;
        let mut waiting = 0;
        let mut total_memory_bytes = 0;
        for p in processes {
            match p.status {
                ProcessStatus::Running => running += 1,
                ProcessStatus::Waiting => waiting += 1,
                _ => {}
            }
            total_memory_bytes += p.memory_bytes;
        }

        let detail = if self.detail_enabled() {
            processes.iter().take(self.max_detail).cloned().collect()
        } else {
            Vec::new()
        };

        let mut load = scheduler_load.to_vec();
        load.truncate(self.scheduler_count);
        load.resize(self.scheduler_count, 0.0);

        ObserverSnapshot {
            uptime_ms,
            process_count: processes.len(),
            running,
            waiting,
            scheduler_load: load,
            total_memory_bytes,
            spawned_total: self.spawned.get(),
            finished_total: self.finished.get(),
            messages_total: self.messages.get(),
            processes: detail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(id: u64, status: ProcessStatus, memory_bytes: u64) -> ProcessInfo {
        ProcessInfo {
            id,
            name: None,
            status,
            mailbox_depth: 0,
            memory_bytes,
            reductions: 0,
        }
    }

    #[test]
    fn zero_schedulers_becomes_one() {
        assert_eq!(Observer::new(0, 10).scheduler_count(), 1);
    }

    #[test]
    fn detail_is_enabled_by_default() {
        assert!(Observer::new(4, 10).detail_enabled());
    }

    #[test]
    fn counters_feed_the_snapshot() {
        let obs = Observer::new(2, 10);
        obs.record_spawn();
        obs.record_spawn();
        obs.record_finish();
        obs.record_message();
        obs.record_messages(4);
        let snap = obs.snapshot(0, &[], &[]);
        assert_eq!(snap.spawned_total, 2);
        assert_eq!(snap.finished_total, 1);
        assert_eq!(snap.messages_total, 5);
    }

    #[test]
    fn snapshot_aggregates_status_and_memory() {
        let obs = Observer::new(2, 10);
        let procs = [
            proc(1, ProcessStatus::Running, 100),
            proc(2, ProcessStatus::Running, 200),
            proc(3, ProcessStatus::Waiting, 50),
            proc(4, ProcessStatus::Sleeping, 25),
        ];
        let snap = obs.snapshot(1234, &procs, &[0.5, 0.25]);
        assert_eq!(snap.uptime_ms, 1234);
        assert_eq!(snap.process_count, 4);
        assert_eq!(snap.running, 2);
        assert_eq!(snap.waiting, 1);
        assert_eq!(snap.total_memory_bytes, 375);
        assert_eq!(snap.processes.len(), 4);
    }

    #[test]
    fn disabling_detail_drops_the_process_table_only() {
        let obs = Observer::new(1, 10);
        let procs = [proc(1, ProcessStatus::Running, 100)];
        obs.set_detail_enabled(false);
        let snap = obs.snapshot(0, &procs, &[]);
        // Aggregates remain; only the per-instance table is suppressed.
        assert_eq!(snap.process_count, 1);
        assert_eq!(snap.running, 1);
        assert!(snap.processes.is_empty());
    }

    #[test]
    fn detail_table_is_capped_at_max_detail() {
        let obs = Observer::new(1, 2);
        let procs: Vec<_> = (0..5).map(|i| proc(i, ProcessStatus::Running, 1)).collect();
        let snap = obs.snapshot(0, &procs, &[]);
        assert_eq!(snap.process_count, 5);
        assert_eq!(snap.processes.len(), 2);
    }

    #[test]
    fn scheduler_load_is_normalised_to_scheduler_count() {
        let obs = Observer::new(3, 10);
        // Too few entries are padded with zeroes.
        assert_eq!(
            obs.snapshot(0, &[], &[0.9]).scheduler_load,
            vec![0.9, 0.0, 0.0]
        );
        // Too many are truncated.
        assert_eq!(
            obs.snapshot(0, &[], &[0.1, 0.2, 0.3, 0.4]).scheduler_load,
            vec![0.1, 0.2, 0.3]
        );
    }

    #[test]
    fn snapshot_round_trips_through_json() {
        let obs = Observer::new(2, 10);
        obs.record_spawn();
        let procs = [proc(1, ProcessStatus::Crashed, 10)];
        let snap = obs.snapshot(7, &procs, &[1.0, 0.0]);
        let json = serde_json::to_string(&snap).unwrap();
        let back: ObserverSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }
}
