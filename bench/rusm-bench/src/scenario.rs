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

/// Display metadata for a [`Scenario`], sent to the dashboard menu and the
/// per-scenario explanation panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioMeta {
    pub id: String,
    pub label: String,
    /// One-line summary (shown in the menu).
    pub description: String,
    /// Longer explanation bullets: what it stresses, the headline metric, what to
    /// watch, and the background — shown above the metrics.
    pub details: Vec<String>,
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
        let (label, description, details, real_after_phase) = match self {
            Scenario::SpawnStorm => (
                "Spawn storm",
                "Spawn processes as fast as possible; measures spawns/sec and per-process memory.",
                vec![
                    "Spawns processes as fast as the runtime allows — the raw creation throughput test.",
                    "Headline: spawns/sec. A RUSM process is one isolated Wasm instance plus one Tokio task, so spawn cost is instantiate + schedule.",
                    "These are NOT OS processes or threads: hundreds of thousands run cooperatively over a handful of OS threads — the schedulers (roughly one per CPU core).",
                    "Watch per-process memory: cheap, small processes are what make massive concurrency viable — the BEAM runs millions; so should we.",
                    "Why it matters: if spawning is cheap, you can model every request/connection/job as its own crash-isolated process.",
                ],
                1,
            ),
            Scenario::PingPong => (
                "Message ping-pong",
                "Two processes bounce messages; measures messages/sec and round-trip latency.",
                vec![
                    "Two processes bounce a message back and forth — the mailbox and scheduler hot path.",
                    "Headline: messages/sec and round-trip latency.",
                    "Messages are copied across isolated memories by the host; nothing is shared, exactly like Erlang.",
                    "Low, stable round-trip latency under load means wakeups are cheap and scheduling is fair.",
                ],
                2,
            ),
            Scenario::Fairness => (
                "Fairness under tight loop",
                "A CPU-bound process must not starve others; measures progress of bystanders.",
                vec![
                    "Runs a CPU-bound, tight-loop process alongside others that must keep making progress.",
                    "Headline: do the bystanders keep running? This proves preemption works.",
                    "Tokio scheduling is cooperative; RUSM adds Wasmtime epoch interruption so even an infinite loop yields — the analogue of the BEAM's reduction counting.",
                    "If bystanders stall, one hot process could starve the system. They must not.",
                ],
                6,
            ),
            Scenario::FaultRecovery => (
                "Fault recovery",
                "Crash supervised children; measures restarts/sec and recovery latency.",
                vec![
                    "Deliberately crashes supervised child processes and watches them come back.",
                    "Headline: restarts/sec and recovery latency — \"let it crash\" in action.",
                    "A trap tears down only the failing Wasm instance; a supervisor restarts a clean one and the rest of the system never notices.",
                    "Background: per-process isolation is what makes crashes survivable rather than fatal.",
                ],
                3,
            ),
            Scenario::ConnectionStorm => (
                "Connection storm",
                "Open connections as fast as possible; measures conns/sec, peak concurrent, latency.",
                vec![
                    "Opens connections as fast as possible — the headline benchmark of the whole project.",
                    "Headline: connections/sec, peak concurrent, and accept latency.",
                    "Goal: prove ~300k/s (hopefully more) — Lunatic's famous number, on a years-old laptop.",
                    "Each connection gets its own lightweight process; cheap spawn + async I/O is the entire game.",
                    "Tip: compare observer-on vs observer-off to confirm live introspection is nearly free.",
                ],
                5,
            ),
            Scenario::DistributedFanout => (
                "Distributed fan-out",
                "Send work across cluster nodes; measures cross-node message latency.",
                vec![
                    "Fans work out across cluster nodes and measures the cost of crossing the wire.",
                    "Cluster: a handful of independent nodes — each its own OS process, typically on a separate machine or CPU core (simulated in Phase 0).",
                    "Headline: cross-node message latency over the QUIC + TLS transport.",
                    "Processes spawn and message across nodes transparently — distributed Erlang, for WebAssembly.",
                    "Why it matters: a single machine has limits; horizontal scale needs cheap, secure node-to-node messaging.",
                    "Background: nodes connect like Node.connect/1, and a global registry resolves process names cluster-wide.",
                ],
                9,
            ),
        };
        ScenarioMeta {
            id: self.id().to_string(),
            label: label.to_string(),
            description: description.to_string(),
            details: details.into_iter().map(str::to_string).collect(),
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
            assert!(meta.details.len() >= 3, "{} needs real detail", meta.id);
            assert!(meta.details.iter().all(|d| !d.is_empty()));
            assert!((1..=10).contains(&meta.real_after_phase));
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
