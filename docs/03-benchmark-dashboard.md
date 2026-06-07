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
- **Backpressure is a safety net, not the operating point.** Spawners pause if
  the *live* population ever reaches the in-flight cap, so the table can't grow
  without bound. But at every profile the population **self-limits far below the
  cap** (a few hundred live), because spawn rate and reap rate balance out — so
  "peak concurrent" reflects the real steady-state population, not a configured
  ceiling.
- **Throughput is reap-bound, so the lever is the spawner-to-reaper balance.**
  The limit is how fast finished processes drain (~one reaper core's worth each).
  Too few spawners under-drives the machine; too many starve the reapers and pile
  processes up *without* going faster. The sweet spot is spawners ≈ reapers
  (~half the cores each) — that's what **Max** uses for peak *smooth* throughput.
- **`memory` shows 0.** Native processes have no per-instance linear memory; that
  figure becomes real once processes are Wasm instances (Phase 6).

## Resource profiles (the throughput dial)

A segmented control picks how hard the storm drives the machine. The **spawn
worker count is the dial** and is relative to your CPU count; throughput rises
with each tier. The in-flight cap is a uniform per-core safety net (memory can't
run away), **not** a per-tier knob — the population self-limits well below it.

| Profile | Spawn workers | Throughput (busy 10-core box, release) | Use it when |
| --- | --- | --- | --- |
| **Light** | ~¼ of cores | ~1.9M/s | speed isn't the point — leave the machine alone |
| **Balanced** (default) | ~⅖ of cores | ~2.5M/s | good throughput with visible room above |
| **Max** | ~½ of cores | ~2.6M/s | most performant — peak sustained rate, still smooth |

`Max` deliberately stops at ~half the cores: the other half reap, which is the
sustained-throughput peak. Pushing spawners higher does **not** go faster — it
just starves the reapers and piles processes up. So `Max` is the fastest profile
*and* keeps the live population to a few hundred (no pile-up). The default is
**Balanced** — fast, with headroom, and easy on the laptop. Defined in
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
