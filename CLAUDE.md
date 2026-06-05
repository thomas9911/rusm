# CLAUDE.md — working notes for RUSM

RUSM is an **Erlang-inspired WebAssembly runtime in Rust**: isolated lightweight
processes (one Wasm instance = one Tokio task), message passing, supervision,
per-actor sandboxing, "write blocking code → runtime makes it async", and secure
distributed clusters you can hook into live. See `README.md` for the pitch and
`docs/` for the full story.

## Status

**Phase 0 — observability foundation.** Built: the metrics core, the live
observer model, the benchmark harness + WebSocket server, the `rusm` CLI, the
React dashboard, and runnable examples — all on synthetic data. The runtime
internals (Wasmtime engine, processes, host ABI, …) arrive in later phases; see
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
