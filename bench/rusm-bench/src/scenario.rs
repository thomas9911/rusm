use serde::{Deserialize, Serialize};

/// The roadmap phase RUSM has reached. A scenario runs on **real** runtime data
/// once its `real_after_phase` is at or below this — bump it as each phase lands.
pub const CURRENT_PHASE: u8 = 7;

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
    ComponentStorm,
    DistributedFanout,
    ModuleStorm,
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
    pub const ALL: [Scenario; 8] = [
        Scenario::SpawnStorm,        // phase 1
        Scenario::PingPong,          // phase 2
        Scenario::FaultRecovery,     // phase 3
        Scenario::ConnectionStorm,   // phase 5
        Scenario::Fairness,          // phase 6
        Scenario::ModuleStorm,       // phase 6 — wasip1 core modules (Lunatic head-to-head)
        Scenario::ComponentStorm,    // phase 7
        Scenario::DistributedFanout, // phase 9
    ];

    pub fn id(self) -> &'static str {
        match self {
            Scenario::SpawnStorm => "spawn-storm",
            Scenario::PingPong => "ping-pong",
            Scenario::Fairness => "fairness",
            Scenario::FaultRecovery => "fault-recovery",
            Scenario::ConnectionStorm => "connection-storm",
            Scenario::ComponentStorm => "component-storm",
            Scenario::DistributedFanout => "distributed-fanout",
            Scenario::ModuleStorm => "module-storm",
        }
    }

    pub fn from_id(id: &str) -> Option<Scenario> {
        Scenario::ALL.into_iter().find(|s| s.id() == id)
    }

    pub fn meta(self) -> ScenarioMeta {
        let (label, description, details, real_after_phase) = match self {
            Scenario::SpawnStorm => (
                "Spawn storm",
                "How fast can RUSM CREATE processes? Raw spawn throughput (create + reap).",
                vec![
                    "What's unique here: this stresses *creating* processes — nothing else. (Ping-pong is about messaging, fairness about CPU sharing, connection-storm about how many you can hold open.)",
                    "Headline: spawns/sec — measured ~2.4M/sec. A RUSM process is one Tokio task (plus, for Wasm, one isolated instance), so a spawn is just instantiate + schedule.",
                    "These are NOT OS processes/threads: hundreds of thousands run cooperatively over a few OS threads (the schedulers, ~one per CPU core). The BEAM runs millions; so can we.",
                    "Why it matters: if spawning is this cheap, you can model every request / connection / job as its own crash-isolated process.",
                    "Phase 1: REAL native rusm-otp processes — spawns/sec and spawn latency measured live.",
                ],
                1,
            ),
            Scenario::PingPong => (
                "Message ping-pong",
                "How fast can two processes TALK? Message throughput + round-trip latency.",
                vec![
                    "What's unique here: this stresses *messaging* between existing processes — the mailbox + scheduler hot path — not creating them (spawn storm).",
                    "Headline: messages/sec — measured ~21M/sec, round-trip p50 well under 1µs.",
                    "Each ping carries the sender's pid so the ponger knows whom to reply to; messages move by value, nothing is shared, exactly like Erlang.",
                    "Low, stable round-trip latency under load means wakeups are cheap and scheduling is fair.",
                    "Phase 2: REAL rusm-otp processes exchanging real messages, measured live.",
                ],
                2,
            ),
            Scenario::Fairness => (
                "Fairness under tight loop",
                "Can one CPU-HOGGING process starve the rest? It must not — preemption keeps others running.",
                vec![
                    "\"Fairness\" = fair CPU sharing: no single process may hog a core and starve the others. What's unique here: it's about *CPU sharing*, not throughput. Tight-loop Wasm guests (spinners) try to hog every core while bystander guests must keep working.",
                    "Headline: bystander progress (work/sec) — they keep running at tens of millions of ops/sec (~50M+ under everyday load, past 400M when cores are free) *despite* the spinners. A nonzero rate IS the proof; the absolute number scales with available CPU.",
                    "Phase 6: REAL Wasm. Tokio scheduling is cooperative, so RUSM arms Wasmtime epoch interruption — even an infinite-loop guest yields (the BEAM's reduction-counting idea; lighter than Lunatic's per-instruction fuel — a periodic atomic, ~zero steady-state cost).",
                    "Without preemption, spinners filling every scheduler thread would pin bystanders to zero. They don't.",
                ],
                6,
            ),
            Scenario::FaultRecovery => (
                "Fault recovery",
                "When a process CRASHES, how fast does its supervisor RESTART it?",
                vec![
                    "What's unique here: it's about *crash recovery* — deliberately crashing supervised children and timing how fast they come back. \"Let it crash\", in action.",
                    "Headline: restarts/sec — measured ~285k/sec.",
                    "Each supervisor traps exits and links its children; a crash arrives as an exit signal and the supervisor starts a clean replacement while the rest of the system never notices.",
                    "Why it matters: per-process isolation is what makes crashes survivable rather than fatal.",
                    "Phase 3: REAL rusm-otp supervisors restarting real crashing children, measured live.",
                ],
                3,
            ),
            Scenario::ConnectionStorm => (
                "Connection storm",
                "How MANY live network connections can RUSM hold at once — one process per connection?",
                vec![
                    "What's unique here: it's about *concurrency* — how many simultaneous TCP connections you can keep open, each served by its own isolated process — not raw create speed.",
                    "Headline: peak concurrent connections (the live process count) — thousands at once — plus connections/sec and connect latency.",
                    "Phase 5: REAL loopback TCP, each connection a cheap isolated process, measured live.",
                    "The ceiling is the OS (file descriptors, ephemeral ports, the kernel connect/accept path), NOT RUSM: minting a process per connection is near-free (spawn storm does ~2.4M/sec, hundreds of times this). The 300k/s target wants an external load generator on a tuned OS — RUSM scales with the kernel, not the runtime.",
                ],
                5,
            ),
            Scenario::ComponentStorm => (
                "Component storm",
                "How fast can RUSM LOAD + RUN real WASM components (vs bare processes)?",
                vec![
                    "What's unique here: like the spawn storm, but each process is a real WASI **component** (a sandboxed Wasm instance), so it measures the true cost of *hosting components* — the thing wasmCloud does heavily.",
                    "Headline: component spawns/sec — measured ~440k/sec (instantiate from the pooling allocator + copy-on-write image, then schedule + reap).",
                    "Phase 7: REAL components. The optimized lever set runs live — pooling allocator, CoW, per-module InstancePre, precomputed export index, single runtime-handle clone, zero-overhead default mailbox, park-based backpressure.",
                    "Lunatic hosts only core modules with its own ABI — it has no component-model host at all; matching bare-process spawn economics while hosting real components is the bar we clear here.",
                ],
                7,
            ),
            Scenario::ModuleStorm => (
                "Module storm",
                "How fast can RUSM spawn raw wasip1 core modules — Lunatic's exact domain?",
                vec![
                    "What's unique here: like the spawn storm and component storm, but each process is a raw **wasip1 core module** — the exact artifact Lunatic hosts. It's the isolation tier *between* a bare task and a full component.",
                    "Headline: core-module spawns/sec — measured ~475k/sec (instantiate from the pooling allocator + copy-on-write image + precomputed export index, then schedule + reap). Faster than a component (~440k, no component-model wiring), slower than a bare task (~2.4M, real Wasm isolation).",
                    "Phase 6: REAL wasip1. The same lever set as the component path runs live — pooling allocator, CoW, per-module InstancePre, precomputed ModuleExport index, single runtime-handle clone, park-based backpressure.",
                    "The direct Lunatic head-to-head: Lunatic spawns wasip1 core-module processes via on-demand allocation + per-instruction fuel; RUSM recycles pooled instances and preempts with epochs — and hosts components too.",
                ],
                6,
            ),
            Scenario::DistributedFanout => (
                "Distributed fan-out",
                "How fast do processes on DIFFERENT machines talk? Cross-node messaging.",
                vec![
                    "What's unique here: all the others run on one node; this one crosses the network between separate RUSM nodes (different machines/processes).",
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
        // Exactly the scenarios with a real engine (real_after_phase <= 7); only
        // distributed-fanout (phase 9) is still synthetic.
        assert_eq!(
            real,
            vec![
                "spawn-storm",
                "ping-pong",
                "fault-recovery",
                "connection-storm",
                "fairness",
                "module-storm",
                "component-storm"
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
