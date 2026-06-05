# observer_overhead

Demonstrate the observer's **detail on/off** switch — the basis of the
"observer-on vs observer-off" overhead proof. With detail off, aggregate counters
still update, but the costly per-instance table is skipped entirely. That's why
the live observer can stay on under load without distorting the benchmark.

## What it shows

- A running `Runner` produces an observer snapshot each tick.
- With detail **on**, the snapshot includes the per-instance process table.
- After `set_observer_detail(false)`, the table is empty while aggregates
  (`process_count`, counts, memory) remain accurate.

## Run

```sh
cargo run -p rusm-bench --example observer_overhead
```

## Expected output

```
detail ON : process_count=64      table_rows=64
detail OFF: process_count=64      table_rows=0  <- table suppressed, aggregates intact
```
