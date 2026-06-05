# Distributed model & live attach (planned — phase 9)

RUSM chases the two BEAM superpowers that matter most.

## Nodes connecting to nodes

Independent RUSM nodes (separate OS processes/machines) discover and connect —
like `Node.connect/1` + epmd — over **QUIC with TLS**. Across the cluster:

- **Transparent remote spawn** — start a process on another node.
- **Cross-node message passing** — `send` works regardless of where a process lives.
- **Distributed registry** — a `:global`-style name → pid map spanning nodes.

An `ex_united`-style test harness boots N nodes and connects them in-process, so
the whole distributed model is TDD-able. (`ex_united` is a hex package the author
wrote for exactly this in Elixir.)

## Hooking into a running node

Attach to an *already-running* node and inspect/manipulate it live — list
processes, peek mailboxes and state, send messages, spawn — over a secure
**control channel**. Two clients:

- The dashboard's **remote observer** (GUI).
- **`rusm attach <node>`** — an interactive REPL, exactly like `iex --remsh`.

> This is a **new RUSM capability**, not something Rust provides. The BEAM bakes
> remote shell + distribution + observer into the VM; Rust has no runtime VM, no
> built-in process/node model, and no live introspection. Closest prior art:
> [`tokio-console`](https://github.com/tokio-rs/console) (live task view) — but
> that's read-mostly diagnostics, not a process-model-aware REPL.

## Phase 0 today

The plumbing exists in miniature: a node (`rusm node start`) serves a
control/observer channel over WebSocket, and both the dashboard and
`rusm attach` connect to it. In Phase 0 the processes are synthetic; the
transport and client model are real. See
[live attach](./concepts/live-attach.md).
