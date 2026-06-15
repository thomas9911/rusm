# rusm-logfmt

> The shared log palette and line format for RUSM — one source of truth for every `rusm`-tagged log line, on the host and in `wasm32-wasip2` guests.

`rusm-logfmt` defines how a RUSM log line looks: the severity colours and the formatting of
the timestamp, `component#pid`, and message. It compiles for **both** the host and
`wasm32-wasip2`, so a guest's `console.*` (TS) / `log` crate (Rust) and the host's lifecycle
and access logs all render identically — logging is a platform primitive, not per-app wiring.

`platform_line` is the single function every `rusm`-tagged line (process lifecycle + the
serving access log) routes through, so the format can never drift between subsystems.

Part of [RUSM](https://github.com/archan937/rusm). See the
[repo README](https://github.com/archan937/rusm#readme).
