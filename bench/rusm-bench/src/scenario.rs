use serde::{Deserialize, Serialize};

/// The roadmap phase RUSM has reached. A scenario runs on **real** runtime data
/// once its `real_after_phase` is at or below this — bump it as each phase lands.
pub const CURRENT_PHASE: u8 = 11;

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
    StreamPipe,
    HttpThroughput,
}

/// What a scenario's headline throughput number *counts*, so the dashboard can
/// format it correctly (a plain count uses k/M/B; a byte rate uses KB/MB/GB/s).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricUnit {
    /// A per-second count — spawns, messages, restarts, …
    Count,
    /// A per-second byte rate — streaming throughput.
    Bytes,
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
    /// How to read/format the throughput headline (count vs byte rate).
    pub unit: MetricUnit,
    /// The engine's implementation source (Rust), so the dashboard can show how the
    /// scenario is built. `None` for synthetic scenarios. Single source of truth:
    /// this is the actual compiled file via `include_str!`.
    pub source: Option<String>,
    /// The engine source's filename (e.g. `streampipe.rs`), shown as the code
    /// panel's header. `None` for synthetic scenarios.
    pub source_file: Option<String>,
}

impl Scenario {
    // Ordered by the phase each scenario goes live (the dashboard menu shows them
    // in this order). The enum discriminants are unchanged, so the synthetic
    // source stays deterministic.
    pub const ALL: [Scenario; 10] = [
        Scenario::SpawnStorm,        // phase 1
        Scenario::PingPong,          // phase 2
        Scenario::FaultRecovery,     // phase 3
        Scenario::ConnectionStorm,   // phase 5
        Scenario::Fairness,          // phase 6
        Scenario::ModuleStorm,       // phase 6 — wasip1 core modules (Lunatic head-to-head)
        Scenario::ComponentStorm,    // phase 7
        Scenario::StreamPipe,        // phase 7 — cross-process byte-stream throughput
        Scenario::DistributedFanout, // phase 9
        Scenario::HttpThroughput,    // phase 11 — serve a WASM component over HTTP
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
            Scenario::HttpThroughput => "http-throughput",
            Scenario::ModuleStorm => "module-storm",
            Scenario::StreamPipe => "stream-pipe",
        }
    }

    pub fn from_id(id: &str) -> Option<Scenario> {
        Scenario::ALL.into_iter().find(|s| s.id() == id)
    }

    /// What this scenario's throughput number measures — bytes/sec for the byte
    /// pipe, a plain per-second count for everything else.
    pub fn metric_unit(self) -> MetricUnit {
        match self {
            Scenario::StreamPipe => MetricUnit::Bytes,
            _ => MetricUnit::Count,
        }
    }

    /// The **engine source file** backing this scenario, embedded at compile time
    /// (`include_str!`) so the dashboard can show the *real* code that produced the
    /// numbers — single source of truth, never a hand-copied snippet. `None` for
    /// scenarios still on synthetic data (no engine yet).
    fn engine_source(self) -> Option<(&'static str, &'static str)> {
        Some(match self {
            Scenario::SpawnStorm => ("spawnstorm.rs", include_str!("spawnstorm.rs")),
            Scenario::PingPong => ("pingpong.rs", include_str!("pingpong.rs")),
            Scenario::FaultRecovery => ("faultrecovery.rs", include_str!("faultrecovery.rs")),
            Scenario::ConnectionStorm => ("connectionstorm.rs", include_str!("connectionstorm.rs")),
            Scenario::Fairness => ("fairness.rs", include_str!("fairness.rs")),
            Scenario::ModuleStorm => ("modulestorm.rs", include_str!("modulestorm.rs")),
            Scenario::ComponentStorm => ("componentstorm.rs", include_str!("componentstorm.rs")),
            Scenario::StreamPipe => ("streampipe.rs", include_str!("streampipe.rs")),
            Scenario::DistributedFanout => {
                ("distributedfanout.rs", include_str!("distributedfanout.rs"))
            }
            Scenario::HttpThroughput => ("httpthroughput.rs", include_str!("httpthroughput.rs")),
        })
    }

    /// The engine's implementation source (the file above its `#[cfg(test)]`
    /// module), trimmed — the "essential benchmark code" for the dashboard panel.
    fn engine_impl_source(self) -> Option<String> {
        self.engine_source().map(|(_, src)| {
            src.split("\n#[cfg(test)]")
                .next()
                .unwrap_or(src)
                .trim_end()
                .to_string()
        })
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
                    "What's unique here: like the spawn storm and component storm, but each process is a raw **wasip1 core module** — the *exact artifact Lunatic hosts*, so it's the apples-to-apples number for a head-to-head.",
                    "Headline: core-module spawns/sec — measured ~475k/sec (instantiate from the pooling allocator + copy-on-write image + precomputed export index, then schedule + reap). Notably this is ~the same as a wasip2 component (~440k): the component model costs almost nothing over a raw core module on RUSM's pooled path. The real gap is to a bare task (~2.4M) — that ~5x is the price of real Wasm memory isolation.",
                    "Phase 6: REAL wasip1. The same lever set as the component path runs live — pooling allocator, CoW, per-module InstancePre, precomputed ModuleExport index, single runtime-handle clone, park-based backpressure.",
                    "The direct Lunatic head-to-head: Lunatic spawns wasip1 core-module processes via on-demand allocation + per-instruction fuel; RUSM recycles pooled instances and preempts with epochs — and hosts components too.",
                ],
                6,
            ),
            Scenario::StreamPipe => (
                "Stream pipe",
                "How fast can one process PIPE a byte stream to another (with back-pressure)?",
                vec![
                    "What's unique here: every other storm measures *creating* processes or *sending one message*; this measures *moving a continuous byte stream* between two processes — one opens a stream to another and floods it with data as fast as the reader will take it.",
                    "Headline: streaming throughput in bytes/sec (so 800,000,000 means ~800 MB/s; this scenario reaches multiple GB/s aggregate across pairs). A producer process streams 4 KiB chunks to a consumer through the runtime; the rate is how many bytes per second make it end to end.",
                    "Back-pressure (the important bit): the stream is a *bounded* channel, so a slow reader automatically slows the writer — the writer's fiber simply parks until there's room, with no unbounded memory growth and no busy-polling. That safety is exactly what lets a component serve HTTP/SSE/WS bodies without falling over.",
                    "Phase 7: REAL streams. Built on the Wasm-free StreamHandle (a Tokio bounded channel) bridged to WASM guests via the actor ABI (stream_open / stream_write / stream_read) — the same byte-stream primitive a native process gets, handed to a sandboxed Wasm process.",
                ],
                7,
            ),
            Scenario::DistributedFanout => (
                "Distributed fan-out",
                "How fast do processes on DIFFERENT machines talk? Cross-node messaging.",
                vec![
                    "What's unique here: all the others run on one node; this one crosses the QUIC + TLS link between separate RUSM nodes (here, several real nodes in-process, each on its own loopback endpoint — a faithful stand-in for separate machines).",
                    "Real (Phase 9): a hub node and a pool of senders each keep one round-trip in flight to worker nodes' echo processes, so the headline is genuine cross-node throughput, not a backlog.",
                    "Headline: cross-node round-trips/sec and round-trip latency over the rusm-cluster transport (~550k cross-node msgs/sec, ~39µs p50 unloaded on loopback).",
                    "Processes message across nodes transparently — distributed Erlang, for WebAssembly — with remote spawn and live attach on the same transport.",
                    "Why it matters: a single machine has limits; horizontal scale needs cheap, secure node-to-node messaging.",
                    "Background: nodes connect like Node.connect/1, and a gossiped global registry resolves process names cluster-wide.",
                ],
                9,
            ),
            Scenario::HttpThroughput => (
                "HTTP throughput",
                "How fast can a sandboxed WASM component serve HTTP? Requests/sec.",
                vec![
                    "What's unique here: the others measure the actor model; this serves real HTTP — a WASM component (wstd `wasi:http`) hosted by hyper + wasmtime-wasi-http, one fresh sandboxed instance per request.",
                    "Real (Phase 11): keep-alive clients hammer the server; the response is produced BY THE GUEST, the host only moves bytes. Total isolation between requests — a trap fails just that request.",
                    "Headline: requests/sec and per-request p50/p99 latency. The cost over a bare-hyper baseline is per-request instantiation — the lever a warm-instance pool would amortize.",
                    "Standards-first: the guest exports the standard `wasi:http` handler (RS via wstd, TS via the js-runner's fetch shape), so stock components run unchanged.",
                    "Why it matters: this is the headline goal — run your low-code/LLM/web component as a high-throughput, sandboxed, supervised HTTP server.",
                ],
                11,
            ),
        };
        ScenarioMeta {
            id: self.id().to_string(),
            label: label.to_string(),
            description: description.to_string(),
            details: details.into_iter().map(str::to_string).collect(),
            real_after_phase,
            real: real_after_phase <= CURRENT_PHASE,
            unit: self.metric_unit(),
            source: self.engine_impl_source(),
            source_file: self.engine_source().map(|(name, _)| name.to_string()),
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
    fn only_the_byte_pipe_reports_a_byte_rate() {
        assert_eq!(Scenario::StreamPipe.metric_unit(), MetricUnit::Bytes);
        for s in Scenario::ALL {
            if s != Scenario::StreamPipe {
                assert_eq!(s.metric_unit(), MetricUnit::Count, "{} is a count", s.id());
            }
        }
        // The unit travels in the meta the dashboard receives.
        assert_eq!(Scenario::StreamPipe.meta().unit, MetricUnit::Bytes);
    }

    #[test]
    fn live_scenarios_carry_their_engine_source() {
        // Single source of truth: the meta ships the real engine file (impl only,
        // no test module); synthetic scenarios ship none.
        let spawn = Scenario::SpawnStorm.meta().source.unwrap();
        assert!(spawn.contains("struct SpawnStormEngine"));
        assert!(
            !spawn.contains("#[cfg(test)]"),
            "tests are stripped from the panel"
        );
        assert!(Scenario::StreamPipe
            .meta()
            .source
            .unwrap()
            .contains("StreamPipeEngine"));
        assert!(Scenario::DistributedFanout
            .meta()
            .source
            .unwrap()
            .contains("DistributedFanoutEngine"));
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
            assert!((1..=11).contains(&meta.real_after_phase));
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
        // Every scenario has a real engine (CURRENT_PHASE = 11) — nothing synthetic.
        assert_eq!(
            real,
            vec![
                "spawn-storm",
                "ping-pong",
                "fault-recovery",
                "connection-storm",
                "fairness",
                "module-storm",
                "component-storm",
                "stream-pipe",
                "distributed-fanout",
                "http-throughput"
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
