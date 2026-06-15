# rusm-rs-macros

> The procedural macros behind the [`rusm-rs`](https://crates.io/crates/rusm-rs) Rust guest SDK.

`rusm-rs-macros` provides the attribute macros that make writing a RUSM component in Rust
ergonomic. You don't depend on this crate directly — it's re-exported through
[`rusm-rs`](https://crates.io/crates/rusm-rs).

- **`#[rusm_rs::service]`** — turn a `mod` of functions into a service: generates the
  receive → dispatch → reply loop plus a typed `Client` with call / cast / streaming /
  callbacks (the same JSON wire TS guests speak, so Rust and TS interoperate).
- **`#[rusm_rs::main]`** — the worker entry point.
- **`#[rusm_rs::handlers]`** — named HTTP "action" handlers for the routed serving path.

Part of [RUSM](https://github.com/archan937/rusm). See the
[repo README](https://github.com/archan937/rusm#readme) and
[`rusm-rs`](https://github.com/archan937/rusm/tree/main/crates/rusm-rs).
