# Features

**RUSM is Erlang/OTP's concurrency and fault-tolerance model, rebuilt in Rust, where
every process can be a sandboxed WebAssembly component** — "let it crash" meets "run
untrusted code safely," at millions of lightweight processes, with none of the
wasmCloud ceremony. The whole design rests on one principle: the actor core
(`rusm-otp`) is **Wasm-free**; WebAssembly is a pluggable process backend. That's why
it's both a great native actor runtime *and* a great Wasm host.

This page is the map; each item links to the [concept](./concepts/wasm-instance-as-process)
that teaches it.

## Concurrency & the actor core

- **Massive lightweight concurrency (Tokio-driven)** — hundreds of thousands of
  processes over a few OS threads; spawning is near-free (~2.4M/sec measured). →
  [the process model](./concepts/wasm-instance-as-process)
- **Isolated, lightweight processes** — one process = one task (and, for Wasm, one
  isolated instance); no shared mutable state.
- **Superior messaging** — per-process mailboxes, by-value messages, selective
  receive, ~21M msgs/sec. → [message passing](./concepts/message-passing)
- **Fault tolerance & supervision** — links, monitors, `trap_exit`, `spawn_link`,
  `exit`, one-for-one / one-for-all / rest-for-one, windowed restart-intensity. →
  [links & supervision](./concepts/links-and-supervision)
- **Preemptive fairness** — a runaway guest can't starve others; Wasmtime epoch
  interruption forces it to yield. → [epoch preemption](./concepts/epoch-preemption)
- **"Write blocking code, get async"** — `receive` parks the fiber and frees the
  worker. → [fibers & blocking→async](./concepts/fibers-and-blocking-to-async)
- **Process management** — named registry, timers, graceful shutdown, full
  introspection. → [process management](./concepts/process-management)
- **Backpressure & overload** — bounded byte-stream channels; opt-in bounded
  mailboxes that shed *user* messages but never system/exit signals.

## WebAssembly & safety

- **WASM components (WASI p2 + p3)** — the modern component model, not just core
  modules (the headline difference from Lunatic). → [components & the actor
  world](./concepts/components-and-the-actor-world)
- **WASM core modules (wasip1)** — also runs raw preview-1 modules via a bridge.
- **Default-deny capability sandboxing** — `sandboxed` / `network-client` / `trusted`
  profiles + per-spawn overrides; per-instance memory caps; spawn-from-guest never
  escalates. → [permissions & sandboxing](./concepts/permissions-and-sandboxing)
- **Trap isolation** — a guest trap becomes that one process `Crashed`; the runtime
  and its neighbours are untouched.
- **Guests in Rust *or* TypeScript** — the `#[service]` macro, the concealed typed
  client, and the **shared rquickjs runner** (tiny TS components vs jco baking an
  engine into every one) + bytecode precompile. → [guests: Rust &
  TypeScript](./concepts/guests-rust-and-typescript)

## Serving & streaming

- **HTTP / WebSocket / SSE serving** — from a Rust *or* TypeScript component;
  per-request (max isolation) or resident (warm, stateful pool). →
  [the serving model](./concepts/serving-model)
- **Streaming & async** — incremental SSE, per-connection WS processes, Tokio
  back-pressure throughout.
- **Cross-process byte streams** — a bounded, back-pressured byte channel between
  processes. → [byte streams](./concepts/byte-streams)

## Apps, clusters & DX

- **App model** — `rusm.toml` describes components and servers; `rusm build` (cargo
  wasm32-wasip2 / Bun, no jco) → `./wasm/`. → [the app model](./concepts/app-model)
- **CLI** — `rusm new` (scaffold), `rusm run`, `rusm serve`, `rusm dev` (watch +
  reload), `rusm attach` (a live REPL into a local or remote node).
- **Distributed clustering** — `ClusterNode::connect` (the `Node.connect` equivalent),
  cross-node send, a gossiped global registry, remote spawn, all over QUIC + **mutual
  TLS**. → [distributed nodes](./concepts/distributed-nodes)
- **Live attach** — inspect/control a running node's processes live. →
  [live attach](./concepts/live-attach)
- **DX: infra never bothers you** — you write *application functions*; RUSM owns
  *all* the infrastructure (spawn/receive/reply/supervise/sockets).
- **No funky rules** — no execution-time cap, no "this must be a service," no
  lattice/provider ceremony.

## Observability & quality

- **Live process statistics** — an Erlang-`observer`-style view (process count,
  scheduler load, memory, per-instance table), nearly free.
- **Live benchmark dashboard** — real scenarios streamed to a React/uPlot UI. →
  [benchmark & dashboard](./03-benchmark-dashboard)
- **Fair, out-of-process benchmarking** — `rusm-loadtest` drives a real `rusm serve`
  port from a separate process, so the numbers are the server's.
- **TDD, ~100% coverage; OTP first, WASM second** — the dependency graph enforces the
  Wasm-free core. → [architecture](./01-architecture)

## Where it's going

The core is audited-solid today; **Phase 12** (serving TLS, serve-path admission
control, cluster gossip authentication) is explicitly planned. See the
[roadmap](./02-roadmap).
