# Roadmap — TDD baby steps

Each phase writes the failing test first, then implements until green, and leaves
`cargo test` passing. Every phase "graduates" a dashboard scenario from synthetic
data to real measurements.

> **Foundation-first ordering.** The Erlang model — processes, messaging,
> supervision/fault-tolerance, management, connectivity — is the foundation and
> comes first, built on **native Rust process bodies** so it's real and measurable
> early. **Wasmtime is the execution *backend*, slotted in at Phase 6**: the actor
> layer is designed wasm-ready, so swapping a process body from a native closure to
> a sandboxed Wasm instance is additive, not a rewrite. That's also when
> "task-level" fault isolation becomes "true memory isolation".
>
> **Crate mapping:** Phases 1–5 build the Wasm-free OTP core (`rusm-otp` — usable
> standalone); Phase 6 adds the `rusm-wasm` backend; the `rusm` runtime composes
> them. The OTP layer is *all* of Phases 1–5, not just Phase 1.

| Phase | Theme | Graduates to real data |
| --- | --- | --- |
| **0 ✅** | Observability + benchmark dashboard (synthetic) | — |
| **1 ✅** | **Process & scheduler core** — task + process table + abort-based lifecycle, pluggable body | **spawn-storm** (live) |
| **2 ✅** | **Mailboxes & message passing** — per-process mailbox, `send`/`recv`, selective `recv_match` | **ping-pong** (live) |
| **3 ✅** | **Links, monitors, supervision, fault tolerance** — exit reasons, `link`/`monitor`/`trap_exit`/`spawn_link`/`exit`, cascades | **fault-recovery** (live) |
| **4 ✅** | **Process management** — named registry, timers (`send_after`/`cancel`), graceful `shutdown` | — |
| **5 ✅** | **Connectivity: TCP** — `listen`/`connect`, process-per-connection (TLS folds into the Phase 9 secure cluster transport) | **connection-storm** (live) |
| **6 ✅** | **Embed Wasmtime as the process backend** — instance-per-process, host ABI, epoch preemption, pooling + CoW + `InstancePre`; fairness graduated to real Wasm | **fairness** (live) |
| **7 ✅** | **Component hosting** — the **component model** (WASI p2 + p3) via `bridges/`, a `rusm:runtime` WIT actor world (self/send/receive/list/info/kill/register), default-deny capability profiles + memory limits, process introspection, byte streams, and an app model (`rusm.toml [[components]]`, `rusm build`/`dev`). ~440k component spawns/s | **component-storm** (live) |
| **8 ✅** | **Guest ergonomics** — **`rusm-ts`** (TS/Bun → the rquickjs **js-runner**, no jco): `rusm build` bundles each TS component with Bun → `wasm/<name>.js`; **service components** export functions (RUSM runs the receive→dispatch→reply loop) and a **worker** exports `default`; the **concealed typed client** `spawn<typeof Svc>("svc")` makes a cross-process call read like `await svc.method(...)` — plus `for await` **streaming** of generator handlers and **callback** args (a function stays in the caller, its invocations routed back) — over capability-gated, non-escalating spawn-from-guest; async `Process` API, binary messages + byte streams, all typed by the importable **`rusm` npm package** (`import { Process, spawn } from "rusm"`). **`rusm-rs`** (the Rust twin): ergonomic `Pid`/`send`/`receive` (serde JSON, same wire as TS)/`spawn`/registry/`Stream` (wit-bindgen library/binary split) + a `#[rusm_rs::service]` macro → a dispatch loop + a typed `Client` with **call/cast/streaming/callbacks**. A Rust client and a TS service interoperate. Both get an in-guest **`Supervisor`** (one-for-one / one-for-all / rest-for-one over a `monitor` ABI), and **`rusm dev`** watches `./components` and rebuilds + reloads on edit. | — |
| **9 ✅** | **Distributed clusters + live attach** — the Wasm-free `rusm-cluster` crate over `rusm-otp`: QUIC+TLS nodes, cross-node `send`, a gossiped **global registry** (`register_global`/`send_global`), **remote spawn** (named factories) and **live attach** (`remote_pids`) over one control-plane RPC. ~550k cross-node msgs/s, ~39µs p50 round-trip (loopback). | **distributed-fanout** (live) |
| 10 | **Scale & hardening** — *not raw speed* (throughput/latency is already at the isolation-model ceiling: ~440k component spawns/s, ~21M msgs/s). An **on-demand instance tier** that lifts the *fixed* pooled-instance cap: when the pool is exhausted, spawn from the on-demand allocator so the live *Wasm*-process count is bounded by **available memory** (each instance carries its own linear memory) rather than a compile-time pool size. (The OTP core already runs millions of *native* processes; this just removes the artificial cap for *Wasm*-backed ones — RAM is the real wall.) Plus **opt-in bounded mailboxes** (overload back-pressure / load-shed), **supervisor restart-intensity**, and **cluster security hardening** — replace Phase 9's single pre-shared cluster cert with **per-node certificates under a cluster CA** (mutual TLS + node-identity authz), so a compromised node can be revoked without re-keying the cluster. See the [design analysis](./design-analysis.md). | — |
| 11 | **Standard-WASI surface & wstd compatibility** — invoke the standard `wasi:cli/run` entrypoint (so stock command components run unchanged), host `wasi:http`, support [`wstd`](https://github.com/bytecodealliance/wstd)-based guests, and a native p3-typed **`stream<u8>`** signature for the actor world (the byte streams already work over a handle ABI — this is the standards-first refinement). RUSM stays a standards-first host; the actor world stays opt-in. (Can be sequenced earlier — `wasi:http` pairs with the HTTP-serving goal.) | — |

## What's shipped so far (Phases 0–9)

- **Phase 0 — observability & harness:** `rusm-metrics`, `rusm-observer`,
  `rusm-bench` (+ WebSocket server), `rusm-cli`, the React dashboard, and this
  docs/VitePress site.
- **Phases 1–5 — the Wasm-free OTP core (`rusm-otp`):** process & scheduler core,
  mailboxes & message passing, links/monitors/supervision, the named registry +
  timers + graceful shutdown, and TCP (process-per-connection).
- **Phase 6 — Wasmtime backend (`rusm-wasm`):** instance-per-process, host ABI,
  epoch preemption, pooling + CoW + `InstancePre`.
- **Phase 7 — component hosting:** the component model (WASI **p2 + p3**) via
  `bridges/{wasip1,wasip2,wasip3}`, the `rusm:runtime` WIT actor world, default-deny
  capabilities, cross-process byte streaming, and the `rusm.toml` app model.
- **Phase 8 — guest ergonomics:** `rusm-ts` (TS/Bun → js-runner) + `rusm-rs` (the
  Rust twin), service components + a concealed typed client (call/cast/streaming/
  callbacks), spawn-from-guest, an in-guest `Supervisor`, and `rusm dev` reload.
- **Phase 9 — distributed clusters (`rusm-cluster`):** QUIC+TLS nodes, cross-node
  `send`, a gossiped global registry, remote spawn, and live attach — over
  `rusm-otp`, Wasm-free.
- **Nine live dashboard benchmarks** — *every* scenario now runs on real data
  (spawn-storm, ping-pong, fault-recovery, connection-storm, fairness, module-storm,
  component-storm, stream-pipe, distributed-fanout) + the standalone `cluster_fanout`
  cross-node benchmark.
- TDD throughout; coverage ≥98% (mostly 100%); `cargo fmt` + Prettier clean.

See the per-phase deep dives under [`phases/`](./phases/phase-00-foundation.md), and
the [RUSM vs Lunatic comparison](./lunatic-comparison.md) for the per-phase
"borrow vs beat" efficiency playbook we update as each gap closes.
