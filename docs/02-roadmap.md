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
| **10 ✅** | **Scale & hardening** — *not raw speed* (throughput/latency is already at the isolation-model ceiling: ~440k component spawns/s, ~21M msgs/s). **On-demand instance tier** (`WasmRuntime::with_overflow` — when the pooled cap is full, spawn from an on-demand engine so the live *Wasm*-process count is bounded by **available memory**, not a compile-time pool size); **opt-in bounded mailboxes** (`Runtime::with_mailbox_capacity` — shed *user* messages past capacity, system signals never shed); **cluster security hardening** (`ClusterCa` issues per-node certs, mutual TLS, foreign-CA peers rejected — replacing Phase 9's pre-shared cert); **supervisor restart-intensity** (windowed `{max_restarts, max_seconds}` in both in-guest supervisors). All with no spawn/message regression. See the [design analysis](./design-analysis.md). | — |
| 11 | **Standard-WASI surface & wstd compatibility** — invoke the standard `wasi:cli/run` entrypoint (so stock command components run unchanged), host `wasi:http`, support [`wstd`](https://github.com/bytecodealliance/wstd)-based guests, and a native p3-typed **`stream<u8>`** signature for the actor world (the byte streams already work over a handle ABI — this is the standards-first refinement). RUSM stays a standards-first host; the actor world stays opt-in. Serving **HTTP / WS / SSE** from a component is the headline goal — see the [design preview](./serving-http-ws-sse.md) for the host model, RS/TS guest code, and the benchmark plan. (Can be sequenced earlier — `wasi:http` pairs with the HTTP-serving goal.) | — |
| 12 | **Edge & cluster hardening** — close the [security-audit](#security-audit-phase-12-scope) exposures before RUSM faces *untrusted* traffic. **Serve-path admission control**: a bound on concurrent in-flight instances + a request-body size cap + a per-request wall-clock timeout (epoch preemption only bounds *CPU*; a slow-loris body or connection flood is unbounded today), degrading to a graceful `503`/close rather than leaning on the pool cap. **Bounded serve-path mailboxes by default** so a fast peer can't OOM a slow WS handler (the opt-in `with_mailbox_capacity` exists; the serve path doesn't set it). **Serving TLS** (`https`/`wss`) — moved here from Phase 11. **Cluster trust hardening**: signed `name→node` ownership so a *mutually-authenticated but malicious* peer can't poison the gossiped global registry (mTLS authenticates the connection, not the gossip payload), plus poison-resistant locking in the `rusm-cluster` control loop (replace `lock().unwrap()`). The sandbox, capability model, trap isolation, supervision, and mailbox load-shedding are **already sound** — these are network-edge and peer-trust gaps, not sandbox breaks. | — |

## What's shipped so far (Phases 0–10)

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
- **Phase 10 — scale & hardening:** on-demand instance tier (lift the pooled cap),
  opt-in bounded mailboxes (overload load-shed), per-node certs under a cluster CA
  + mutual TLS, and windowed supervisor restart-intensity — no spawn/message
  regression.
- **Phase 11 — serving (in progress):** run a component as a high-throughput
  **HTTP / WS / SSE** server. The serving engine is **built and measured** —
  `WasmRuntime::http_server` (instance-per-request `wasi:http`), `ws_server` (one
  sandboxed component process per WebSocket connection, replies via a Wasm-free writer
  process), and SSE (a `wasi:http` streaming body). **`rusm serve`** now hosts
  `rusm.toml [[serve]]` entries (`name`, `protocol` = `http`|`sse`|`ws`, `listen`,
  `capability`) on real TCP ports, loading `wasm/<name>.{wasm,js}` (HTTP/SSE via the
  `http_server` path, WS via `ws_server`); **`rusm new <name>`** scaffolds a
  ready-to-serve TS HTTP app. Serving is benchmarked **out-of-process** by the
  `rusm-loadtest` binary against a live `rusm serve` port — loopback: HTTP ~46k req/s
  (0% errors), WS ~146k round-trips/s (256 held), SSE ~609k events/s (256 held). See
  [serving HTTP/WS/SSE](./serving-http-ws-sse.md). Serving TLS and the edge-hardening
  it pairs with move to **Phase 12** (below).
  `rusm-otp` stays Wasm-free (hyper/tungstenite/`wasi:http` live only in `rusm-wasm`).

## Planned

### Security audit (Phase 12 scope)

A security/robustness review found the **core sound**: the capability sandbox is
true default-deny, spawn-from-guest is non-escalating, a guest trap is isolated to
that one process (no scheduler/runtime crash, no cascade), TS guests are sandboxed
exactly like Rust ones (rquickjs compiled to `wasm32-wasip2`), the `rusm-otp` core is
panic-isolated (sharded `DashMap`, `Drop`-guard reaping), cluster transport enforces
**mutual TLS** (foreign-CA peers rejected), remote spawn is gated to pre-registered
factories, supervisor restart-intensity is windowed, and bounded mailboxes shed only
*user* messages (system/exit signals are never dropped). **No sandbox escapes or
privilege escalation.**

The gaps are **operational (DoS hardening) and trust-model**, not architectural —
and become **Phase 12**:

- **Serve-path admission control (medium).** `HttpServer`/`WsServer` accept per
  connection/request with no concurrency bound, no request-body cap, and no
  per-request timeout (epoch only bounds CPU). It degrades *gracefully* at the pool
  cap — verified, **not** a panic — but with the on-demand overflow tier a flood is
  memory-bounded, not count-bounded. Add a semaphore + body cap + request timeout →
  graceful `503`.
- **Default-unbounded serve mailboxes (medium).** Erlang-compatible, but a fast peer
  flooding a slow WS handler is an OOM vector; bound the serve path's mailboxes by
  default.
- **Plaintext serve (known).** `http://`/`ws://` only today; serving TLS lands here.
- **Cluster peer-trust (medium).** mTLS authenticates the *connection*; a malicious
  *authenticated* peer can still advertise false `name→node` ownership in the gossiped
  global registry (silent misrouting). Sign ownership; and replace `lock().unwrap()`
  in the control loop with poison-resistant locking.

These are the items to land before exposing `rusm serve` to untrusted traffic.
- **Ten live dashboard benchmarks** — *every* scenario now runs on real data
  (spawn-storm, ping-pong, fault-recovery, connection-storm, connection-scale,
  fairness, module-storm, component-storm, stream-pipe, distributed-fanout) + the
  standalone `cluster_fanout` benchmark. Serving throughput is measured separately by
  `rusm-loadtest` (out-of-process, vs a live `rusm serve` port), not in the dashboard.
- TDD throughout; coverage ≥98% (mostly 100%); `cargo fmt` + Prettier clean.

See the per-phase deep dives under [`phases/`](./phases/phase-00-foundation.md), and
the [RUSM vs Lunatic comparison](./lunatic-comparison.md) for the per-phase
"borrow vs beat" efficiency playbook we update as each gap closes.
