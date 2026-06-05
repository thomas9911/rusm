use rusm_metrics::{LatencyHistogram, TimeSeries};
use rusm_observer::{NodeSample, Observer};

use crate::protocol::Frame;
use crate::scenario::Scenario;
use crate::synthetic::SyntheticSource;

#[derive(Debug, Clone, Copy)]
pub struct RunnerConfig {
    pub scheduler_count: usize,
    pub max_detail: usize,
    pub latency_samples: usize,
    pub throughput_window: usize,
    /// Sampling rate; ties `ops_per_sec` to a per-tick operation count.
    pub ticks_per_second: u32,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            scheduler_count: 8,
            max_detail: 64,
            latency_samples: 64,
            throughput_window: 120,
            ticks_per_second: 20,
        }
    }
}

struct RunState {
    source: SyntheticSource,
    tick: u64,
    peak_concurrent: u64,
}

/// Drives a benchmark run and aggregates each tick into a [`Frame`].
///
/// The runner is the synchronous heart of the harness: a transport (the
/// WebSocket server, or the terminal runner) owns the clock and calls
/// [`Runner::tick`] on a cadence, broadcasting the returned frame. Keeping it
/// clock-free makes the whole pipeline deterministic and unit-testable.
pub struct Runner {
    config: RunnerConfig,
    observer: Observer,
    latency: LatencyHistogram,
    throughput: TimeSeries,
    run: Option<RunState>,
}

impl Runner {
    pub fn new(config: RunnerConfig) -> Self {
        Self {
            observer: fresh_observer(&config, true),
            latency: LatencyHistogram::new(),
            throughput: TimeSeries::new(config.throughput_window),
            run: None,
            config,
        }
    }

    pub fn is_running(&self) -> bool {
        self.run.is_some()
    }

    pub fn scenario(&self) -> Option<Scenario> {
        self.run.as_ref().map(|r| r.source.scenario())
    }

    /// Starts (or restarts) `scenario`, resetting all metrics to a clean slate.
    pub fn start(&mut self, scenario: Scenario) {
        let detail = self.observer.detail_enabled();
        self.observer = fresh_observer(&self.config, detail);
        self.latency.clear();
        self.throughput.clear();
        self.run = Some(RunState {
            source: SyntheticSource::new(scenario),
            tick: 0,
            peak_concurrent: 0,
        });
    }

    pub fn stop(&mut self) {
        self.run = None;
    }

    pub fn set_observer_detail(&self, enabled: bool) {
        self.observer.set_detail_enabled(enabled);
    }

    pub fn observer_detail_enabled(&self) -> bool {
        self.observer.detail_enabled()
    }

    pub fn tick(&mut self, uptime_ms: u64) -> Frame {
        let Some(state) = self.run.as_mut() else {
            let idle = NodeSample {
                process_count: 0,
                running: 0,
                waiting: 0,
                total_memory_bytes: 0,
                scheduler_load: &[],
                processes: &[],
            };
            return Frame {
                scenario: None,
                running: false,
                uptime_ms,
                ops_per_sec: 0.0,
                peak_concurrent: 0,
                latency: self.latency.snapshot(),
                throughput: self.throughput.snapshot(),
                observer: self.observer.snapshot(uptime_ms, idle),
            };
        };

        let t = state.source.tick(
            state.tick,
            self.config.latency_samples,
            self.config.max_detail,
            self.config.scheduler_count,
        );
        state.tick += 1;
        state.peak_concurrent = state.peak_concurrent.max(t.process_count);
        let scenario = state.source.scenario();
        let peak_concurrent = state.peak_concurrent;

        for latency_ns in &t.latencies_ns {
            self.latency.record_nanos(*latency_ns);
        }
        self.throughput.push(t.ops_per_sec);

        let ops_this_tick = (t.ops_per_sec / self.config.ticks_per_second as f64) as u64;
        self.observer.record_messages(ops_this_tick);

        let sample = NodeSample {
            process_count: t.process_count as usize,
            running: t.running as usize,
            waiting: t.waiting as usize,
            total_memory_bytes: t.total_memory_bytes,
            scheduler_load: &t.scheduler_load,
            processes: &t.processes,
        };

        Frame {
            scenario: Some(scenario.id().to_string()),
            running: true,
            uptime_ms,
            ops_per_sec: t.ops_per_sec,
            peak_concurrent,
            latency: self.latency.snapshot(),
            throughput: self.throughput.snapshot(),
            observer: self.observer.snapshot(uptime_ms, sample),
        }
    }
}

fn fresh_observer(config: &RunnerConfig, detail_enabled: bool) -> Observer {
    let observer = Observer::new(config.scheduler_count, config.max_detail);
    observer.set_detail_enabled(detail_enabled);
    observer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner() -> Runner {
        Runner::new(RunnerConfig::default())
    }

    #[test]
    fn starts_idle() {
        let r = runner();
        assert!(!r.is_running());
        assert_eq!(r.scenario(), None);
    }

    #[test]
    fn idle_tick_reports_not_running() {
        let mut r = runner();
        let frame = r.tick(10);
        assert!(!frame.running);
        assert_eq!(frame.scenario, None);
        assert_eq!(frame.ops_per_sec, 0.0);
        assert_eq!(frame.uptime_ms, 10);
    }

    #[test]
    fn start_then_tick_produces_running_frame() {
        let mut r = runner();
        r.start(Scenario::ConnectionStorm);
        assert!(r.is_running());
        assert_eq!(r.scenario(), Some(Scenario::ConnectionStorm));
        let frame = r.tick(50);
        assert!(frame.running);
        assert_eq!(frame.scenario.as_deref(), Some("connection-storm"));
        assert!(frame.ops_per_sec > 0.0);
        assert_eq!(
            frame.latency.count as usize,
            RunnerConfig::default().latency_samples
        );
        assert!(frame.observer.messages_total > 0);
    }

    #[test]
    fn peak_concurrent_is_monotonic_across_ticks() {
        let mut r = runner();
        r.start(Scenario::ConnectionStorm);
        let mut peak = 0;
        for tick in 0..20 {
            let frame = r.tick(tick);
            assert!(frame.peak_concurrent >= peak);
            peak = frame.peak_concurrent;
        }
    }

    #[test]
    fn stop_returns_to_idle() {
        let mut r = runner();
        r.start(Scenario::PingPong);
        r.stop();
        assert!(!r.is_running());
        assert!(!r.tick(0).running);
    }

    #[test]
    fn restart_resets_metrics() {
        let mut r = runner();
        r.start(Scenario::PingPong);
        for tick in 0..10 {
            r.tick(tick);
        }
        r.start(Scenario::SpawnStorm);
        // A fresh run starts with empty latency history until the first tick.
        let frame = r.tick(0);
        assert_eq!(frame.scenario.as_deref(), Some("spawn-storm"));
        assert_eq!(
            frame.latency.count as usize,
            RunnerConfig::default().latency_samples
        );
    }

    #[test]
    fn observer_detail_toggle_persists_across_restart() {
        let mut r = runner();
        r.set_observer_detail(false);
        r.start(Scenario::SpawnStorm);
        assert!(!r.observer_detail_enabled());
        let frame = r.tick(0);
        assert!(frame.observer.processes.is_empty());
        assert!(frame.observer.process_count > 0);
    }

    #[test]
    fn throughput_window_is_bounded() {
        let mut r = runner();
        r.start(Scenario::ConnectionStorm);
        for tick in 0..500 {
            r.tick(tick);
        }
        let frame = r.tick(500);
        assert!(frame.throughput.points.len() <= RunnerConfig::default().throughput_window);
    }
}
