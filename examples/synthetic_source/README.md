# synthetic_source

Show that the Phase 0 **synthetic data source** is a pure function of
`(scenario, tick)`: lively and scenario-shaped, yet perfectly reproducible. This
determinism is what keeps the dashboard demoable and every test stable before the
real runtime exists.

## What it shows

- Constructing a `SyntheticSource` for a scenario.
- Calling `tick(index, latency_samples, max_processes, scheduler_count)` to get a
  `SyntheticTick` (ops/sec, peak concurrency, latency samples, a process sample).
- That re-running the **same** tick yields byte-identical output.

## Run

```sh
cargo run -p rusm-bench --example synthetic_source
```

## Expected output

```
tick 0:    258456 ops/s, peak  27597, 4 latency samples
tick 1:    292953 ops/s, peak  17517, 4 latency samples
tick 2:    297454 ops/s, peak  38319, 4 latency samples
tick 3:    328098 ops/s, peak  44179, 4 latency samples
tick 4:    260541 ops/s, peak  19954, 4 latency samples

re-running tick 0 produced byte-identical data ✓
```
