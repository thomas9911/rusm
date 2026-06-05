use rusm_observer::{ProcessInfo, ProcessStatus};

use crate::scenario::Scenario;

/// One tick of synthetic load for a scenario: the signals the runner records.
#[derive(Debug, Clone, PartialEq)]
pub struct SyntheticTick {
    pub ops_per_sec: f64,
    pub peak_concurrent: u64,
    pub latencies_ns: Vec<u64>,
    pub processes: Vec<ProcessInfo>,
    pub scheduler_load: Vec<f32>,
}

/// Deterministic synthetic data source.
///
/// Output is a pure function of `(scenario, tick)` — no wall clock, no RNG state
/// shared across ticks — so the dashboard shows lively, scenario-shaped data in
/// Phase 0 and every test is reproducible. Real measurements replace this source
/// per scenario as the runtime lands (see [`Scenario::meta`]).
#[derive(Debug, Clone, Copy)]
pub struct SyntheticSource {
    scenario: Scenario,
}

struct Ranges {
    ops: (u64, u64),
    latency_ns: (u64, u64),
    processes: (u64, u64),
}

/// SplitMix64 finalizer — maps a counter to a well-distributed pseudo-random u64.
fn mix(z: u64) -> u64 {
    let mut x = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        mix(self.state)
    }

    fn next_unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in the inclusive range `[lo, hi]`.
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        debug_assert!(lo <= hi);
        lo + self.next_u64() % (hi - lo + 1)
    }
}

impl SyntheticSource {
    pub fn new(scenario: Scenario) -> Self {
        Self { scenario }
    }

    pub fn scenario(self) -> Scenario {
        self.scenario
    }

    pub fn tick(
        self,
        tick: u64,
        latency_samples: usize,
        max_processes: usize,
        scheduler_count: usize,
    ) -> SyntheticTick {
        let mut rng = Rng::new(mix(self.scenario as u64).wrapping_add(tick));
        let r = self.ranges();

        let ops_per_sec = rng.range(r.ops.0, r.ops.1) as f64;
        let peak_concurrent = rng.range(r.processes.0, r.processes.1);
        let latencies_ns = (0..latency_samples)
            .map(|_| rng.range(r.latency_ns.0, r.latency_ns.1))
            .collect();

        let detail = (peak_concurrent as usize).min(max_processes);
        let processes = (0..detail)
            .map(|i| ProcessInfo {
                id: i as u64,
                name: None,
                status: status_from(rng.next_unit()),
                mailbox_depth: rng.range(0, 16) as u32,
                memory_bytes: rng.range(64 * 1024, 1024 * 1024),
                reductions: rng.next_u64() % 1_000_000,
            })
            .collect();

        let scheduler_load = (0..scheduler_count)
            .map(|_| rng.next_unit() as f32)
            .collect();

        SyntheticTick {
            ops_per_sec,
            peak_concurrent,
            latencies_ns,
            processes,
            scheduler_load,
        }
    }

    fn ranges(self) -> Ranges {
        match self.scenario {
            Scenario::SpawnStorm => Ranges {
                ops: (250_000, 350_000),
                latency_ns: (1_000, 20_000),
                processes: (5_000, 50_000),
            },
            Scenario::PingPong => Ranges {
                ops: (500_000, 2_000_000),
                latency_ns: (200, 2_000),
                processes: (2, 2),
            },
            Scenario::Fairness => Ranges {
                ops: (10_000, 50_000),
                latency_ns: (1_000, 10_000),
                processes: (8, 64),
            },
            Scenario::FaultRecovery => Ranges {
                ops: (1_000, 10_000),
                latency_ns: (100_000, 2_000_000),
                processes: (10, 100),
            },
            Scenario::ConnectionStorm => Ranges {
                ops: (280_000, 340_000),
                latency_ns: (50_000, 500_000),
                processes: (1_000, 5_000),
            },
            Scenario::DistributedFanout => Ranges {
                ops: (50_000, 150_000),
                latency_ns: (200_000, 2_000_000),
                processes: (100, 1_000),
            },
        }
    }
}

fn status_from(unit: f64) -> ProcessStatus {
    match unit {
        u if u < 0.80 => ProcessStatus::Running,
        u if u < 0.92 => ProcessStatus::Waiting,
        u if u < 0.98 => ProcessStatus::Sleeping,
        _ => ProcessStatus::Crashed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_deterministic_for_same_inputs() {
        let src = SyntheticSource::new(Scenario::ConnectionStorm);
        assert_eq!(src.tick(7, 32, 100, 4), src.tick(7, 32, 100, 4));
    }

    #[test]
    fn different_ticks_differ() {
        let src = SyntheticSource::new(Scenario::ConnectionStorm);
        assert_ne!(src.tick(1, 32, 100, 4), src.tick(2, 32, 100, 4));
    }

    #[test]
    fn different_scenarios_differ() {
        let a = SyntheticSource::new(Scenario::SpawnStorm).tick(1, 8, 10, 2);
        let b = SyntheticSource::new(Scenario::PingPong).tick(1, 8, 10, 2);
        assert_ne!(a.ops_per_sec, b.ops_per_sec);
    }

    #[test]
    fn every_scenario_produces_signals_in_its_ranges() {
        for scenario in Scenario::ALL {
            let src = SyntheticSource::new(scenario);
            assert_eq!(src.scenario(), scenario);
            let r = src.ranges();
            for tick in 0..100 {
                let t = src.tick(tick, 16, 10, 2);
                assert!((r.ops.0..=r.ops.1).contains(&(t.ops_per_sec as u64)));
                assert!((r.processes.0..=r.processes.1).contains(&t.peak_concurrent));
                for l in &t.latencies_ns {
                    assert!((r.latency_ns.0..=r.latency_ns.1).contains(l));
                }
            }
        }
    }

    #[test]
    fn sample_sizes_are_honoured() {
        let t = SyntheticSource::new(Scenario::SpawnStorm).tick(0, 24, 12, 3);
        assert_eq!(t.latencies_ns.len(), 24);
        assert_eq!(t.scheduler_load.len(), 3);
        // SpawnStorm has thousands of processes, but detail is capped.
        assert_eq!(t.processes.len(), 12);
    }

    #[test]
    fn detail_is_capped_by_peak_when_smaller_than_max() {
        // PingPong only ever has 2 processes, below the max_processes cap.
        let t = SyntheticSource::new(Scenario::PingPong).tick(0, 4, 100, 2);
        assert_eq!(t.peak_concurrent, 2);
        assert_eq!(t.processes.len(), 2);
    }

    #[test]
    fn scheduler_load_is_in_unit_interval() {
        let t = SyntheticSource::new(Scenario::Fairness).tick(3, 4, 10, 8);
        for load in t.scheduler_load {
            assert!((0.0..=1.0).contains(&load));
        }
    }

    #[test]
    fn status_distribution_covers_all_variants() {
        assert_eq!(status_from(0.0), ProcessStatus::Running);
        assert_eq!(status_from(0.85), ProcessStatus::Waiting);
        assert_eq!(status_from(0.95), ProcessStatus::Sleeping);
        assert_eq!(status_from(0.999), ProcessStatus::Crashed);
    }
}
