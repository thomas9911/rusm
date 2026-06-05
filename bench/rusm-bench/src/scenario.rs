use serde::{Deserialize, Serialize};

/// A benchmark scenario the dashboard can run.
///
/// In Phase 0 every scenario is driven by synthetic data; `real_after_phase`
/// records the roadmap phase at which it switches to measuring the real runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scenario {
    SpawnStorm,
    PingPong,
    Fairness,
    FaultRecovery,
    ConnectionStorm,
    DistributedFanout,
}

/// Display metadata for a [`Scenario`], sent to the dashboard menu.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioMeta {
    pub id: String,
    pub label: String,
    pub description: String,
    pub real_after_phase: u8,
}

impl Scenario {
    pub const ALL: [Scenario; 6] = [
        Scenario::SpawnStorm,
        Scenario::PingPong,
        Scenario::Fairness,
        Scenario::FaultRecovery,
        Scenario::ConnectionStorm,
        Scenario::DistributedFanout,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Scenario::SpawnStorm => "spawn-storm",
            Scenario::PingPong => "ping-pong",
            Scenario::Fairness => "fairness",
            Scenario::FaultRecovery => "fault-recovery",
            Scenario::ConnectionStorm => "connection-storm",
            Scenario::DistributedFanout => "distributed-fanout",
        }
    }

    pub fn from_id(id: &str) -> Option<Scenario> {
        Scenario::ALL.into_iter().find(|s| s.id() == id)
    }

    pub fn meta(self) -> ScenarioMeta {
        let (label, description, real_after_phase) = match self {
            Scenario::SpawnStorm => (
                "Spawn storm",
                "Spawn processes as fast as possible; measures spawns/sec and per-process memory.",
                2,
            ),
            Scenario::PingPong => (
                "Message ping-pong",
                "Two processes bounce messages; measures messages/sec and round-trip latency.",
                5,
            ),
            Scenario::Fairness => (
                "Fairness under tight loop",
                "A CPU-bound process must not starve others; measures progress of bystanders.",
                5,
            ),
            Scenario::FaultRecovery => (
                "Fault recovery",
                "Crash supervised children; measures restarts/sec and recovery latency.",
                6,
            ),
            Scenario::ConnectionStorm => (
                "Connection storm (300k/s proof)",
                "Open connections as fast as possible; measures conns/sec, peak concurrent, latency.",
                8,
            ),
            Scenario::DistributedFanout => (
                "Distributed fan-out",
                "Send work across cluster nodes; measures cross-node message latency.",
                10,
            ),
        };
        ScenarioMeta {
            id: self.id().to_string(),
            label: label.to_string(),
            description: description.to_string(),
            real_after_phase,
        }
    }

    pub fn all_meta() -> Vec<ScenarioMeta> {
        Scenario::ALL.into_iter().map(Scenario::meta).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_round_trip() {
        let mut ids = std::collections::HashSet::new();
        for s in Scenario::ALL {
            let id = s.id();
            assert!(ids.insert(id), "duplicate id {id}");
            assert_eq!(Scenario::from_id(id), Some(s));
        }
    }

    #[test]
    fn from_id_rejects_unknown() {
        assert_eq!(Scenario::from_id("nope"), None);
    }

    #[test]
    fn serde_uses_kebab_case_id() {
        let json = serde_json::to_string(&Scenario::ConnectionStorm).unwrap();
        assert_eq!(json, "\"connection-storm\"");
        let back: Scenario = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Scenario::ConnectionStorm);
    }

    #[test]
    fn meta_matches_id_and_is_populated() {
        for meta in Scenario::all_meta() {
            assert!(Scenario::from_id(&meta.id).is_some());
            assert!(!meta.label.is_empty());
            assert!(!meta.description.is_empty());
            assert!((2..=10).contains(&meta.real_after_phase));
        }
    }

    #[test]
    fn all_meta_covers_every_scenario() {
        assert_eq!(Scenario::all_meta().len(), Scenario::ALL.len());
    }

    #[test]
    fn meta_round_trips_through_json() {
        let metas = Scenario::all_meta();
        let json = serde_json::to_string(&metas).unwrap();
        let back: Vec<ScenarioMeta> = serde_json::from_str(&json).unwrap();
        assert_eq!(metas, back);
    }
}
