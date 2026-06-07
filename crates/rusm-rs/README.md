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

## Services & the typed client

`#[rusm_rs::service]` on a module of free functions (mirrors a TS service's
`export function`s — no `impl`, no `self`) generates a `serve()` dispatch loop and
a typed, blocking `Client`:

```rust
#[rusm_rs::service]
pub mod calc {
    pub fn add(a: i64, b: i64) -> i64 { a + b }
    pub fn count_to(n: i64) -> impl Iterator<Item = i64> { 1..=n }   // streaming
    pub fn work(progress: rusm_rs::Callback<i64>) -> String {        // callback
        for pct in [25, 50, 100] { progress.call(pct); }
        "done".into()
    }
}
```

```rust
let calc = calc::Client::spawn("calc")?;          // spawn-from-guest by name
let sum = calc.add(2, 3)?;                         // a call
for n in calc.count_to(3) { /* 1, 2, 3 */ }       // a stream
let status = calc.work(|pct| println!("{pct}"))?; // a callback (closure stays here)
```

A guest component wires it with the usual `generate!`/`export!`, running
`calc::serve()` from `run`. The same JSON wire as rusm-ts, so a Rust client and a
TS service interoperate.

## Status (Phase 8)

- ✅ **Foundation** — `Pid`, `send`/`receive`(_bytes), serde-typed `send`/`receive`,
  `spawn`, `register`/`whereis`/`unregister`, `set_label`, `is_alive`, `kill`,
  `list`, `Stream`; the library/binary split.
- ✅ **`#[rusm_rs::service]`** — dispatch loop + typed `Client`: **call**, **cast**,
  **streaming** (`impl Iterator` → a client iterator), and **callbacks**
  (`Callback<T>` → a client closure). Over the rusm-ts JSON wire.

## Regenerating the fixture wasm

```sh
cd ../rusm-wasm/tests/fixtures/rs-guest
cargo build --target wasm32-wasip2 --release
cp target/wasm32-wasip2/release/rs_guest.wasm ../rs_guest.wasm
```
