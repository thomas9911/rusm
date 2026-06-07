# rusm-rs — the Rust guest crate

Write a RUSM **component** (or **service**) in Rust over the `rusm:runtime` actor
world — the Rust twin of [rusm-ts](../rusm-wasm/js-runner). It wraps the raw
`wit-bindgen` bindings into a small, idiomatic API: `Pid`, `send`/`receive`
(serde-typed JSON, the wire shared with TS guests), `spawn`, the registry, and
`Stream`. Built for `wasm32-wasip2`.

## Writing a guest

A guest crate depends on `rusm-rs`, generates the `process` world **mapping the
actor import to rusm-rs's bindings** (so the interface is imported exactly once —
the wit-bindgen library/binary split), and `export!`s its component:

```rust
wit_bindgen::generate!({
    world: "process",
    path: "wit",                                  // a vendored copy of the world
    with: { "rusm:runtime/actor@0.1.0": rusm_rs::rusm::runtime::actor },
});

struct Component;
impl Guest for Component {
    fn run() {
        let reply_to: u64 = String::from_utf8(rusm_rs::receive_bytes())
            .unwrap().parse().unwrap();
        rusm_rs::set_label("worker");
        rusm_rs::send_bytes(rusm_rs::Pid(reply_to), b"pong");
    }
}
export!(Component);
```

(The `rs-guest` test fixture under `rusm-wasm/tests/fixtures/` is exactly this.)

## Status (Phase 8)

- ✅ **Foundation** — `Pid`, `send`/`receive`(_bytes), serde-typed `send`/`receive`,
  `spawn`, `register`/`whereis`/`unregister`, `set_label`, `is_alive`, `kill`,
  `list`, `Stream`; the library/binary split.
- 🔨 **`#[rusm::service]`** — a proc-macro over a `mod` of free functions that
  generates the dispatch loop + a typed client (`spawn::<calc::Client>("calc")`),
  with streaming + callbacks over the same JSON wire as rusm-ts. *(Not yet built.)*

## Regenerating the fixture wasm

```sh
cd ../rusm-wasm/tests/fixtures/rs-guest
cargo build --target wasm32-wasip2 --release
cp target/wasm32-wasip2/release/rs_guest.wasm ../rs_guest.wasm
```
