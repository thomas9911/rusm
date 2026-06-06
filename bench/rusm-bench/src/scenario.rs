use serde::{Deserialize, Serialize};

/// The roadmap phase RUSM has reached. A scenario runs on **real** runtime data
/// once its `real_after_phase` is at or below this — bump it as each phase lands.
pub const CURRENT_PHASE: u8 = 6;

/// A benchmark scenario the dashboard can run.
///
/// `real_after_phase` records the roadmap phase at which a scenario switches from
/// synthetic data to measuring the real runtime; see [`CURRENT_PHASE`].
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
    /// Whether this scenario is driven by the **real** runtime now (vs synthetic),
    /// i.e. `real_after_phase <= CURRENT_PHASE`.
    pub real: bool,
}

impl Scenario {
    // Ordered by the phase each scenario goes live (the dashboard menu shows them
    // in this order). The enum discriminants are unchanged, so the synthetic
    // source stays deterministic.
    pub const ALL: [Scenario; 6] = [
        Scenario::SpawnStorm,        // phase 1
        Scenario::PingPong,          // phase 2
        Scenario::FaultRecovery,     // phase 3
        Scenario::ConnectionStorm,   // phase 5
        Scenario::Fairness,          // phase 6
        Scenario::DistributedFanout, // phase 9
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
                    "Phase 1: these are REAL native rusm-otp processes — spawns/sec and spawn latency are measured live. Per-process memory shows 0 until processes become Wasm instances (Phase 6).",
                ],
                1,
            ),
            Scenario::PingPong => (
                "Message ping-pong",
                "Two processes bounce messages; measures messages/sec and round-trip latency.",
                vec![
                    "Pairs of processes bounce messages back and forth — the mailbox and scheduler hot path.",
                    "Headline: messages/sec and round-trip latency.",
                    "Each ping carries the sender's pid so the ponger knows whom to reply to; messages move by value, nothing is shared, exactly like Erlang.",
                    "Low, stable round-trip latency under load means wakeups are cheap and scheduling is fair.",
                    "Phase 2: these are REAL rusm-otp processes exchanging real messages — messages/sec and round-trip latency are measured live.",
                ],
                2,
            ),
            Scenario::Fairness => (
                "Fairness under tight loop",
                "A CPU-bound process must not starve others; measures progress of bystanders.",
                vec![
                    "Runs CPU-bound, tight-loop Wasm guests (spinners) alongside bystander guests that must keep making progress.",
                    "Headline: bystander progress (work/sec). A nonzero rate proves preemption works — the bars don't flatline under the spinners.",
                    "Phase 6: REAL Wasm. Tokio scheduling is cooperative, so RUSM arms Wasmtime epoch interruption — even an infinite-loop guest yields, the analogue of the BEAM's reduction counting (and we beat Lunatic's per-instruction fuel: a periodic atomic, ~zero steady-state cost).",
                    "Without preemption, spinners filling every scheduler thread would pin bystanders to zero. They don't.",
                ],
                6,
            ),
            Scenario::FaultRecovery => (
                "Fault recovery",
                "Crash supervised children; measures restarts/sec and recovery latency.",
                vec![
                    "Deliberately crashes supervised child processes and watches them come back.",
                    "Headline: restarts/sec and recovery latency — \"let it crash\" in action.",
                    "Each supervisor traps exits and links its children; a child's crash arrives as an exit signal, and the supervisor starts a clean replacement while the rest of the system never notices.",
                    "Background: per-process isolation is what makes crashes survivable rather than fatal.",
                    "Phase 3: these are REAL rusm-otp supervisors restarting real crashing children — restarts/sec and recovery latency are measured live.",
                ],
                3,
            ),
            Scenario::ConnectionStorm => (
                "Connection storm",
                "Open real TCP connections, one process each; measures peak concurrent, conns/sec, latency.",
                vec![
                    "Opens real loopback TCP connections, each served by its own rusm-otp process — process-per-connection, the headline scenario.",
                    "Headline: peak concurrent connections (the live process count), plus connections/sec and connect latency.",
                    "Phase 5: REAL TCP — thousands of simultaneous connections, each a cheap isolated process, measured live.",
    "The ceiling is the OS — file descriptors, ephemeral ports, the kernel connect/accept path — not RUSM: minting a process per connection is near-free (the spawn storm does ~1.4M/s, ~100x this). The loopback rate is OS-bound (pushing client concurrency just raises latency); the project's 300k/s target wants an external load generator on a tuned OS, where RUSM scales with the kernel, not the runtime.",
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
            real: real_after_phase <= CURRENT_PHASE,
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
    fn graduated_scenarios_are_marked_real() {
        let real: Vec<String> = Scenario::all_meta()
            .into_iter()
            .filter(|m| m.real)
            .map(|m| m.id)
            .collect();
        // Exactly the scenarios with a real engine (real_after_phase <= 6); only
        // distributed-fanout (phase 9) is still synthetic.
        assert_eq!(
            real,
            vec![
                "spawn-storm",
                "ping-pong",
                "fault-recovery",
                "connection-storm",
                "fairness"
            ]
        );
    }

    #[test]
    fn scenarios_are_listed_in_phase_order() {
        let phases: Vec<u8> = Scenario::all_meta()
            .iter()
            .map(|m| m.real_after_phase)
            .collect();
        let mut sorted = phases.clone();
        sorted.sort_unstable();
        assert_eq!(phases, sorted, "the scenario menu must be phase-ordered");
    }

    #[test]
    fn meta_round_trips_through_json() {
        let metas = Scenario::all_meta();
        let json = serde_json::to_string(&metas).unwrap();
        let back: Vec<ScenarioMeta> = serde_json::from_str(&json).unwrap();
        assert_eq!(metas, back);
    }
}
