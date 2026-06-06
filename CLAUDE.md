# CLAUDE.md — working notes for RUSM

RUSM is an **Erlang-inspired WebAssembly runtime in Rust**: isolated lightweight
processes (one Wasm instance = one Tokio task), message passing, supervision,
per-actor sandboxing, "write blocking code → runtime makes it async", and secure
distributed clusters you can hook into live. See `README.md` for the pitch and
`docs/` for the full story.

## Status

**Phases 1–5 complete; Phase 6 backend landed.** The Wasmtime backend
(`rusm-wasm`, the *only* crate that touches Wasmtime) runs each process as an
isolated Wasm instance: instance-per-process, a host ABI, epoch preemption
(infinite-loop guests yield and stay killable), and the efficiency levers —
pooling allocator + copy-on-write + a per-module `InstancePre` — for **~167k Wasm
spawns/sec** (3.2× a naive on-demand allocator). Trap → process `Crashed`.
Remaining Phase 6: graduate the fairness scenario to real Wasm on the dashboard
and re-measure spawn/conn. `rusm-otp` stays Wasm-free (verified: no `wasmtime` in
its dep tree).

Underneath, the Wasm-free OTP core (`rusm-otp`) spawns,
schedules, kills, messages, supervises, manages, and **connects** **real**
lightweight processes: links, monitors, exit reasons, `trap_exit`, `spawn_link`,
`exit/2`, exit cascades, a named **registry**, **timers** (`send_after`/`cancel`),
graceful `shutdown`, and **TCP** (`listen`/`connect`, one process per connection).
Four benchmarks are live — spawn-storm (~1.4M spawns/sec), ping-pong (~3M
messages/sec, round-trip p50 ~2 µs), fault-recovery (~100k restarts/sec), and
connection-storm (thousands of concurrent real connections, each a process;
connect p50 ~70 µs). Each process keeps a single channel; exit signals ride the
mailbox (a `Received` enum) and kill rides a `futures` abort handle (no second
signal channel — we beat Lunatic's two). The registry is a sharded `DashMap`,
timers use Tokio's timer wheel, and TCP is process-per-connection — the
connection ceiling is the OS (fds, ports), not RUSM. Phase 0 (metrics, live
observer, benchmark harness + WebSocket server, `rusm` CLI, React dashboard,
examples) is done. TLS folds into the Phase 9 secure cluster transport. See
`docs/02-roadmap.md`.

## Tech stack

- **Rust** (host) + **Tokio** (scheduler/IO) + **Wasmtime** (guests, later phases).
- **Bun** for all JS/TS (dashboard, docs site) — never Node.js.
- Charts: **uPlot**. Docs site: **VitePress**.

## Conventions (please keep)

- **TDD always** — write the failing test first; baby steps.
- **Coverage: aim for 100%** (≥98% floor). Rust via `cargo-llvm-cov`; dashboard
  via `bun test --coverage`. Thin glue (`main.rs`) and presentational `.tsx` are
  excluded; only genuinely-unreachable invariant guards are acceptable gaps.
- **Comments only for critical info** — no comments restating obvious code.
- **Formatting**: `cargo fmt` + Prettier. No required linter.
- **Senior, idiomatic, reference-quality** code. Self-review every change for
  weak tests, readability, DRY, and separation of concerns.
- **Wasm-free core (hard boundary).** The Erlang/OTP core (`rusm-otp`:
  processes, messaging, supervision, registry, scheduler) must **never** depend on
  or reference Wasmtime. All Wasm lives in `rusm-wasm` (Phase 6). Wasm must not
  bleed into Wasm-irrelevant code; the dependency graph enforces it.
- **Total awareness on sweeping changes.** For any rename/renumber/API change,
  grep the *entire* repo, fix every hit, then re-grep to prove zero stragglers.

## Commands

```sh
cargo test                                  # all Rust tests
cargo llvm-cov --workspace --ignore-filename-regex 'main\.rs' --summary-only
cargo fmt --check
cargo run -p rusm-cli -- node start         # start a node
cargo run -p rusm-cli -- attach             # local node; or attach host[:port]
cargo run -p rusm-bench -- run connection-storm 5
cargo run -p rusm-bench --example headless_run

cd bench/dashboard && bun install && bun run dev      # dashboard
cd bench/dashboard && bun test --coverage             # dashboard tests
```

## Layout

`crates/rusm-metrics`, `crates/rusm-observer`, `bench/rusm-bench` (lib+bin),
`rusm-cli` (`rusm`), `bench/dashboard` (Bun/React), `examples/`, `docs/`.
Per-crate purpose: see `README.md` → Crates.
