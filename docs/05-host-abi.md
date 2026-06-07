# Host ABI reference

A RUSM guest reaches the runtime through the **`rusm:runtime` actor ABI** — the
Erlang `Process` API (self, send, receive, registry, introspection, kill, …),
backed by thin calls into `rusm-otp`. The ABI comes in two equivalent shapes, one
per artifact kind, plus the standard WASI interfaces.

## Components — the `rusm:runtime` WIT actor world (wasip2/p3)

A WASI **component** imports the `rusm:runtime/actor` interface (a real WIT world,
bound with `wasmtime::component::bindgen!`), so a guest in any language calls typed
functions:

| Function | Meaning |
| --- | --- |
| `own-pid() -> pid` | the calling process's own pid |
| `send(to: pid, msg: list<u8>)` | enqueue bytes into another process's mailbox |
| `receive() -> list<u8>` | **async** — park the fiber until a message arrives |
| `list-processes() -> list<pid>` | all live pids |
| `info(pid) -> option<process-info>` | links, monitors, names, label, mailbox depth, trap-exit |
| `is-alive(pid) -> bool` / `kill(pid) -> bool` | liveness / forced termination |
| `register(name) / whereis(name) / unregister(name)` | the named registry |
| `set-label(label)` | a human-readable label for the observer |

Composition is **message passing** (spawn instances, then `send`/`receive`/
`register`/`whereis`) — *not* WIT inter-component wiring, and no lattice. Standard
**WASI p2 and p3** (`@0.2.0` and `@0.3.0` `wasi:cli`/`clocks`/`filesystem`/
`random`/`sockets`) are wired on the same component linker, gated by the process's
capability profile.

## Core modules — the raw `rusm::*` ABI (wasip1)

A `wasm32-wasip1` **core module** can't pass a WIT `list<u8>`, so the same
operations are flat imports under the `rusm` namespace that marshal through the
guest's exported linear `memory` (pointer + length):

| Import | Signature |
| --- | --- |
| `own_pid` / `notify` | `() -> i64` / `()` (the latter bumps the shared progress counter) |
| `send` | `(to: i64, ptr: i32, len: i32)` |
| `receive` | `(ptr: i32, cap: i32) -> i32` (async; returns the message length) |
| `list_processes` | `(ptr: i32, cap: i32) -> i32` (writes pids; returns the count) |
| `is_alive` / `kill` | `(pid: i64) -> i32` |
| `register` / `whereis` / `unregister` | `(ptr: i32, len: i32) -> i32`/`i64`/`i32` |
| `set_label` | `(ptr: i32, len: i32)` |
| `stream_open` | `(to: i64) -> i64` — open a byte stream to a process; returns a stream id |
| `stream_write` / `stream_close` | `(id, ptr, len) -> i32` (async, back-pressured) / `(id)` |
| `stream_accept` / `stream_read` | `() -> i64` (async) / `(id, ptr, cap) -> i32` (async; `-1` at EOF) |

Both shapes call the *same* `rusm-otp` operations; only the calling convention
differs. Standard **WASI preview1** (clocks, random, env, stdio, scoped fs) is
wired via `wasmtime_wasi::p1`, capability-gated.

## Capabilities (default-deny)

Every grant maps onto standard WASI plus a `StoreLimiter` memory cap. Named
profiles — `Sandboxed` (CPU + bounded heap only), `NetworkClient` (+ outbound
network), `Trusted` (+ stdio, large heap) — set defaults; a per-spawn
`Capabilities` builder overrides them. See
[permissions & sandboxing](./concepts/permissions-and-sandboxing.md).

## Wire protocol (node ↔ dashboard / REPL)

Defined in `rusm-bench` `protocol.rs`, mirrored in the dashboard's `types.ts`
(`serde` tagged, `snake_case`).

Server → client:

- `hello { scenarios: ScenarioMeta[], profiles: ResourceProfileMeta[] }` — the
  scenario and resource-profile menus, sent on connect.
- `tick { frame: Frame }` — one sampled frame per tick.
- `error { message: string }` — a rejected command.

Client → server:

- `run { scenario: string }`, `stop`
- `set_observer_detail { enabled: bool }`
- `set_resource_profile { profile: string }` (`light` / `balanced` / `max`)

A `Frame` = `{ scenario, running, uptime_ms, ops_per_sec, peak_concurrent,
latency, throughput, observer, profile }`. Each `ScenarioMeta` carries a `unit`
(`count` or `bytes`) so the dashboard formats throughput correctly.
