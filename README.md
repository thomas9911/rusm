# RUSM

**An Erlang-inspired WebAssembly runtime in Rust.**

RUSM gives you isolated, lightweight *processes* — message passing, supervision,
fault tolerance, and secure clusters you can hook into live — the BEAM's
concurrency and connection model, in Rust. The **Erlang/OTP actor model is the
core** (pure Rust); **WebAssembly is the sandboxed execution backend** that later
runs each process as an isolated instance. Rust + Tokio do the scheduling;
Wasmtime does the isolation.

> **Status: Phase 1 of 10.** The Wasm-free OTP core (`rusm-otp`) already spawns,
> schedules, and kills **real** lightweight processes — the spawn-storm benchmark
> shows real numbers (~170k+ spawns/sec, ~2 µs p50 spawn latency). Messaging,
> supervision, the Wasmtime backend, and clustering come in later phases. See the
> [roadmap](docs/02-roadmap.md).

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

- **Rust** 1.94+ (`rustup`). The `wasm32-wasip1` target is only needed once the
  Wasm backend lands (Phase 6): `rustup target add wasm32-wasip1`.
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
percentiles, and the host/instance observer. (Spawn-storm runs **real**
`rusm-otp` processes; the other scenarios are synthetic until their phase lands.)

Prefer to drive the pieces yourself? (`make help` lists everything)

```sh
make node       # start a node on ws://127.0.0.1:4000
make ui         # the dashboard only, in another terminal
make attach     # a live REPL into the node (like iex --remsh)
make run        # run a scenario in the terminal (SCENARIO=… SECONDS=…)
make example    # run an example app (EX=headless_run)
```

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
by construction** — Wasm lives only in the (planned) backend.

| Crate | Kind | Purpose |
| --- | --- | --- |
| `rusm-otp` | lib | **The Erlang/OTP core** — processes, scheduler, signals, lifecycle. Pure Rust, **no `wasmtime` dependency** (usable standalone). Built up across Phases 1–5. |
| `rusm-metrics` | lib | Counters, HdrHistogram-backed latency percentiles, ring-buffer time-series. |
| `rusm-observer` | lib | Low-overhead live-observer snapshots — aggregate counters plus a sampled per-instance table, with a detail on/off toggle. |
| `rusm-bench` | lib + bin | Scenarios, the synthetic data source, the real spawn-storm engine, the run aggregator, the wire protocol, and the WebSocket server. Binary: `rusm-bench serve` / `run`. |
| `rusm-cli` | bin (`rusm`) | The `rusm` command: `node start` and `attach <url>` (live REPL). |

**Planned:** `rusm-wasm` (Phase 6) — the *only* crate that touches Wasmtime; it
runs each process as a sandboxed Wasm instance behind the same `rusm-otp` API.
Not crates: the dashboard at `bench/dashboard` (Bun/React); docs under `docs/`.

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
