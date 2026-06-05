# Concept — hooking into a running node

One of the BEAM's best tricks: connect to an *already-running* node and inspect or
poke it live — `iex --remsh`, or `:observer` attached to production. RUSM brings
this to WebAssembly.

## What it is

A node exposes a secure **control channel**. A client that attaches can:

- list processes and see their status, mailbox depth, memory, reductions;
- send a message to a process, or spawn one;
- run/stop benchmark scenarios and toggle observer detail;
- (later) hot-reload a module.

Two clients speak this channel: the dashboard's **observer view** (GUI) and the
**`rusm attach <node>`** REPL (terminal).

## This is new — Rust doesn't give it to us

The BEAM bakes remote shell + distribution + observer into the VM. Rust has no
runtime VM, no built-in process/node model, and no live introspection, so RUSM
builds this itself on the control channel. The closest prior art,
[`tokio-console`](https://github.com/tokio-rs/console), is read-mostly
diagnostics over a gRPC endpoint — useful inspiration, but not a process-aware
REPL you can spawn and message through.

## Phase 0 today

The plumbing already exists in miniature: `rusm node start` serves a
control/observer channel over WebSocket, and both the dashboard and `rusm attach`
connect to it. The processes are synthetic for now; the transport, the protocol,
and the two clients are real.

> Full cross-node attach lands in Phase 9; the local channel exists today.
