# Concept — a process is a Wasm instance

In RUSM a *process* is a single WebAssembly instance running as a Tokio task. The
instance owns its linear memory (heap), its stack, and its set of permitted host
functions (syscalls). Nothing is shared with other processes.

## Why this is the right unit

- **Isolation = fault tolerance.** A trap (panic, out-of-bounds, `unreachable`)
  tears down only that instance. The host catches it and turns it into a process
  exit; linked processes and supervisors react (see
  [links & supervision](./links-and-supervision.md)).
- **Isolation = security.** Each instance only has the host functions and
  resources it was granted (see [permissions & sandboxing](./permissions-and-sandboxing.md)).
- **Cheap.** A fresh instance is small and fast to create — the basis for the
  300k spawns/sec goal. Memory is bounded per process via Wasmtime store limits.

## How it maps to Tokio

`spawn(module)` instantiates the module and drives its entry function inside a
Tokio task. Because host calls are async (see
[fibers & blocking→async](./fibers-and-blocking-to-async.md)), a process that
"blocks" simply parks its task, freeing the worker thread for other processes.

> The process *abstraction* (task + mailbox + signal loop) is built on native
> Rust bodies in Phase 1. A process becomes a real isolated **Wasm instance** when
> the Wasmtime backend is slotted in at Phase 6 — the actor layer above is unchanged.
