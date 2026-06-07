# `ws_bench` — WebSocket stress benchmark

Holds many concurrent WebSocket connections against RUSM's host echo and hammers
each with echo round-trips — the **stability under load** proof (each connection is
an isolated supervised task; one dropping never touches the others or the listener).

```sh
cargo run --release -p rusm-bench --example ws_bench -- [seconds] [connections]
```

Representative output (loopback):

```
WebSocket stress: 256 concurrent connections, 5s

connections held: 256/256   (each an isolated supervised task)
echo round-trips:  190900/sec   p50 1284.3µs  p99 2744.4µs
```

## Reading it

- **`connections held: 256/256`** is the headline — no flakiness, no cascading
  drops. RUSM has no lattice or broker between the socket and the handler (unlike a
  NATS-based host), so there's far less to wedge.
- The upgrade + WebSocket protocol run host-side (hyper + `tokio-tungstenite`); each
  connection is its own task. Bridging a connection to a WASM **component process**
  (each WS message ↔ the process mailbox) is the next slice — then "supervised task"
  becomes "supervised RUSM process," with `monitor`/restart on top.
- `p50`/`p99` are round-trip latency under N-way concurrency; throughput scales with
  free CPU and connection count.
