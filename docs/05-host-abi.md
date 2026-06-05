# Host ABI reference

RUSM guests will be plain `wasm32-wasip1` modules that import host functions
under the `rusm::*` namespace (mirroring Lunatic's `lunatic::*`). This page is the
reference for that ABI and **grows phase by phase**.

> **Phase 0:** no guest ABI exists yet — there is no Wasmtime engine until
> Phase 1 and no host functions until Phase 3. The wire protocol below is the
> only "ABI" today: how the benchmark node talks to its clients.

## Planned host modules (by phase)

| Module | Phase | Functions (sketch) |
| --- | --- | --- |
| `rusm::process` | 3 | `id()`, `spawn()`, `spawn_link()`, `cancel()`, `wait()` |
| `rusm::message` | 4 | `create()`, `write_data()`, `send(pid)`, `receive()`, `read_data()` |
| `rusm::timer` | 8 | `sleep(ms)`, `after(ms)` |
| `rusm::registry` | 8 | `register(name)`, `lookup(name)` |
| `rusm::net` | 8 | `tcp_listen()`, `tcp_accept()`, `tcp_connect()`, `read()`, `write()` |
| WASI preview1 | 7 | clocks, random, env, stdio, scoped fs |

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
