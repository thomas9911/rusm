# Benchmark dashboard & observer

The dashboard (`bench/dashboard`, React on Bun, uPlot charts) is the north-star
artifact — how we *see* every phase's progress. It has two views fed by one
WebSocket stream from a node.

## Benchmark view

A menu of scenarios and a **Run** button. Each tick streams: throughput
(ops/sec), peak concurrent, latency p50/p95/p99, and a live throughput chart.

| Scenario | Headline | Real after |
| --- | --- | --- |
| Spawn storm | spawns/sec, memory/process | phase 1 |
| Message ping-pong | msgs/sec, round-trip latency | phase 2 |
| Fault recovery | restarts/sec, recovery latency | phase 3 |
| **Connection storm (300k/s proof)** | conns/sec, peak, latency | phase 5 (native), re-measured phase 6 |
| Fairness under tight loop | bystanders keep progressing | phase 6 |
| Distributed fan-out | cross-node latency | phase 9 |

## Live observer view

A real-time view of the node (à la Erlang `observer`): process count,
running/waiting, scheduler load bars, total memory, and a per-instance table.

## Observability must stay cheap

Counters are relaxed atomics; the node pushes a **periodic aggregated snapshot**
(10–60 Hz), never an event per operation. The per-instance detail table is the
only costly part of a snapshot, so it is **toggleable** — off for clean
benchmark runs. We prove the overhead is negligible by running the 300k/s
benchmark **observer-on vs observer-off** (see the `observer_overhead` example).

## Spawn-storm: how the number is produced (read this)

The spawn-storm is the first scenario on **real** data, so it's worth being
precise about what its `ops/sec` means and why it's safe.

- **It's a continuous, multi-core storm.** One background spawner task per
  (allowed) core hammers `rusm-otp` — `runtime.spawn(...)` — as fast as it can.
  A single sequential loop would be capped by one core; a storm uses many. The
  tick just **samples** the achieved rate (`Δspawned / Δt`).
- **It measures create *and reap*.** The spawned processes are trivial and finish
  immediately, so the rate reflects full lifecycle throughput, not just creation.
- **Backpressure keeps it honest and safe.** Spawners pause once the *live*
  population reaches the in-flight cap, then resume as processes drain. Without
  this the population would run to millions — a memory leak, not throughput. This
  is why **"peak concurrent" sits at the cap** (e.g. ~50k on Balanced): it's the
  configured ceiling, not a capability limit.
- **`memory` shows 0.** Native processes have no per-instance linear memory; that
  figure becomes real once processes are Wasm instances (Phase 6).

## Resource profiles (how hard it drives the machine)

A segmented control picks how much of the machine a storm may use. Everything is
**relative to your CPU count**, and there are two hard safety guarantees: the
in-flight cap is never unbounded, and even **Max never exceeds 90% of cores** (it
always leaves headroom so the system stays responsive).

| Profile | Spawn workers | In-flight cap | Use it when |
| --- | --- | --- | --- |
| **Light** | ~¼ of cores | ~1k × cores | you want a gentle background demo |
| **Balanced** (default) | ~½ of cores | ~5k × cores | normal use — already well past 300k/s on multi-core |
| **Max** | up to 90% of cores | ~50k × cores | you want to see the absolute ceiling |

The default is **Balanced** on purpose: it's impressive *and* keeps your laptop
usable. Switch to **Max** in the selector when you want the ceiling. Defined in
`rusm-bench` `profile.rs` (`ResourceProfile`).

## Protocol

The node and clients speak a small JSON protocol (`rusm-bench` `protocol.rs`,
mirrored in `bench/dashboard/src/types.ts`):

- Server → client: `hello { scenarios, profiles }`, `tick { frame }`, `error { message }`.
- Client → server: `run { scenario }`, `stop`, `set_observer_detail { enabled }`,
  `set_resource_profile { profile }`.

A `Frame` carries the scenario, running flag, throughput, latency snapshot,
observer snapshot, and the **active resource profile**. The dashboard folds
messages into state with a pure reducer (`state.ts`) — fully unit-tested.
