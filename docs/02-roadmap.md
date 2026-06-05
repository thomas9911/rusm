# Roadmap — TDD baby steps

Each phase writes the failing test first, then implements until green, and leaves
`cargo test` passing. Every phase "graduates" a dashboard scenario from synthetic
data to real measurements.

| Phase | Theme | Concept it teaches |
| --- | --- | --- |
| **0 ✅** | Docs + benchmark dashboard + harness (synthetic) | the observability + load harness we measure everything with |
| 1 | Embed Wasmtime | Engine/Module/Store/Linker/Instance, host imports |
| 2 | Process = Wasm instance + Tokio task | instance↔task mapping, spawn throughput |
| 3 | Host ABI + per-process state | reading/writing guest memory from the host |
| 4 | Message passing & mailboxes | copying across isolated memories; receive suspends |
| 5 | Preemption & blocking→async | fibers = stack switching; epoch fairness |
| 6 | Links, traps & supervision | "let it crash"; restart strategies |
| 7 | WASI + per-process permissions | fine-grained per-actor sandboxing |
| 8 | Timers, registry, TCP networking | real servers; **300k/s connection proof** |
| 9 | `rusm-rs` guest crate | ergonomic `spawn`/`Mailbox`/`AbstractProcess` |
| 10 | Distributed clusters + live attach | secure nodes; hook into a running node |
| 11 | Stretch | 300k/s hardening, hot reload, hand-rolled stack switching |

## Phase 0 deliverables (done)

- `rusm-metrics`, `rusm-observer`, `rusm-bench` (+ WebSocket server), `rusm-cli`.
- React dashboard (benchmark + live observer) on synthetic data.
- Runnable examples; this docs set + the VitePress site.
- TDD throughout; coverage ≥98% (mostly 100%); `cargo fmt` + Prettier clean.

See the per-phase deep dives under [`phases/`](./phases/phase-00-foundation.md), and
the [RUSM vs Lunatic comparison](./lunatic-comparison.md) for the per-phase
"borrow vs beat" efficiency playbook we update as each gap closes.
