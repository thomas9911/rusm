# CLAUDE.md — working notes for RUSM

RUSM is an **Erlang-inspired WebAssembly runtime in Rust**: isolated lightweight
processes (one Wasm instance = one Tokio task), message passing, supervision,
per-actor sandboxing, "write blocking code → runtime makes it async", and secure
distributed clusters you can hook into live. See `README.md` for the pitch and
`docs/` for the full story.

## Status

**Phase 7 of 10 — complete.** RUSM **hosts real WASM components** as isolated,
supervised processes. The Wasmtime backend (`rusm-wasm`, the *only* crate that
touches Wasmtime) runs each component instance-per-process via the **component
model** (`wasmtime-wasi`; `bridges/{wasip1,wasip2}.rs` over a shared core — the
component path wires **WASI p2**; the `p3` crate feature is compiled in but a
`wasip3` bridge is a follow-on). It exposes a `rusm:runtime` **WIT actor world** (`bindgen!`): a
component calls `self`/`send`/`receive`/`list`/`info`/`kill`/`register`/`whereis`/
`set-label` — the Erlang `Process` API, callable from Rust or TS guests — backed
by thin calls into `rusm-otp`. **Default-deny capability profiles** (`caps.rs`:
Sandboxed/NetworkClient/Trusted) build a `WasiCtx` + a `StoreLimiter` memory cap.
The spawn path is optimized — pooling allocator + copy-on-write + per-module
`InstancePre` + **precomputed export index** + **opt-in mailbox depth** (default
off → zero hot-path atomics) + single runtime-handle clone — sustaining **~440k
component spawns/sec** (live `component-storm` scenario). Trap → process
`Crashed`. An **app model** (`rusm-cli`): `rusm.toml [[components]]`, `rusm build`
(cargo `wasm32-wasip2` per `components/*`, no jco), `rusm run`/`rusm dev`; env the
Rust way (process env, then `.env` via `dotenvy`). `rusm-otp` stays Wasm-free
(verified: no `wasmtime` in its dep tree).

Underneath, the Wasm-free OTP core (`rusm-otp`) spawns,
schedules, kills, messages, supervises, manages, and **connects** **real**
lightweight processes: links, monitors, exit reasons, `trap_exit`, `spawn_link`,
`exit/2`, exit cascades, a named **registry**, **timers** (`send_after`/`cancel`),
graceful `shutdown`, **TCP** (`listen`/`connect`, one process per connection),
process **introspection** (`list`/`info`/`set_label`), and **byte streams**
(`Received::Stream`, Tokio-backpressured). Seven benchmarks are live (release):
spawn-storm (~2.4M spawns/sec), ping-pong (~21M messages/sec, round-trip p50
<1 µs), fault-recovery (~285k restarts/sec), fairness (bystanders at ~50M+
ops/sec — past 400M on free cores — under tight-loop spinners), module-storm
(~475k wasip1 core-module spawns/sec — the direct Lunatic head-to-head),
component-storm (~440k component spawns/sec), and connection-storm (thousands of
concurrent connections; connect p50 sub-millisecond). Numbers are measured under
everyday machine load and scale up with free CPU.
Each process keeps a single channel; exit signals ride the mailbox (a `Received`
enum) and kill rides a `futures` abort handle (no second signal channel — we beat
Lunatic's two). The registry is a sharded `DashMap`, timers use Tokio's timer
wheel, and TCP is process-per-connection — the connection ceiling is the OS (fds,
ports), not RUSM. Phase 0 (metrics, live observer, benchmark harness + WebSocket
server, `rusm` CLI, React dashboard, examples) is done. The **wasip1 bridge**
(`bridges/wasip1.rs`) runs preview1 core modules as processes too — preview1 WASI,
the same default-deny caps + `StoreLimiter`, the precomputed export index, and a
raw `rusm::*` actor ABI over linear memory, including **cross-process byte
streaming** (`stream_open`/`write`/`close`/`accept`/`read` over the Wasm-free
`StreamHandle`, real Tokio back-pressure) — RUSM on Lunatic's home turf
(module-storm bench). Deferred follow-ons: the same stream ops through the
component WIT world + a native p3-typed `stream<u8>`, and `rusm dev` filesystem
watch/reload. TLS folds into the Phase 9 secure cluster transport. See
`docs/02-roadmap.md`.

## Tech stack

- **Rust** (host) + **Tokio** (scheduler/IO) + **Wasmtime** (component guests, in `rusm-wasm`).
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
