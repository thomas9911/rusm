use rusm_metrics::{LatencyHistogram, TimeSeries};
use rusm_observer::{NodeSample, Observer};

use crate::componentstorm::ComponentStormEngine;
use crate::connectionstorm::ConnectionStormEngine;
use crate::engine::SpawnStormEngine;
use crate::fairness::FairnessEngine;
use crate::faultrecovery::FaultRecoveryEngine;
use crate::modulestorm::ModuleStormEngine;
use crate::pingpong::PingPongEngine;
use crate::profile::ResourceProfile;
use crate::protocol::Frame;
use crate::sample::Sample;
use crate::scenario::Scenario;
use crate::streampipe::StreamPipeEngine;
use crate::synthetic::SyntheticSource;

fn available_cores() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get())
}

#[derive(Debug, Clone, Copy)]
pub struct RunnerConfig {
    pub scheduler_count: usize,
    pub max_detail: usize,
    pub latency_samples: usize,
    pub throughput_window: usize,
    /// Sampling rate; ties `ops_per_sec` to a per-tick operation count.
    pub ticks_per_second: u32,
    /// Background spawner tasks for the real spawn-storm engine (one per core).
    pub spawn_workers: usize,
    /// Spawn-storm backpressure: max live processes before spawners pause.
    pub spawn_max_in_flight: usize,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        // Spawn knobs come from the default (Balanced) resource profile.
        let (spawn_workers, spawn_max_in_flight) =
            ResourceProfile::default().tuning(available_cores());
        Self {
            scheduler_count: 8,
            max_detail: 64,
            latency_samples: 64,
            throughput_window: 120,
            ticks_per_second: 20,
            spawn_workers,
            spawn_max_in_flight,
        }
    }
}

/// The per-scenario data source: synthetic for most scenarios, a real
/// `rusm-otp` spawn engine for spawn-storm.
enum Engine {
    Synthetic(SyntheticSource),
    SpawnStorm(SpawnStormEngine),
    PingPong(PingPongEngine),
    FaultRecovery(FaultRecoveryEngine),
    ConnectionStorm(ConnectionStormEngine),
    Fairness(FairnessEngine),
    ModuleStorm(ModuleStormEngine),
    ComponentStorm(ComponentStormEngine),
    StreamPipe(StreamPipeEngine),
}

impl Engine {
    fn for_scenario(scenario: Scenario, config: &RunnerConfig) -> Self {
        // Real engines scale their worker/pair/supervisor count with the resource
        // profile; the rest are still synthetic.
        match scenario {
            Scenario::SpawnStorm => Engine::SpawnStorm(SpawnStormEngine::new(
                config.spawn_workers,
                config.scheduler_count,
                config.spawn_max_in_flight,
            )),
            Scenario::PingPong => Engine::PingPong(PingPongEngine::new(
                config.spawn_workers,
                config.scheduler_count,
            )),
            Scenario::FaultRecovery => Engine::FaultRecovery(FaultRecoveryEngine::new(
                config.spawn_workers,
                config.scheduler_count,
            )),
            Scenario::ConnectionStorm => Engine::ConnectionStorm(ConnectionStormEngine::new(
                config.spawn_workers,
                config.scheduler_count,
            )),
            Scenario::Fairness => Engine::Fairness(FairnessEngine::new(
                config.spawn_workers,
                config.scheduler_count,
            )),
            Scenario::ModuleStorm => Engine::ModuleStorm(ModuleStormEngine::new(
                config.spawn_workers,
                config.scheduler_count,
            )),
            Scenario::ComponentStorm => Engine::ComponentStorm(ComponentStormEngine::new(
                config.spawn_workers,
                config.scheduler_count,
            )),
            Scenario::StreamPipe => Engine::StreamPipe(StreamPipeEngine::new(
                config.spawn_workers,
                config.scheduler_count,
            )),
            _ => Engine::Synthetic(SyntheticSource::new(scenario)),
        }
    }

    fn tick(&mut self, tick: u64, config: &RunnerConfig) -> Sample {
        match self {
            Engine::Synthetic(source) => source.tick(
                tick,
                config.latency_samples,
                config.max_detail,
                config.scheduler_count,
            ),
            Engine::SpawnStorm(engine) => engine.tick(),
            Engine::PingPong(engine) => engine.tick(),
            Engine::FaultRecovery(engine) => engine.tick(),
            Engine::ConnectionStorm(engine) => engine.tick(),
            Engine::Fairness(engine) => engine.tick(),
            Engine::ModuleStorm(engine) => engine.tick(),
            Engine::ComponentStorm(engine) => engine.tick(),
            Engine::StreamPipe(engine) => engine.tick(),
        }
    }
}

struct RunState {
    scenario: Scenario,
    engine: Engine,
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
    profile: ResourceProfile,
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
            profile: ResourceProfile::default(),
            config,
        }
    }

    pub fn resource_profile(&self) -> ResourceProfile {
        self.profile
    }

    /// Switches the resource profile — re-resolving how many spawn workers and
    /// how large an in-flight cap the storm uses — and re-applies it to a live
    /// spawn-storm run.
    pub fn set_resource_profile(&mut self, profile: ResourceProfile) {
        let (workers, cap) = profile.tuning(available_cores());
        self.config.spawn_workers = workers;
        self.config.spawn_max_in_flight = cap;
        self.profile = profile;
        // Restart whichever real engine is running so the new tuning takes effect.
        if let Some(
            scenario @ (Scenario::SpawnStorm
            | Scenario::PingPong
            | Scenario::FaultRecovery
            | Scenario::ConnectionStorm
            | Scenario::Fairness
            | Scenario::ModuleStorm
            | Scenario::ComponentStorm
            | Scenario::StreamPipe),
        ) = self.scenario()
        {
            self.start(scenario);
        }
    }

    pub fn is_running(&self) -> bool {
        self.run.is_some()
    }

    pub fn scenario(&self) -> Option<Scenario> {
        self.run.as_ref().map(|r| r.scenario)
    }

    /// Starts (or restarts) `scenario`, resetting all metrics to a clean slate.
    pub fn start(&mut self, scenario: Scenario) {
        let detail = self.observer.detail_enabled();
        self.observer = fresh_observer(&self.config, detail);
        self.latency.clear();
        self.throughput.clear();
        self.run = Some(RunState {
            scenario,
            engine: Engine::for_scenario(scenario, &self.config),
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
                profile: self.profile.id().to_string(),
                latency: self.latency.snapshot(),
                throughput: self.throughput.snapshot(),
                observer: self.observer.snapshot(uptime_ms, idle),
            };
        };

        let t = state.engine.tick(state.tick, &self.config);
        state.tick += 1;
        state.peak_concurrent = state.peak_concurrent.max(t.process_count);
        let scenario = state.scenario;
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
            profile: self.profile.id().to_string(),
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
        r.start(Scenario::DistributedFanout);
        assert!(r.is_running());
        assert_eq!(r.scenario(), Some(Scenario::DistributedFanout));
        let frame = r.tick(50);
        assert!(frame.running);
        assert_eq!(frame.scenario.as_deref(), Some("distributed-fanout"));
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
        r.start(Scenario::DistributedFanout);
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
        r.start(Scenario::DistributedFanout);
        r.stop();
        assert!(!r.is_running());
        assert!(!r.tick(0).running);
    }

    #[test]
    fn restart_resets_metrics() {
        let mut r = runner();
        r.start(Scenario::DistributedFanout);
        for tick in 0..10 {
            r.tick(tick);
        }
        r.start(Scenario::DistributedFanout);
        // A fresh run accumulates from scratch — one tick of synthetic samples.
        let frame = r.tick(0);
        assert_eq!(frame.scenario.as_deref(), Some("distributed-fanout"));
        assert_eq!(
            frame.latency.count as usize,
            RunnerConfig::default().latency_samples
        );
    }

    #[test]
    fn observer_detail_toggle_persists_across_restart() {
        let mut r = runner();
        r.set_observer_detail(false);
        r.start(Scenario::DistributedFanout);
        assert!(!r.observer_detail_enabled());
        let frame = r.tick(0);
        assert!(frame.observer.processes.is_empty());
        assert!(frame.observer.process_count > 0);
    }

    #[test]
    fn resource_profile_defaults_to_balanced_and_can_change() {
        let mut r = runner();
        assert_eq!(r.resource_profile(), ResourceProfile::Balanced);
        r.set_resource_profile(ResourceProfile::Light);
        assert_eq!(r.resource_profile(), ResourceProfile::Light);
        // Reflected in the frame.
        assert_eq!(r.tick(0).profile, "light");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn spawn_storm_runs_real_processes() {
        // The spawn-storm scenario uses the real rusm-otp engine (continuous
        // background spawners, needs a Tokio runtime), unlike the synthetic ones.
        let mut r = runner();
        r.start(Scenario::SpawnStorm);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await; // warm up
        let frame = r.tick(50);
        assert_eq!(frame.scenario.as_deref(), Some("spawn-storm"));
        assert!(frame.ops_per_sec > 0.0);
        assert!(frame.latency.count > 0);

        // Changing the profile mid-run restarts the storm with the new tuning.
        r.set_resource_profile(ResourceProfile::Max);
        assert_eq!(r.scenario(), Some(Scenario::SpawnStorm));
        assert_eq!(r.resource_profile(), ResourceProfile::Max);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ping_pong_runs_real_processes() {
        // The ping-pong scenario bounces messages between real rusm-otp process
        // pairs, unlike the synthetic scenarios.
        let mut r = runner();
        r.start(Scenario::PingPong);
        tokio::time::sleep(std::time::Duration::from_millis(80)).await; // warm up
        let frame = r.tick(50);
        assert_eq!(frame.scenario.as_deref(), Some("ping-pong"));
        assert!(frame.ops_per_sec > 0.0);
        assert!(frame.observer.process_count >= 2); // at least one live pair
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fault_recovery_runs_real_supervisors() {
        // Real supervisors trap exits and restart crashing children.
        let mut r = runner();
        r.start(Scenario::FaultRecovery);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await; // warm up
        let frame = r.tick(50);
        assert_eq!(frame.scenario.as_deref(), Some("fault-recovery"));
        assert!(frame.ops_per_sec > 0.0); // restarts/sec
        assert!(frame.observer.process_count >= 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn connection_storm_runs_real_tcp() {
        // Real loopback TCP, one process per accepted connection.
        let mut r = runner();
        r.start(Scenario::ConnectionStorm);
        tokio::time::sleep(std::time::Duration::from_millis(150)).await; // ramp up
        let frame = r.tick(50);
        assert_eq!(frame.scenario.as_deref(), Some("connection-storm"));
        assert!(frame.ops_per_sec > 0.0); // connections/sec
        assert!(frame.observer.process_count > 1); // live connections + acceptor
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fairness_runs_real_wasm_and_bystanders_progress() {
        // Real Wasm spinners + bystanders; epoch preemption keeps bystanders going.
        // Poll until progress shows (robust to scheduling/parallel-test load).
        let mut r = runner();
        r.start(Scenario::Fairness);
        let mut frame = r.tick(0);
        let mut progressed = false;
        for tick in 1..=200 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            frame = r.tick(tick);
            if frame.ops_per_sec > 0.0 {
                progressed = true;
                break;
            }
        }
        assert_eq!(frame.scenario.as_deref(), Some("fairness"));
        assert!(progressed, "bystanders progressed despite spinners");
        assert!(frame.observer.process_count >= 2);
    }

    #[test]
    fn throughput_window_is_bounded() {
        let mut r = runner();
        r.start(Scenario::DistributedFanout);
        for tick in 0..500 {
            r.tick(tick);
        }
        let frame = r.tick(500);
        assert!(frame.throughput.points.len() <= RunnerConfig::default().throughput_window);
    }
}
