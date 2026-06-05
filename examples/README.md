# RUSM examples

Small, ready-to-run programs — each demonstrates one capability. In Phase 0 they
run against synthetic data; the same code keeps working as the real runtime lands.

## Runnable example apps

Each example is its own directory with a `main.rs` and a `README.md` (what it
shows, how to run it, and the output to expect). Run any from the repo root:

| Example | Shows | Command |
| --- | --- | --- |
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
