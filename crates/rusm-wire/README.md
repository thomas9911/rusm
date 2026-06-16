# rusm-wire

> The shared HTTP request/response wire types for RUSM — one definition the guest SDK and the host both use, so they can never drift.

`rusm-wire` holds the serde types for HTTP requests and responses (headers, status, and
base64-encoded bodies) exchanged between a RUSM guest and the host. The Rust guest SDK
([`rusm-rs`](https://crates.io/crates/rusm-rs)) and the Wasmtime host
([`rusm-wasm`](https://crates.io/crates/rusm-wasm)) **both** depend on this crate, so the
serving contract is defined once and the two sides stay in lock-step by construction.

It is a small, dependency-light crate (serde + base64) with no runtime logic — just the
shared shapes.

Part of [RUSM](https://github.com/archan937/rusm). See the
[repo README](https://github.com/archan937/rusm#readme).
