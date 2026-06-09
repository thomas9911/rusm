# Concept — process management (registry, timers, introspection)

Beyond spawn / send / receive, a RUSM process has the full Erlang **management**
toolkit — all backed by the Wasm-free `rusm-otp` core, so it's there whether a
process body is native Rust or a sandboxed Wasm instance.

## Named registry

Register a process under a name so others reach it without holding its pid:
`register(name)` / `whereis(name)`. The registry is a sharded `DashMap` (no global
lock), so lookups scale with cores. For names that resolve across machines, see
[distributed nodes](./distributed-nodes.md).

## Timers

`send_after(ms, msg)` schedules a message to land in a mailbox later; `cancel`
aborts a pending one. It's built on Tokio's timer wheel, so millions of outstanding
timers cost almost nothing.

## Introspection

`list()` enumerates live pids; `info(pid)` returns a process's links, monitors,
names, label, mailbox depth and status; `set_label(..)` tags a process for the
observer. This is exactly what the [live-attach](./live-attach.md) REPL and the
dashboard observer read — nothing is hidden from you.

## Lifecycle: graceful shutdown vs kill

`shutdown` drains and stops a process in order; `kill` aborts it immediately via a
`futures` abort handle (no second signal channel — kill rides that handle, exit
signals ride the mailbox). Crashes and exits flow through
[links & supervision](./links-and-supervision.md).

> Registry, timers and graceful shutdown shipped in Phase 4; introspection +
> labels + mailbox depth surface in the observer snapshot.
