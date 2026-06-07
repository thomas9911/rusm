# Phase 7 — component hosting

**Goal:** the payoff the whole design was built toward — **run real WASM
*components* as RUSM processes**: the component-model artifact and capabilities of
wasmCloud, but on RUSM's Erlang/OTP actor model, with no lattice and no
execution-time cap. **Graduates:** the **component-storm** scenario to live data.

## Why this matters

Phases 1–6 made the actor model real on native bodies and core-module Wasm. Phase
7 hosts the *modern* artifact: a WASI **component** (what `cargo component`, `jco`
and wasmCloud emit). A component becomes a long-lived, addressable, supervised,
killable, preemptible process — the BEAM model, for the component ecosystem.

## What we built (TDD throughout)

1. **`bridges/` over a shared core** — `rusm-wasm` gains `wasmtime-wasi` (p1/p2/p3
   features) and a per-version bridge layout (`wasip1`/`wasip2`/`wasip3`) over one
   shared engine (epoch ticker, pooling allocator, CoW). The wasip2 bridge is the
   baseline component host; p3 is additive.
2. **The `rusm:runtime` WIT actor world** (`wit/world.wit` + `bindgen!`) — a
   component imports `actor` and gets typed `self`/`send`/`receive`(async)/
   `list-processes`/`info`/`is-alive`/`kill`/`register`/`whereis`/`unregister`/
   `set-label`. Each host function is a thin call into `rusm-otp` — the Erlang
   `Process` API, callable from any language (Rust via `wit-bindgen`, TS via the
   genius-style Bun+rquickjs embed). Composition is **message passing**, not WIT
   wiring — no lattice.
3. **Default-deny capabilities** (`caps.rs`) — named profiles (`Sandboxed` /
   `NetworkClient` / `Trusted`) build a `WasiCtx` (fs preopens, env, network) plus
   a `StoreLimiter` memory cap. A process gets nothing unless granted.
4. **Introspection & byte streams** (`rusm-otp`, Wasm-free) — `list`/`info`/
   `set_label`, opt-in `mailbox_depth`, and `Received::Stream` over a
   Tokio-backpressured `StreamHandle`.
5. **App model** (`rusm-cli`) — `rusm.toml [[components]]`, a `./wasm/` loader that
   spawns each component under its profile, and `rusm build` / `rusm run` /
   `rusm dev` (one toolchain: `cargo build --target wasm32-wasip2`, no jco). Env is
   the Rust way: process env, then `.env`.
6. **Lifetime superiority** — a component runs as long as it needs, stays killable
   and preemptible (epoch), supervised — **no wasmCloud-style execution timeout**.

## Performance

The spawn path is deliberately optimized: pooling allocator + copy-on-write +
per-module `InstancePre` + a **precomputed export index** (no per-spawn by-name
lookup) + **opt-in mailbox depth** (default off → zero hot-path atomics) + a single
runtime-handle clone + park-based backpressure. The live **component-storm**
scenario sustains **~440k component spawns/sec** (p50 ~1 µs). Lunatic hosts only
core modules with its own ABI — it has no component-model host at all.

## Concepts introduced

- [Permissions & sandboxing](../concepts/permissions-and-sandboxing.md) — per-process
  WASI capabilities, default-deny.
- The component model + the `rusm:runtime` WIT world — see the
  [host ABI](../05-host-abi.md).

## Play with it

```sh
cargo run --release -p rusm-bench -- run component-storm 3   # ~440k component spawns/sec
# In an app project (rusm.toml + components/ + wasm/):
rusm dev                                                     # build, then run the components
```

## Verification

`cargo test` green (component runs/reaped, trap → Crashed, epoch preempts a
component, memory-cap deny → Crashed, the full actor ABI driven by a real
`wit-bindgen` guest, component-to-component request/reply, manifest + loader);
component-storm live in the dashboard; workspace coverage ≥98%; the Wasm-free
invariant holds (no `wasmtime` under `rusm-otp`).

**Deferred follow-ons:** the wasip1 bridge's full WASI + raw `rusm::*` actor ABI,
p3 cross-component `stream<u8>`, and `rusm dev` filesystem watch/reload.

## Next

[Phase 8](../02-roadmap.md): the **`rusm-rs` guest crate** — ergonomic
spawn/Mailbox/AbstractProcess/Supervisor over the raw ABI, so guests write idiomatic
code instead of hand-rolled bindings.
