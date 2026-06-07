# RUSM examples

Small, ready-to-run programs — each demonstrates one capability. Spawn-storm,
ping-pong, and fault-recovery run **real** `rusm-otp` processes; the remaining
scenarios are synthetic until their phase lands — the same code keeps working as
each part of the runtime arrives.

## Runnable example apps

Each example is its own directory with a `main.rs` and a `README.md` (what it
shows, how to run it, and the output to expect). Run any from the repo root:

| Example | Shows | Command |
| --- | --- | --- |
| [`host_components`](./host_components/) | **Host real WASM components** as isolated, introspectable, capability-sandboxed processes (Phase 7) — the heart of RUSM. | `cargo run -p rusm-bench --example host_components` |
| [`cluster`](./cluster/) | **A two-node cluster** (Phase 9): processes message across nodes over QUIC+TLS, a global registry hides location, and live attach lists a peer's processes. | `cargo run -p rusm-bench --example cluster` |
| [`cluster_fanout`](./cluster_fanout/) | Benchmark the cross-node transport: unloaded round-trip latency + saturation throughput. | `cargo run --release -p rusm-bench --example cluster_fanout` |
| [`http_bench`](./http_bench/) | **Serve a WASM component as HTTP** (Phase 11) and stress it vs a bare-hyper baseline — req/s + p50/p99, sandbox overhead. | `cargo run --release -p rusm-bench --example http_bench` |
| [`headless_run`](./headless_run/) | Drive the benchmark runner directly (no network) and print sampled ticks. | `cargo run -p rusm-bench --example headless_run` |
| [`synthetic_source`](./synthetic_source/) | The deterministic synthetic data source — reproducible per `(scenario, tick)`. | `cargo run -p rusm-bench --example synthetic_source` |
| [`observer_overhead`](./observer_overhead/) | The observer's detail on/off switch (basis of the overhead proof). | `cargo run -p rusm-bench --example observer_overhead` |
| [`embedded_node`](./embedded_node/) | Embed a node and serve the live protocol for the dashboard / REPL. | `cargo run -p rusm-bench --example embedded_node` |

## End-to-end recipes

Start a node, then watch it from the dashboard and/or a REPL:

```sh
# 1. Start a node (or run the embedded_node example above)
cargo run -p rusm-cli -- node start            # ws://127.0.0.1:4000

# 2a. The dashboard
cd bench/dashboard && bun install && bun run dev

# 2b. …or a live REPL (like `iex --remsh`); no URL needed for the local node
cargo run -p rusm-cli -- attach
#   run connection-storm
#   detail off
#   stop
#   quit

# Or run a scenario straight in the terminal, no node:
cargo run -p rusm-bench -- run connection-storm 5
```
