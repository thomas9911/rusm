# Architecture — Rust + Tokio + Wasmtime, mapped to the BEAM

Three layers, each with one job; together they reproduce (and in places beat) the
BEAM.

## Rust → the fast, safe host

Native speed with **no garbage collector**, so no stop-the-world pauses hurting
tail latency, and a tiny per-process footprint — what lets the spawn storm
sustain **~1.4M spawns/sec**. Rust's speed is the *host*: the scheduler, cross-memory message
copying, networking, and host functions. Guest actor code is **Wasm**, compiled
to native by Wasmtime's Cranelift JIT — so guest speed is Wasmtime's.

## Tokio → the process scheduler + async I/O

A multi-threaded **work-stealing** runtime that multiplexes millions of
lightweight tasks over a few OS threads (M:N) — exactly what BEAM schedulers do.
**One RUSM process (a Wasm instance) = one Tokio task.** Tokio also gives us async
networking (TCP, and QUIC for the cluster) and timers.

## Wasmtime → fast, isolated, sandboxed guests

Compiles and sandboxes each actor — isolation gives fault tolerance and
per-actor permissions. Its **fiber-based async support** suspends a guest's
"blocking" call so the Tokio task can `await`: the blocking→async trick. See
[fibers & blocking→async](./concepts/fibers-and-blocking-to-async.md).

## Beyond plain Tokio: fair preemption

Tokio is *cooperative* — a tight `loop {}` would hog a worker. The BEAM avoids
this with reduction counting; RUSM uses **Wasmtime epoch interruption** to force
even an infinite-loop guest to yield. See
[epoch preemption](./concepts/epoch-preemption.md).

## Mapping table

| BEAM | RUSM |
| --- | --- |
| process | Wasm instance + Tokio task |
| scheduler | Tokio work-stealing runtime |
| reduction counting | Wasmtime epoch interruption |
| mailbox / `send` | host-copied message + async channel |
| link / monitor / supervisor | trap propagation + link table |
| `:global` registry | distributed registry (later phase) |
| `Node.connect` / epmd | QUIC + TLS node transport (later phase) |
| `iex --remsh` / observer | `rusm attach` + dashboard observer |

## Architectural invariant — a Wasm-free core

RUSM's heart is the **Erlang/OTP actor model in pure Rust**, and it must stay that
way: **Wasm must not bleed into code where it is irrelevant.**

- **`crates/rusm-otp`** — the core: processes, mailboxes, `Signal`s, links,
  monitors, supervisors, registry, scheduler, and native connectivity. Generic
  over an abstract process **body**. **It must not depend on `wasmtime` or name
  any Wasm type.** It is usable on its own as a native-Rust OTP/actor library (an
  "rustOTP"). It is **built incrementally across Phases 1–5** (process core →
  messaging → supervision → management → connectivity) — the OTP layer is the
  whole of those phases, not just Phase 1. (Networking may live in a sibling
  Wasm-free crate, e.g. `rusm-net`, but is part of this layer.)
- **`crates/rusm-wasm`** — the *optional* execution backend (Phase 6): implements
  the body trait with Wasmtime instances. The **only** crate that touches `wasmtime`.
- **`rusm`** — the runtime = `rusm-otp` + `rusm-wasm` + host APIs + CLI.

The dependency graph **enforces** the boundary: because `rusm-otp` has no
`wasmtime` dependency, the compiler *guarantees* the actor model stands alone and
Wasmtime is a swappable backend — a structural fact, not a claim. Even messages
stay Wasm-agnostic: bytes plus opaque resource handles (`Arc<dyn Any + Send +
Sync>`), no Wasm types.

## Phase 0 shape

Phase 0 builds the **observability + harness** that every later phase plugs into:

```
rusm-metrics ─┐
rusm-observer ─┼─→ rusm-bench (runner + WebSocket server) ─→ dashboard / rusm attach
synthetic src ─┘
```

The runtime crates (engine, processes, host ABI, networking, distribution) are
introduced phase by phase — see [the roadmap](./02-roadmap.md).
