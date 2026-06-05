use std::sync::atomic::{AtomicBool, Ordering};

use rusm_metrics::Counter;

use crate::types::{ObserverSnapshot, ProcessInfo};

/// A node's live numbers for one snapshot: authoritative aggregate totals plus a
/// (possibly capped) **sample** of processes for the detail table.
///
/// The aggregates (`process_count`, `running`, …) are the real cluster-scale
/// figures — the runtime tracks them cheaply. `processes` is only a sample, so
/// the observer never infers totals from it (a 5k-process node still reports
/// 5,000, while the table shows at most `max_detail` rows).
#[derive(Debug, Clone, Copy)]
pub struct NodeSample<'a> {
    pub process_count: usize,
    pub running: usize,
    pub waiting: usize,
    pub total_memory_bytes: u64,
    pub scheduler_load: &'a [f32],
    pub processes: &'a [ProcessInfo],
}

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

    pub fn snapshot(&self, uptime_ms: u64, sample: NodeSample) -> ObserverSnapshot {
        // Aggregates come from the authoritative totals; the per-instance table is
        // only the sample, optionally suppressed, and never exceeds max_detail.
        let processes = if self.detail_enabled() {
            sample
                .processes
                .iter()
                .take(self.max_detail)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        let mut scheduler_load = sample.scheduler_load.to_vec();
        scheduler_load.truncate(self.scheduler_count);
        scheduler_load.resize(self.scheduler_count, 0.0);

        ObserverSnapshot {
            uptime_ms,
            process_count: sample.process_count,
            running: sample.running,
            waiting: sample.waiting,
            scheduler_load,
            total_memory_bytes: sample.total_memory_bytes,
            spawned_total: self.spawned.get(),
            finished_total: self.finished.get(),
            messages_total: self.messages.get(),
            processes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProcessStatus;

    fn proc(id: u64) -> ProcessInfo {
        ProcessInfo {
            id,
            name: None,
            status: ProcessStatus::Running,
            mailbox_depth: 0,
            memory_bytes: 1024,
            reductions: 0,
        }
    }

    fn sample<'a>(
        process_count: usize,
        processes: &'a [ProcessInfo],
        scheduler_load: &'a [f32],
    ) -> NodeSample<'a> {
        NodeSample {
            process_count,
            running: process_count,
            waiting: 0,
            total_memory_bytes: process_count as u64 * 1024,
            scheduler_load,
            processes,
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
        let snap = obs.snapshot(0, sample(0, &[], &[]));
        assert_eq!(snap.spawned_total, 2);
        assert_eq!(snap.finished_total, 1);
        assert_eq!(snap.messages_total, 5);
    }

    #[test]
    fn aggregates_are_authoritative_totals_not_the_sample_size() {
        // The regression guard: a 5000-process node samples only a few rows for
        // the table, but must still report the real total of 5000.
        let obs = Observer::new(2, 64);
        let rows: Vec<_> = (0..10).map(proc).collect();
        let snap = obs.snapshot(
            1234,
            NodeSample {
                process_count: 5000,
                running: 4000,
                waiting: 600,
                total_memory_bytes: 5000 * 512_000,
                scheduler_load: &[0.5, 0.25],
                processes: &rows,
            },
        );
        assert_eq!(snap.uptime_ms, 1234);
        assert_eq!(snap.process_count, 5000);
        assert_eq!(snap.running, 4000);
        assert_eq!(snap.waiting, 600);
        assert_eq!(snap.total_memory_bytes, 5000 * 512_000);
        assert_eq!(snap.processes.len(), 10); // the sample, not the total
    }

    #[test]
    fn disabling_detail_drops_the_table_only() {
        let obs = Observer::new(1, 10);
        let rows = [proc(1)];
        obs.set_detail_enabled(false);
        let snap = obs.snapshot(0, sample(5000, &rows, &[]));
        // Aggregates remain; only the per-instance table is suppressed.
        assert_eq!(snap.process_count, 5000);
        assert!(snap.processes.is_empty());
    }

    #[test]
    fn detail_table_is_capped_at_max_detail() {
        let obs = Observer::new(1, 2);
        let rows: Vec<_> = (0..5).map(proc).collect();
        let snap = obs.snapshot(0, sample(5, &rows, &[]));
        assert_eq!(snap.process_count, 5);
        assert_eq!(snap.processes.len(), 2);
    }

    #[test]
    fn scheduler_load_is_normalised_to_scheduler_count() {
        let obs = Observer::new(3, 10);
        // Too few entries are padded with zeroes.
        assert_eq!(
            obs.snapshot(0, sample(0, &[], &[0.9])).scheduler_load,
            vec![0.9, 0.0, 0.0]
        );
        // Too many are truncated.
        assert_eq!(
            obs.snapshot(0, sample(0, &[], &[0.1, 0.2, 0.3, 0.4]))
                .scheduler_load,
            vec![0.1, 0.2, 0.3]
        );
    }

    #[test]
    fn snapshot_round_trips_through_json() {
        let obs = Observer::new(2, 10);
        obs.record_spawn();
        let rows = [proc(1)];
        let snap = obs.snapshot(7, sample(1, &rows, &[1.0, 0.0]));
        let json = serde_json::to_string(&snap).unwrap();
        let back: ObserverSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }
}
