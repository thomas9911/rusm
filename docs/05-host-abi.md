# Host ABI reference

RUSM guests will be plain `wasm32-wasip1` modules that import host functions
under the `rusm::*` namespace (mirroring Lunatic's `lunatic::*`). This page is the
reference for that ABI and **grows phase by phase**.

> **Foundation-first:** each Erlang capability is built as a **native Rust API
> first** (Phases 1–5), then **exposed to Wasm guests** as the `rusm::*` host ABI
> once the Wasmtime backend is slotted in at **Phase 6** (WASI at Phase 7). In
> Phase 0 there is no guest ABI at all — the only "ABI" today is the wire protocol
> below (how the benchmark node talks to its clients).

## Host modules — native capability vs guest ABI

| Module | Native capability | Exposed to guests |
| --- | --- | --- |
| `rusm::process` — `id`, `spawn`, `spawn_link`, `cancel`, `wait` | Phase 1 | Phase 6 |
| `rusm::message` — `create`, `write_data`, `send`, `receive`, `read_data` | Phase 2 | Phase 6 |
| `rusm::timer` / `rusm::registry` — `sleep`/`after`, `register`/`lookup` | Phase 4 | Phase 6 |
| `rusm::net` — `tcp_listen`/`accept`/`connect`, `read`/`write` | Phase 5 | Phase 6 |
| WASI preview1 — clocks, random, env, stdio, scoped fs | — | Phase 7 |

## Today's wire protocol (node ↔ clients)

Defined in `rusm-bench` `protocol.rs`, mirrored in the dashboard's `types.ts`.

Server → client (`serde` tagged, `snake_case`):

- `hello { scenarios: ScenarioMeta[] }` — sent on connect (the menu).
- `tick { frame: Frame }` — one sampled frame per tick.
- `error { message: string }` — a rejected command.

Client → server:

- `run { scenario: string }`
- `stop`
- `set_observer_detail { enabled: bool }`

A `Frame` = `{ scenario, running, uptime_ms, ops_per_sec, peak_concurrent,
latency, throughput, observer }`.
