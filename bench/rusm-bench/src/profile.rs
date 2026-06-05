use serde::{Deserialize, Serialize};

/// How hard a benchmark may drive the machine. Friendly presets instead of raw
/// knobs — each maps to a number of spawn workers and an in-flight cap.
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

/// The hard CPU ceiling: at most 90% of cores, and always at least one core left
/// free, so a storm can never numb the whole machine.
fn max_workers(cores: usize) -> usize {
    let ninety_percent = cores * 90 / 100;
    ninety_percent.clamp(1, cores.saturating_sub(1).max(1))
}

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
            ResourceProfile::Light => "Gentle — about a quarter of cores, ~1k live processes per core.",
            ResourceProfile::Balanced => "Default — about half the cores, ~5k live processes per core.",
            ResourceProfile::Max => "Aggressive — up to 90% of cores (always leaves headroom so the system stays responsive), ~50k live per core.",
        }
    }

    /// Resolves to `(spawn_workers, spawn_max_in_flight)` for `cores` available
    /// CPU cores. Everything is **relative to the machine**: workers are a
    /// fraction of cores and the in-flight cap is a per-core allowance. Two
    /// safety guarantees hold regardless: even `Max` is **hard-capped at 90% of
    /// cores** (never saturate the whole machine), and the cap is never unbounded
    /// (memory stays bounded).
    pub fn tuning(self, cores: usize) -> (usize, usize) {
        let cores = cores.max(1);
        match self {
            ResourceProfile::Light => ((cores / 4).max(1), cores * 1_000),
            ResourceProfile::Balanced => ((cores / 2).max(1), cores * 5_000),
            ResourceProfile::Max => (max_workers(cores), cores * 50_000),
        }
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
    fn tuning_scales_and_max_leaves_headroom() {
        let cores = 20;
        let (lw, lc) = ResourceProfile::Light.tuning(cores);
        let (bw, bc) = ResourceProfile::Balanced.tuning(cores);
        let (mw, mc) = ResourceProfile::Max.tuning(cores);
        assert!(lw < bw && bw < mw); // three distinct CPU tiers
        assert!(lc < bc && bc < mc); // three distinct memory caps
        assert!(mw < cores); // hard 90% cap — never the whole machine
        assert!(mc > 0); // never unbounded
    }

    #[test]
    fn max_always_leaves_a_core_free_except_on_a_single_core_box() {
        assert_eq!(ResourceProfile::Max.tuning(2).0, 1); // 2 cores → 1 worker, 1 free
        assert_eq!(ResourceProfile::Max.tuning(1).0, 1); // 1 core → 1 (can't do better)
        assert!(ResourceProfile::Max.tuning(64).0 <= 58); // ~90%, well under 64
    }

    #[test]
    fn tuning_handles_zero_cores() {
        assert_eq!(ResourceProfile::Balanced.tuning(0).0, 1);
        assert_eq!(ResourceProfile::Light.tuning(0).0, 1);
    }

    #[test]
    fn caps_are_relative_to_cores() {
        // Twice the cores → twice the in-flight allowance.
        assert_eq!(
            ResourceProfile::Balanced.tuning(16).1,
            2 * ResourceProfile::Balanced.tuning(8).1
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
