# Phase 6 — Wasmtime as the process backend

**Goal:** the pivot the whole design was built for — swap a process body from a
native Rust closure to a **sandboxed Wasm instance**, behind the *same*
`rusm-otp` API. Task-level isolation becomes **true memory isolation**.
**Graduates:** the **fairness** scenario to live Wasm.

## Why this is the keystone

Phases 1–5 made the actor model real and measurable on native bodies. Phase 6
proves the bet: because the OTP layer was designed Wasm-ready, adding Wasmtime is
**additive, not a rewrite**. A process is still a Tokio task and a table entry —
now its body is a guest instance that can crash, loop, or misbehave without
touching anything else.

## The hard boundary

All Wasmtime lives in **`rusm-wasm`**. The core (`rusm-otp`) still has *zero*
`wasmtime` dependency — the dependency graph enforces it. `rusm-wasm` drives the
core through its public API; Wasm never bleeds into Wasm-irrelevant code.

## What we built (TDD throughout)

1. **`WasmRuntime`** over a shared `rusm-otp` `Runtime` — owns the Wasmtime
   `Engine`, a `Linker<Host>`, and shared `Counters`.
2. **Instance-per-process** — `compile(wat) -> Module`, `prepare(module) ->
   InstancePre<Host>`, `spawn(prepared, entry) -> ProcessHandle`. Each spawn
   instantiates a fresh, isolated instance as a rusm-otp process.
3. **Fast spawns** (instance-per-process, far cheaper than a naive on-demand
   allocator; the optimized component path reaches ~440k spawns/sec — see
   [Phase 7](./phase-07-components.md)) via three levers on one `Engine`:
   - **pooling allocator** — instances/memories/tables recycled from a pool,
   - **`memory_init_cow`** — copy-on-write memory images, so a fresh instance
     doesn't zero/copy its whole linear memory,
   - **per-module `InstancePre`** — type-checking and host-import resolution done
     **once** at `prepare`, not per spawn.
4. **Epoch-interruption preemption** — even a guest in `loop { }` is forced to
   yield and stays killable. The epoch is bumped on a **dedicated OS thread**, not
   a Tokio task — *critical*: as a task it could be starved by the very guests it
   must preempt, deadlocking. The store yields async on each epoch tick.
5. **Host ABI via `Caller::data`** — `rusm::self_pid` (the guest's own pid) and
   `rusm::notify` (bumps a shared counter), the seed of the
   [host ABI](../05-host-abi.md).
6. **Trap → `ExitReason::Crashed`** — a guest trap is reported through the same
   exit machinery as a native crash from [Phase 3](./phase-03-supervision.md).
7. **Fairness engine** (`rusm-bench`) — Wasm spinners saturate **every core**
   while Wasm bystanders keep calling `notify`; a nonzero bystander rate (~60M+
   ops/sec) *is* the proof that preemption is yielding the spinners.

## Design notes — efficiency & honesty

- **One `Engine`, shared levers.** Pooling + CoW + `InstancePre` all hang off the
  same engine, so the cost moves from per-spawn to one-time per-module.
- **Dedicated epoch thread.** The single most important correctness fix in this
  phase — preemption that can itself be preempted isn't preemption.
- **The spawn bench counts honestly.** It asserts `notifications == n` (every
  guest actually ran its body), so crashed instances can't inflate the rate.

## Concepts introduced

- [Wasm instance as a process](../concepts/wasm-instance-as-process.md),
  [fibers & blocking→async](../concepts/fibers-and-blocking-to-async.md), and
  [epoch preemption](../concepts/epoch-preemption.md).

## Play with it

```sh
cargo run -p rusm-bench -- run fairness 5     # spinners saturate cores; bystanders still run
cargo test -p rusm-wasm                       # instance-per-process, traps, preemption
```

## Verification

`cargo test -p rusm-wasm` green (add, host-import call, pid reporting, trap →
crash, spinner preemption); fairness live in the dashboard; the Wasm-free
invariant holds (no `wasmtime` anywhere under `rusm-otp`).

## Next

[Phase 7](./phase-07-components.md): **component hosting** — run real WASM
*components* (the component model + WASI p2/p3) as RUSM processes, with a
`rusm:runtime` actor ABI and default-deny per-process capabilities.
