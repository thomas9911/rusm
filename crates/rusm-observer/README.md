# rusm-observer

> Live process introspection for RUSM — point-in-time snapshots of a running node for the dashboard and `rusm attach`.

`rusm-observer` turns a running RUSM node into something you can watch. It assembles
snapshots of the live system — processes (pids, labels, links, monitors, mailbox depth),
per-scheduler load, memory, and the [`rusm-metrics`](https://crates.io/crates/rusm-metrics)
signals — in the shape the React dashboard and a `rusm attach` REPL consume.

It's the BEAM-style "observer" experience: see what's running, on local or remote nodes,
without instrumenting your app.

Part of [RUSM](https://github.com/archan937/rusm). See the
[repo README](https://github.com/archan937/rusm#readme).
