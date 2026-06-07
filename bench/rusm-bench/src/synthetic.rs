use rusm_observer::{ProcessInfo, ProcessStatus};

use crate::sample::Sample;
use crate::scenario::Scenario;

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
    ) -> Sample {
        let mut rng = Rng::new(mix(self.scenario as u64).wrapping_add(tick));
        let r = self.ranges();

        let ops_per_sec = rng.range(r.ops.0, r.ops.1) as f64;
        let process_count = rng.range(r.processes.0, r.processes.1);
        let latencies_ns = (0..latency_samples)
            .map(|_| rng.range(r.latency_ns.0, r.latency_ns.1))
            .collect();

        // The detail table is only a sample of the (possibly huge) process set.
        let sample_size = (process_count as usize).min(max_processes);
        let processes: Vec<ProcessInfo> = (0..sample_size)
            .map(|i| ProcessInfo {
                id: i as u64,
                name: None,
                status: status_from(rng.next_unit()),
                mailbox_depth: rng.range(0, 16) as u32,
                // Lightweight: a small Wasm-instance heap (KB-scale, like a BEAM
                // process), so tens of thousands of processes stay well under a few GiB.
                memory_bytes: rng.range(4 * 1024, 64 * 1024),
                reductions: rng.next_u64() % 1_000_000,
            })
            .collect();

        // Aggregates are at full scale: per-process memory averaged over the
        // sample, multiplied by the true count; status split ~80% / ~12%.
        let avg_memory = processes
            .iter()
            .map(|p| p.memory_bytes)
            .sum::<u64>()
            .checked_div(processes.len() as u64)
            .unwrap_or(0);
        let total_memory_bytes = avg_memory.saturating_mul(process_count);
        let running = process_count * 4 / 5;
        let waiting = process_count * 3 / 25;

        let scheduler_load = (0..scheduler_count)
            .map(|_| rng.next_unit() as f32)
            .collect();

        Sample {
            ops_per_sec,
            process_count,
            running,
            waiting,
            total_memory_bytes,
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
            // Real from Phase 7, so this synthetic profile is only a placeholder;
            // shaped like the spawn storm (component instantiation throughput).
            Scenario::ComponentStorm => Ranges {
                ops: (200_000, 400_000),
                latency_ns: (1_000, 20_000),
                processes: (50, 100),
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
                assert!((r.processes.0..=r.processes.1).contains(&t.process_count));
                assert!(t.running <= t.process_count);
                assert!(t.waiting <= t.process_count);
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
    fn detail_is_capped_by_count_when_smaller_than_max() {
        // PingPong only ever has 2 processes, below the max_processes cap.
        let t = SyntheticSource::new(Scenario::PingPong).tick(0, 4, 100, 2);
        assert_eq!(t.process_count, 2);
        assert_eq!(t.processes.len(), 2);
    }

    #[test]
    fn aggregates_are_full_scale_not_the_sample_size() {
        // Regression guard: a thousands-of-processes scenario must report the true
        // count and a total memory far larger than the small sampled table.
        let t = SyntheticSource::new(Scenario::SpawnStorm).tick(0, 4, 16, 4);
        assert!(t.process_count >= 5_000);
        assert_eq!(t.processes.len(), 16);
        assert!(t.process_count > t.processes.len() as u64);
        let sample_memory: u64 = t.processes.iter().map(|p| p.memory_bytes).sum();
        assert!(t.total_memory_bytes > sample_memory);
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
