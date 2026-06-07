# Concept — per-process permissions & sandboxing

Because every process is an isolated Wasm instance, RUSM can grant each one a
**precise, individual** set of capabilities — finer-grained than Go or Erlang,
where any goroutine/process can touch the whole machine.

## What can be scoped per process

- **Filesystem**: which directories (if any) the process may open — via WASI
  preopens. A process with no preopens cannot touch the disk at all.
- **Network**: whether outbound/inbound sockets are allowed.
- **Memory**: a hard ceiling via Wasmtime store limits; exceeding it traps *that*
  process only (see [links & supervision](./links-and-supervision.md)).
- **Host functions**: only the `rusm::*` imports the process was linked with are
  callable.

## Default-deny profiles

A process gets **nothing** unless granted. Named profiles (`caps.rs`) bundle
sensible defaults, and a per-spawn `Capabilities` builder overrides them:

- **`Sandboxed`** — CPU + a bounded heap only: no fs, net, env, or stdio.
- **`NetworkClient`** — sandboxed plus outbound network.
- **`Trusted`** — inherits stdio, allows network, a large heap.

Grants map onto **standard WASI** (`wasi:cli/environment`, `wasi:filesystem`,
`wasi:sockets`) plus a `StoreLimiter` memory cap — no wasmCloud-style
`wasi:config/store`.

## Why it matters

You can run untrusted or risky code — even a C library compiled to Wasm with a
latent bug — knowing a crash or exploit is confined to that one process and its
granted resources. This is the security half of "isolation"; fault tolerance is
the other half.

## The test that proves it

Tests cover a memory-limit breach trapping a single process (both the component
and core-module paths) without disturbing the rest, and an unbuildable grant
(a preopen of a missing path) crashing only that process before it runs.

> Shipped in Phase 7.
