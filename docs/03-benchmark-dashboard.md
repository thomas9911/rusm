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

## Protocol

The node and clients speak a small JSON protocol (`rusm-bench` `protocol.rs`,
mirrored in `bench/dashboard/src/types.ts`):

- Server → client: `hello { scenarios }`, `tick { frame }`, `error { message }`.
- Client → server: `run { scenario }`, `stop`, `set_observer_detail { enabled }`.

A `Frame` carries the scenario, running flag, throughput, latency snapshot, and
observer snapshot. The dashboard folds messages into state with a pure reducer
(`state.ts`) — fully unit-tested.
