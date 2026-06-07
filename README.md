# RUSM

**An Erlang-inspired WebAssembly runtime in Rust.**

RUSM gives you isolated, lightweight *processes* — message passing, supervision,
fault tolerance, and secure clusters you can hook into live — the BEAM's
concurrency and connection model, in Rust. The **Erlang/OTP actor model is the
core** (pure Rust); **WebAssembly is the sandboxed execution backend** that later
runs each process as an isolated instance. Rust + Tokio do the scheduling;
Wasmtime does the isolation.

> **Status: Phase 9 of 11 complete.** RUSM **hosts real WASM components** as
> isolated, supervised processes. The Wasmtime backend (`rusm-wasm`) runs each
> instance-per-process behind three bridges — **wasip1** (core modules + a raw
> `rusm::*` actor ABI + cross-process byte streams), **wasip2** (components, the
> `rusm:runtime` **WIT actor world** — `self`/`spawn`/`monitor`/`send`/`receive`/
> `list`/`info`/`kill`/`register`, the Erlang `Process` API in any language), and
> **wasip3** (the `@0.3.0` async WASI interfaces). **Default-deny capability
> profiles** (fs/net/env/memory/spawn), epoch preemption, and a spawn path tuned to
> **~440k component spawns/sec**. **Guest ergonomics (Phase 8):** write components
> in **TypeScript** (the `rusm` npm package) or **Rust** (the `rusm-rs` crate) — a
> service is just exported functions, called from another component through a
> concealed **typed client** (`spawn<typeof Svc>("svc")` → `await svc.method(...)`,
> with streaming + callbacks), with an in-guest **`Supervisor`** (one/all/rest-for-one)
> and `rusm dev` watch+reload. An **app model** lets you
> `rusm dev` a project: `rusm.toml` `[[components]]`, source under `components/`,
> built to `./wasm/`, spawned under their capabilities — env the Rust way (process
> env, then `.env`). Underneath, the Wasm-free OTP core (`rusm-otp`) spawns,
> schedules, kills, messages, **supervises**, **manages**, and **connects** **real**
> lightweight processes — links, monitors, `trap_exit`, exit cascades, a named
> registry, timers, graceful shutdown, and **TCP** (one process per connection).
> Nine benchmarks show real numbers (release) — *every* dashboard scenario now runs
> on live data: spawn-storm **~2.4M spawns/sec**,
> ping-pong **~21M messages/sec** (round-trip p50 <1 µs), fault-recovery
> **~285k restarts/sec**, fairness keeping bystanders at **~50M+ ops/sec**
> (peaking past **400M** when cores are free) under tight-loop spinners,
> module-storm **~475k wasip1 core-module spawns/sec** (the direct Lunatic
> head-to-head), component-storm **~440k component spawns/sec**, stream-pipe
> piping bytes between processes at **multiple GB/sec**, connection-storm
> holding **thousands of concurrent connections** (connect p50 sub-millisecond) —
> the connection ceiling is the OS, not RUSM — and distributed-fanout doing
> **~550k cross-node messages/sec** over QUIC+TLS. These are measured under everyday
> load and scale up with free CPU. **Distributed clusters (Phase 9):** the
> Wasm-free `rusm-cluster` crate connects nodes over **QUIC + TLS** so processes
> message across machines — cross-node `send`, a gossiped **global registry**,
> **remote spawn**, and **live attach** — at **~550k cross-node messages/sec**
> (~39µs p50 round-trip, loopback). See the [roadmap](docs/02-roadmap.md).

## Why

Existing options force a trade-off. The BEAM has the model we want (cheap
processes, "let it crash", distribution, live introspection) but only runs BEAM
languages. WebAssembly component-model runtimes give language portability but a
heavy, rigid wiring model and no actor semantics. RUSM takes the BEAM's ideas,
builds them in pure Rust, and uses WebAssembly purely as the per-process sandbox:

- **Isolated processes** — each process gets its own stack, heap, and (with the
  Wasm backend) its own sandboxed instance. A crash stays contained.
- **Massive concurrency** — processes are Tokio tasks scheduled M:N over a few OS
  threads. The goal is hundreds of thousands of spawns per second.
- **Write blocking code, get async for free** — Wasmtime fibers suspend a guest's
  "blocking" call while the host awaits; you never write `async` in a guest.
- **Fault tolerance** — links and supervisors, Erlang-style.
- **Secure clusters you can hook into** — nodes connect over TLS, and you can
  attach a live REPL/observer to a running node (like `iex --remsh`).

See [`docs/00-vision.md`](docs/00-vision.md) for the full rationale,
[`docs/01-architecture.md`](docs/01-architecture.md) for how Rust + Tokio +
Wasmtime map onto the BEAM, and the [RUSM vs Lunatic
comparison](docs/lunatic-comparison.md) for where we borrow and where we aim to
beat the runtime that inspired this.

## Installation

Prerequisites:

- **Rust** 1.94+ (`rustup`). To build guest components/modules add the Wasm
  targets: `rustup target add wasm32-wasip2 wasm32-wasip1` (`rusm build` uses
  `wasm32-wasip2` for components; `wasm32-wasip1` for core modules).
- **Bun** 1.3+ (the dashboard and docs site use Bun — never Node.js).

```sh
git clone https://github.com/archan937/rusm.git
cd rusm
cargo build
```

## Quick start

**See it in action — one command:**

```sh
make dashboard
```

That builds the CLI, starts a node, launches the dashboard, and opens your
browser. Pick a scenario, press **Run**, and watch live throughput, latency
percentiles, and the host/instance observer. (Spawn-storm, ping-pong, and
fault-recovery run **real** `rusm-otp` processes; the remaining scenarios are
synthetic until their phase lands.)

Prefer to drive the pieces yourself? (`make help` lists everything)

```sh
make node       # start a node on ws://127.0.0.1:4000
make ui         # the dashboard only, in another terminal
make attach     # a live REPL into the node (like iex --remsh)
make run        # run a scenario in the terminal (SCENARIO=… SECONDS=…)
make example    # run an example app (EX=headless_run)
```

## Configuration

`rusm node start` reads an optional **`rusm.toml`** from the working directory
(or `--config <file>`). Layering is **built-in defaults → `rusm.toml` → CLI
flags**:

```toml
listen = "127.0.0.1:4000"   # WebSocket address
profile = "balanced"        # light | balanced | max — how hard a storm drives the machine
ticks_per_second = 20       # snapshot / sampling rate (Hz)
```

```sh
rusm node start                                    # uses ./rusm.toml if present, else defaults
rusm node start --config prod.toml                 # an explicit config file
rusm node start --profile max --listen 0.0.0.0:80  # flags override the file
```

The `profile` can also be switched **live** from the dashboard — see
[`docs/03-benchmark-dashboard.md`](docs/03-benchmark-dashboard.md).

## Running tests

```sh
make test       # all Rust tests + dashboard tests
make cov        # coverage (gate: >= 98%, mostly 100%)
make fmt-check  # cargo fmt + Prettier
```

…or directly: `cargo test`, `cargo test -p rusm-otp`, and
`cd bench/dashboard && bun test --coverage`.

## Docs site

The documentation under `docs/` is also a [VitePress](https://vitepress.dev) site
(landing page, sidebar, search, dark mode):

```sh
make docs        # live preview   (or: make docs-build for the static site)
```

## Crates

RUSM is a Cargo workspace; each crate has a single job. The core is **Wasm-free
by construction** — Wasm lives only in the `rusm-wasm` backend.

| Crate | Kind | Purpose |
| --- | --- | --- |
| `rusm-otp` | lib | **The Erlang/OTP core** — processes, scheduler, mailboxes & selective receive, links/monitors/supervision, registry, timers, TCP. Pure Rust, **no `wasmtime` dependency** (usable standalone). Built up across Phases 1–5. |
| `rusm-wasm` | lib | **The Wasmtime backend** — the *only* crate that touches Wasmtime; runs each process as a sandboxed Wasm instance behind three bridges (`wasip1` core modules + raw actor ABI + byte streams, `wasip2` components + WIT actor world, `wasip3` `@0.3.0` interfaces), with default-deny capabilities, epoch preemption, pooling + CoW — all behind the same `rusm-otp` API. |
| `rusm-cluster` | lib | **The distributed transport** (Phase 9) — connects nodes over QUIC + TLS for cross-node `send`, a gossiped global registry, remote spawn, and live attach. Over `rusm-otp`, **no `wasmtime` dependency**. |
| `rusm-metrics` | lib | Counters, HdrHistogram-backed latency percentiles, ring-buffer time-series. |
| `rusm-observer` | lib | Low-overhead live-observer snapshots — aggregate counters plus a sampled per-instance table, with a detail on/off toggle. |
| `rusm-bench` | lib + bin | Scenarios, the synthetic data source, the eight real engines (spawn-storm, ping-pong, fault-recovery, connection-storm, fairness, module-storm, component-storm, stream-pipe), the run aggregator, the wire protocol, and the WebSocket server. Binary: `rusm-bench serve` / `run`. |
| `rusm-cli` | bin (`rusm`) | The `rusm` command: `node start`, `attach <url>` (live REPL), and the app model — `build` / `run` / `dev` over `rusm.toml [[components]]`. |
| `rusm-rs` | lib (guest) | **The Rust guest crate** — write a component/service in Rust over the actor world: `Pid`/`send`/`receive` (serde)/`spawn`/registry/`Stream`, plus the `#[rusm_rs::service]` macro (dispatch loop + typed `Client`: call/cast/streaming/callbacks). Wasm-only (built for `wasm32-wasip2`), excluded from the host workspace. |
| `rusm-rs-macros` | proc-macro | The `#[rusm_rs::service]` macro behind `rusm-rs`. |

Not crates: the dashboard at `bench/dashboard` (Bun/React); docs under `docs/`. The
**rusm-ts** guest (TS/Bun) ships as the embedded js-runner in `rusm-wasm/js-runner`.

## Examples

`examples/` holds small, ready-to-run programs — each its own directory with a
README and expected output. Run with `cargo run -p rusm-bench --example <name>`:

| Example | What it shows |
| --- | --- |
| [`headless_run`](examples/headless_run) | Drive the benchmark runner directly (no network) and print sampled ticks. |
| [`synthetic_source`](examples/synthetic_source) | The deterministic synthetic data source, reproducible per `(scenario, tick)`. |
| [`observer_overhead`](examples/observer_overhead) | The observer's detail on/off switch (basis of the overhead proof). |
| [`embedded_node`](examples/embedded_node) | Embed a node and serve the live protocol for the dashboard / REPL. |

See [`examples/README.md`](examples/README.md) for end-to-end recipes.

## Acknowledgements

My Elixir years left me with a clear itch: I wanted Elixir's concurrency and
process model, but in Rust, running WebAssembly — on infrastructure that's
lightweight, optimal, and *crazy fast*.
[**Lunatic**](https://github.com/lunatic-solutions/lunatic) by Bernard Kolobara
nailed exactly that pitch — its whole message and how it profiled itself was spot
on (Wasmtime + Tokio + stack switching, processes as Wasm instances). Honestly,
**if Lunatic were still active and up to date, RUSM would never have been built**
— I'd just use it. RUSM exists to carry that torch forward.

## License

MIT © [Paul Engel](https://github.com/archan937)
