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

## Where it stands (Phase 9)

Two layers are real:

- **Local channel (Phase 0+):** `rusm node start` serves a control/observer channel
  over WebSocket; the dashboard and `rusm attach` connect to it to run scenarios,
  toggle observer detail, and watch live process samples.
- **Cross-node primitive (Phase 9):** over the cluster transport, a node's
  control-plane RPC answers `remote_pids(node)` — list the processes alive on a
  *peer*. That's the building block behind attaching to a remote node; richer
  remote introspection (per-process info, message/spawn-through) extends the same
  RPC.

> The transport, control channel, and remote listing are real; the full remote
> `rusm attach <node>` REPL surface builds on this primitive.
