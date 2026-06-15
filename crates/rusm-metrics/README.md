# rusm-metrics

> Lightweight runtime metrics for RUSM — the counters and gauges the observer and dashboard read.

`rusm-metrics` collects the runtime signals RUSM exposes for observability: throughput
counters, live process counts, scheduler load, memory, and latency samples. It's the data
source the [`rusm-observer`](https://crates.io/crates/rusm-observer) snapshot and the live
benchmark dashboard render, and what a `rusm attach` session surfaces.

Small and allocation-conscious so collection never perturbs the workload it measures.

Part of [RUSM](https://github.com/archan937/rusm). See the
[repo README](https://github.com/archan937/rusm#readme).
