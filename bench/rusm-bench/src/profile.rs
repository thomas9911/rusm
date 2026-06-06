use serde::{Deserialize, Serialize};

/// How hard a benchmark drives the machine — a performance dial. Each preset is
/// a number of spawn workers; throughput rises with the tier. Spawning is
/// **reap-bound** (the limit is how fast finished processes drain, ~one reaper
/// core's worth each), so the real lever is the *spawner-to-reaper balance*:
/// too few spawners under-drives the machine, too many starve the reapers and
/// pile processes up. `Max` sits at the balance point (≈half the cores spawn,
/// half reap) for peak *smooth* throughput.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceProfile {
    Light,
    Balanced,
    Max,
}

/// Display metadata for a [`ResourceProfile`], sent to the dashboard selector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceProfileMeta {
    pub id: String,
    pub label: String,
    pub description: String,
}

impl Default for ResourceProfile {
    fn default() -> Self {
        ResourceProfile::Balanced
    }
}

/// Live-process safety net, per core. The profiles self-limit their population
/// far below this through their worker count alone; the cap is not an operating
/// point — it only exists so that on a machine where spawning outpaces reaping
/// the process table can never grow without bound.
const SAFETY_CAP_PER_CORE: usize = 5_000;

impl ResourceProfile {
    pub const ALL: [ResourceProfile; 3] = [
        ResourceProfile::Light,
        ResourceProfile::Balanced,
        ResourceProfile::Max,
    ];

    pub fn id(self) -> &'static str {
        match self {
            ResourceProfile::Light => "light",
            ResourceProfile::Balanced => "balanced",
            ResourceProfile::Max => "max",
        }
    }

    pub fn from_id(id: &str) -> Option<ResourceProfile> {
        ResourceProfile::ALL.into_iter().find(|p| p.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            ResourceProfile::Light => "Light",
            ResourceProfile::Balanced => "Balanced",
            ResourceProfile::Max => "Max",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            ResourceProfile::Light => "Gentle — about a quarter of the cores spawn, so throughput is deliberately modest. Use it when raw speed isn't the point and you want the machine left alone.",
            ResourceProfile::Balanced => "Good throughput with headroom — about 40% of the cores spawn. Fast, but kept short of the peak so there's visibly room to push higher.",
            ResourceProfile::Max => "Most performant — spawners balanced against reapers (about half the cores each) for peak sustained throughput. The live population self-limits to a few hundred, so it stays smooth: fastest, with no pile-up.",
        }
    }

    /// Resolves to `(spawn_workers, spawn_max_in_flight)` for `cores` available
    /// CPU cores. The **worker count is the throughput dial** and is relative to
    /// the machine: `Light` ≈ ¼, `Balanced` ≈ ⅖, `Max` ≈ ½ of the cores — `Max`
    /// spends the other half reaping, which is the sustained-throughput peak and
    /// is why it never saturates the whole machine. The cap is a uniform per-core
    /// safety net (see [`SAFETY_CAP_PER_CORE`]), not a per-tier knob.
    pub fn tuning(self, cores: usize) -> (usize, usize) {
        let cores = cores.max(1);
        let workers = match self {
            ResourceProfile::Light => cores / 4,
            ResourceProfile::Balanced => cores * 2 / 5,
            ResourceProfile::Max => cores / 2,
        };
        (workers.max(1), cores * SAFETY_CAP_PER_CORE)
    }

    pub fn meta(self) -> ResourceProfileMeta {
        ResourceProfileMeta {
            id: self.id().to_string(),
            label: self.label().to_string(),
            description: self.description().to_string(),
        }
    }

    pub fn all_meta() -> Vec<ResourceProfileMeta> {
        ResourceProfile::ALL
            .into_iter()
            .map(ResourceProfile::meta)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_default_is_balanced() {
        assert_eq!(ResourceProfile::default(), ResourceProfile::Balanced);
        for p in ResourceProfile::ALL {
            assert_eq!(ResourceProfile::from_id(p.id()), Some(p));
        }
        assert_eq!(ResourceProfile::from_id("nope"), None);
    }

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

    #[test]
    fn meta_is_populated_for_all() {
        let metas = ResourceProfile::all_meta();
        assert_eq!(metas.len(), 3);
        for m in metas {
            assert!(ResourceProfile::from_id(&m.id).is_some());
            assert!(!m.label.is_empty() && !m.description.is_empty());
        }
    }

    #[test]
    fn meta_round_trips_through_json() {
        let metas = ResourceProfile::all_meta();
        let json = serde_json::to_string(&metas).unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<ResourceProfileMeta>>(&json).unwrap(),
            metas
        );
    }
}
