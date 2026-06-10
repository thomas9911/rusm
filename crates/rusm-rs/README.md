# rusm-rs — the Rust guest crate for RUSM

Write a [RUSM](https://github.com/archan937/rusm) **component** in Rust — a sandboxed,
supervised WASM process on an Erlang-style actor runtime. `rusm-rs` wraps the raw
`wit-bindgen` actor bindings into a small, idiomatic API (`Pid`, `send`/`receive`,
`spawn`, the registry, `Stream`), adds HTTP/WS/SSE serving handlers, and hides the
component boilerplate behind `#[rusm_rs::main]`. The TypeScript twin is the
[`rusm`](https://www.npmjs.com/package/rusm) npm package — they share one JSON wire and
interoperate.

Built for `wasm32-wasip2`. A component crate is a `cdylib` depending on `rusm-rs` and
`wit-bindgen`:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
rusm-rs = "0.1"
wit-bindgen = "0.46"
```

The fastest start is `rusm new <name> --rust` (scaffolds this for you).

## A component is just your logic

`#[rusm_rs::main]` generates the `process` world, the `Guest` impl, and `export!` — so
there's **no `wit/` dir and no `wit-bindgen` boilerplate** in your source:

```rust
use rusm_rs::{receive_bytes, send_bytes, set_label, Pid};

#[rusm_rs::main]
fn main() {
    let reply_to: u64 = String::from_utf8(receive_bytes()).unwrap().parse().unwrap();
    set_label("worker");
    send_bytes(Pid(reply_to), b"pong");
}
```

## Serving HTTP / WS / SSE

Implement a handler and `serve` it — the host turns requests/connections into calls:

```rust
use rusm_rs::http::{Handler, Request, Response};

#[derive(Default)]
struct Api {
    hits: u64,
}

impl Handler for Api {
    fn handle(&mut self, _req: Request) -> Response {
        self.hits += 1;
        Response::text(format!("hit #{}\n", self.hits))
    }
}

#[rusm_rs::main]
fn main() {
    rusm_rs::http::serve(Api::default());
}
```

`rusm_rs::ws::Handler` (`open`/`message`/`close`, reply with `ws::send(conn, …)`) serves
WebSockets; `rusm_rs::http::serve_sse(|req| …)` streams Server-Sent Events.

## Services & the typed client

`#[rusm_rs::service]` on a module of free functions (no `impl`, no `self`) generates a
`serve()` dispatch loop and a typed, blocking `Client`:

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

The same JSON wire as the `rusm` TS package, so a Rust client and a TS service
interoperate.

## What you get

`Pid`, `send`/`receive` (raw bytes and serde-typed JSON), `spawn`, the named registry
(`register`/`whereis`/`unregister`), `set_label`, `is_alive`, `kill`, `list`, a
back-pressured `Stream`, an in-guest `Supervisor`, `#[rusm_rs::main]`,
`#[rusm_rs::service]`, and the `http`/`ws` serving handlers — all over the `rusm:runtime`
actor world. See the [RUSM docs](https://archan937.github.io/rusm/) for the full guide.
