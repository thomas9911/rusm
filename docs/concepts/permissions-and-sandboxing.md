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

## Why it matters

You can run untrusted or risky code — even a C library compiled to Wasm with a
latent bug — knowing a crash or exploit is confined to that one process and its
granted resources. This is the security half of "isolation"; fault tolerance is
the other half.

## The test that proves it

Phase 7 ships tests where an allowed-directory read succeeds, a denied one fails,
and a memory-limit breach traps a single process without disturbing the rest.

> Implemented in Phase 7.
