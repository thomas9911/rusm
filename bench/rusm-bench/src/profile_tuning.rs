//! The **benchmark** interpretation of a [`ResourceProfile`]: how many spawn
//! workers a tier drives the machine with. This is benchmark-specific (it sets
//! the spawn-storm's spawner/reaper balance), so it lives here rather than in the
//! generic, benchmark-free `rusm-node`.

use rusm_node::ResourceProfile;

/// Live-process safety net, per core. The profiles self-limit their population
/// far below this through their worker count alone; the cap is not an operating
/// point — it only exists so that on a machine where spawning outpaces reaping
/// the process table can never grow without bound.
const SAFETY_CAP_PER_CORE: usize = 5_000;

/// Benchmark spawn tuning for a [`ResourceProfile`] tier.
pub trait ProfileTuning {
    /// Resolves to `(spawn_workers, spawn_max_in_flight)` for `cores` available
    /// CPU cores. The **worker count is the throughput dial** and is relative to
    /// the machine: `Light` ≈ ¼, `Balanced` ≈ ⅖, `Max` ≈ ½ of the cores — `Max`
    /// spends the other half reaping, which is the sustained-throughput peak and
    /// is why it never saturates the whole machine. The cap is a uniform per-core
    /// safety net (see [`SAFETY_CAP_PER_CORE`]), not a per-tier knob.
    fn tuning(self, cores: usize) -> (usize, usize);
}

impl ProfileTuning for ResourceProfile {
    fn tuning(self, cores: usize) -> (usize, usize) {
        let cores = cores.max(1);
        let workers = match self {
            ResourceProfile::Light => cores / 4,
            ResourceProfile::Balanced => cores * 2 / 5,
            ResourceProfile::Max => cores / 2,
        };
        (workers.max(1), cores * SAFETY_CAP_PER_CORE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workers_rise_per_tier_and_max_balances_spawners_with_reapers() {
        let cores = 20;
        let (lw, lc) = ResourceProfile::Light.tuning(cores);
        let (bw, bc) = ResourceProfile::Balanced.tuning(cores);
        let (mw, mc) = ResourceProfile::Max.tuning(cores);
        // The worker count is the throughput dial — strictly rising per tier.
        assert!(lw < bw && bw < mw);
        // Max spawns on ~half the cores, leaving the other half to reap; it never
        // claims the whole machine (that would starve the reaper and pile up).
        assert_eq!(mw, cores / 2);
        assert!(mw < cores);
        // The cap is a uniform safety net, not a per-tier operating point.
        assert_eq!(lc, bc);
        assert_eq!(bc, mc);
        assert!(mc > 0);
    }

    #[test]
    fn max_always_leaves_cores_to_reap() {
        assert_eq!(ResourceProfile::Max.tuning(10).0, 5); // half spawn, half reap
        assert_eq!(ResourceProfile::Max.tuning(1).0, 1); // 1 core → 1 (can't do better)
        assert!(ResourceProfile::Max.tuning(64).0 < 64); // always leaves reapers
    }

    #[test]
    fn tuning_handles_zero_cores() {
        assert_eq!(ResourceProfile::Balanced.tuning(0).0, 1);
        assert_eq!(ResourceProfile::Light.tuning(0).0, 1);
        assert_eq!(ResourceProfile::Max.tuning(0).0, 1);
    }

    #[test]
    fn safety_cap_is_relative_to_cores() {
        // Twice the cores → twice the safety allowance (same for every tier).
        assert_eq!(
            ResourceProfile::Max.tuning(16).1,
            2 * ResourceProfile::Max.tuning(8).1
        );
    }
}
