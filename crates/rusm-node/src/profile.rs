use serde::{Deserialize, Serialize};

/// The machine-usage tier a node runs at — how much of the host it claims. A
/// performance dial: `Light` leaves the machine largely free, `Max` drives it
/// hardest while still leaving headroom. The concrete effect of a tier (thread
/// counts, worker balance) is decided by whatever consumes the profile; this
/// type is just the tier and its display metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceProfile {
    Light,
    #[default]
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
            ResourceProfile::Light => "Gentle — claims about a quarter of the machine's cores. Use it when you want the node to leave the host largely free.",
            ResourceProfile::Balanced => "Balanced — claims roughly two-fifths of the cores: responsive, with clear headroom still left on the machine.",
            ResourceProfile::Max => "Most performant — claims about half the cores for peak sustained work while leaving the rest free, so the node stays smooth under load.",
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
