# Vision — why RUSM exists

## The itch

My Elixir years left me wanting one thing: the BEAM's process model — cheap
isolated processes, "let it crash", distribution, live introspection — but able
to run **any** language, on **WebAssembly**, on infrastructure that is
lightweight, optimal, and *crazy fast*. I wanted Elixir's concurrency and process
model, in Rust, running Wasm.

[Lunatic](https://github.com/lunatic-solutions/lunatic) proved it was possible
and pitched it perfectly. But it went quiet. **RUSM exists to carry that torch
forward** — if Lunatic were still active and current, I'd just use it.

## The end goal

A runtime where:

- **A process is an isolated Wasm instance** — its own stack, heap, syscalls, and
  permissions. One crash can never corrupt another.
- **Concurrency is massive and cheap** — processes are Tokio tasks multiplexed
  M:N over a few OS threads, targeting hundreds of thousands of spawns per second.
- **You write blocking code** — Wasmtime fibers suspend a guest's "blocking" call
  while the host awaits; guests never see `async`.
- **Failure is survivable** — links and supervisors, Erlang-style.
- **Clusters are first-class** — nodes connect over TLS, processes spawn and
  message across nodes, and you can **attach a live REPL/observer to a running
  node** (like `iex --remsh`).

## How we prove it

The north-star artifact is a **benchmark + live-observer dashboard** that
stress-tests RUSM and shows latency, throughput, peak concurrency, and the live
process table — culminating in proving the **"300k/s connections"** claim
(observer-on vs observer-off, to show observability is nearly free).

We build it in small, test-driven phases (see [the roadmap](./02-roadmap.md)),
each one teaching one concept. Phase 0 built the dashboard and observability
foundation on synthetic data, so every later phase has something to measure with;
Phases 1–5 made the OTP core real (processes, messaging, supervision, management,
TCP); Phase 6 slotted in Wasmtime as the process backend; and by **Phase 7** a
RUSM process is a real, supervised, sandboxed **WASM component** (or wasip1 core
module) hosting WASI p1/p2/p3; by **Phase 9** nodes cluster over QUIC+TLS — and
all ten dashboard benchmarks now run on real data. See the
[roadmap](./02-roadmap.md) for where things stand.
