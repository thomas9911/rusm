# RUSM

**An Erlang-inspired WebAssembly runtime in Rust.**

RUSM runs WebAssembly modules as isolated, lightweight *processes* — millions of
them — with message passing, supervision, and fault tolerance, and lets you form
secure clusters of nodes you can hook into live. Think the BEAM's concurrency and
connection model, with WebAssembly as the bytecode and Rust + Tokio + Wasmtime as
the engine.

> **Status:** Phase 0 — the observability foundation (metrics, live observer, and
> a benchmark dashboard running on synthetic data). The runtime internals land in
> later phases; see [`docs/02-roadmap.md`](docs/02-roadmap.md).

## Why

Existing options force a trade-off. The BEAM has the model we want (cheap
processes, "let it crash", distribution, live introspection) but only runs BEAM
languages. WebAssembly component-model runtimes give language portability but a
heavy, rigid wiring model and no actor semantics. RUSM takes the BEAM's ideas and
rebuilds them on WebAssembly:

- **Isolated processes** — one Wasm instance per process, each with its own stack,
  heap, and permissions. A crash stays contained.
- **Massive concurrency** — processes are Tokio tasks scheduled M:N over a few OS
  threads. The goal is hundreds of thousands of spawns per second.
- **Write blocking code, get async for free** — Wasmtime fibers suspend a guest's
  "blocking" call while the host awaits; you never write `async` in a guest.
- **Fault tolerance** — links and supervisors, Erlang-style.
- **Secure clusters you can hook into** — nodes connect over TLS, and you can
  attach a live REPL/observer to a running node (like `iex --remsh`).

See [`docs/00-vision.md`](docs/00-vision.md) for the full rationale and
[`docs/01-architecture.md`](docs/01-architecture.md) for how Rust, Tokio, and
Wasmtime map onto the BEAM.

## Installation

Prerequisites:

- **Rust** 1.94+ (`rustup`), with the `wasm32-wasip1` target for guest code in
  later phases: `rustup target add wasm32-wasip1`.
- **Bun** 1.3+ (the dashboard and docs site use Bun — never Node.js).

```sh
git clone https://github.com/archan937/rusm.git
cd rusm
cargo build
```

## Quick start

Start a node and open the dashboard:

```sh
# 1. Start a RUSM node (serves the control/observer channel)
cargo run -p rusm-cli -- node start

# 2. In another terminal, run the benchmark + observer dashboard
cd bench/dashboard
bun install
bun run dev          # open the printed URL

# 3. Or hook into the running node from the terminal (REPL, like iex --remsh)
cargo run -p rusm-cli -- attach ws://127.0.0.1:4000
```

In the dashboard, pick a scenario from the menu and press **Run** to watch live
latency, throughput, and connection graphs, plus the host/instance observer.

## Running tests

Rust:

```sh
cargo test                      # all crates
cargo test -p rusm-metrics      # a single crate
cargo llvm-cov --summary-only   # coverage (gate: >= 98%)
```

Dashboard (Bun):

```sh
cd bench/dashboard
bun test --coverage
```

Formatting:

```sh
cargo fmt --check
bunx prettier --check bench/dashboard docs
```

## Project layout

| Path | What |
| --- | --- |
| `crates/rusm-metrics` | Counters, latency histograms, time-series |
| `crates/rusm-observer` | Live observer snapshot model (low-overhead sampling) |
| `bench/rusm-bench` | Load harness, scenarios, WebSocket metrics server |
| `bench/dashboard` | React (Bun) benchmark + observer dashboard |
| `rusm-cli` | `rusm node start`, `rusm attach` |
| `docs/` | Documentation (markdown source + VitePress site) |
| `examples/` | Runnable, ready-to-try examples |

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
