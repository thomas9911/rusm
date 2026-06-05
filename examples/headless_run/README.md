# headless_run

The smallest taste of the harness: drive the benchmark **runner** directly — no
node, no network, no browser — and print a handful of sampled ticks.

## What it shows

- Creating a `Runner` with the default config.
- Starting a scenario (`connection-storm`) and calling `tick()` on a loop.
- Each `tick` returns a `Frame` (throughput, peak concurrency, latency
  percentiles, observer snapshot); `summarize_frame` renders it as one line.

This is the same `Runner` the WebSocket server and the terminal `rusm-bench run`
command use — here with synthetic data.

## Run

```sh
cargo run -p rusm-bench --example headless_run
```

## Expected output

```
running `connection-storm` for 10 ticks:

[connection-storm]       324559 ops/s  peak    4552  p50     284µs  p99     498µs  procs 64
...
```
(numbers vary per tick but stay within the scenario's synthetic ranges.)
