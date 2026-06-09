# Concept — serving HTTP / WS / SSE (per-request vs resident)

A RUSM component can be a high-throughput **HTTP / WebSocket / SSE** server. The
host owns the socket and the protocol; the guest just produces responses — written
in Rust or [TypeScript](./guests-rust-and-typescript.md). `rusm-otp` stays
Wasm-free; all the serving machinery (hyper, tungstenite, `wasi:http`) lives only in
`rusm-wasm`.

## Two deployment shapes

- **Per-request / per-connection** — a fresh, sandboxed instance per HTTP request
  (or per WS connection). Maximum isolation: a trap fails *just that* request or
  socket, never the listener or the other connections. Cheap on the pooled spawn
  path.
- **Resident (singleton / pool)** — a warm, supervised pool of long-lived instances
  that **hold state across requests** (a counter, a cache, a session map) and skip
  per-request instantiation — the "real server" deployment. Per-instance restart
  isolation: one crash-looping slot never takes its siblings down.

You choose per `rusm.toml [[serve]]`: `mode = "per-request" | "resident"`, plus
`instances`, `shard-by` (header affinity) and `max-inflight` (shed to `503`).

## How it works

- **HTTP / SSE** ride `wasi:http`. SSE streams `data:` frames incrementally as the
  guest yields them, back-pressured by the body — see [byte streams](./byte-streams.md).
- **WebSocket** upgrades host-side; each inbound frame becomes a mailbox message,
  and replies go out through a Wasm-free **writer process** that owns the socket
  sink (one isolated process per connection, or a routed resident pool).
- **Standards-first:** a stock `wasi:http` component serves unchanged; the
  `rusm:runtime` actor world is opt-in.

## How it's benchmarked (honestly)

Serving throughput is measured **out-of-process** by the `rusm-loadtest` binary
against a real `rusm serve` port, so the load generator never steals the server's
CPU and the number is the server's — see the
[benchmark reference](../03-benchmark-dashboard.md).

> Phase 11. `rusm serve` hosts `rusm.toml [[serve]]` entries on real ports; serving
> TLS is planned for Phase 12.
