//! The **rusm-ts js-runner**: a component that embeds rquickjs (QuickJS) and runs a
//! JavaScript bundle, exposing a `Process` global (and `Stream`) bridged to the
//! `rusm:runtime` actor world. A TypeScript app is just a Bun-bundled `.js` — the
//! runner is one shared, sandboxed, capability-gated wasm process per JS instance.
//!
//! Protocol: the runner's **first** message is the JS bundle (UTF-8 source);
//! everything after is the app's own mailbox, read via `Process.receive()`.
//!
//! Bridge: messages and stream chunks cross as **`Uint8Array`** (the actor model's
//! currency), with text convenience helpers (UTF-8 done in Rust, since QuickJS has
//! no `TextEncoder`). Pids cross as decimal strings (a `u64` doesn't fit a JS
//! number) and JS wraps them in `BigInt`; stream handles are small ints.
//!
//! Blocking JS "just works": `Process.receive()` / `stream.read()` call the host,
//! which suspends the whole instance's fiber until data arrives — no async needed.

wit_bindgen::generate!({
    world: "process",
    path: "wit",
});

use rusm::runtime::actor;
use rquickjs::{Ctx, Function, TypedArray};

struct Component;

// The `TypedArray`-taking/returning bridges are named generic fns (not closures):
// rquickjs needs `for<'js>` HRTB on the `Ctx`/`TypedArray` lifetime, which inline
// closures don't infer ("lifetime may not live long enough").
fn js_send(to: String, data: TypedArray<u8>) {
    actor::send(to.parse().unwrap_or(0), data.as_bytes().unwrap_or(&[]));
}
fn js_receive(ctx: Ctx<'_>) -> rquickjs::Result<TypedArray<'_, u8>> {
    TypedArray::new(ctx, actor::receive())
}
fn js_stream_write(h: f64, data: TypedArray<u8>) -> bool {
    actor::stream_write(h as u64, data.as_bytes().unwrap_or(&[]))
}
fn js_stream_read(ctx: Ctx<'_>, h: f64) -> Option<TypedArray<'_, u8>> {
    actor::stream_read(h as u64).map(|b| TypedArray::new(ctx, b).unwrap())
}

/// The `Process` + `Stream` API, defined in JS over the host primitives below.
const PRELUDE: &str = r#"
class Stream {
  constructor(h) { this.handle = h; }
  write(chunk) {
    if (typeof chunk === "string") return __stream_write_text(this.handle, chunk);
    return __stream_write(this.handle, chunk);          // Uint8Array
  }
  close() { __stream_close(this.handle); }
  // Normalise the host's `undefined` (Rust None) to `null` so `read() !== null`
  // works for end-of-stream.
  read() { const c = __stream_read(this.handle); return c === undefined ? null : c; }  // Uint8Array | null (EOF)
}
globalThis.Process = {
  self()        { return BigInt(__own_pid()); },
  list()        { return __list().map(BigInt); },
  send(to, msg) {
    if (typeof msg === "string") __send_text(String(to), msg);
    else __send(String(to), msg);                       // Uint8Array
  },
  receive()     { return __receive(); },                // Uint8Array
  receiveText() { return __receive_text(); },           // string
  register(n)   { return __register(n); },
  whereis(n)    { const p = __whereis(n); return p === "" ? null : BigInt(p); },
  isAlive(p)    { return __is_alive(String(p)); },
  kill(p)       { return __kill(String(p)); },
  setLabel(l)   { __set_label(l); },
  openStream(to){ const h = __stream_open(String(to)); return h < 0 ? null : new Stream(h); },
  acceptStream(){ return new Stream(__stream_accept()); },
};
"#;

impl Guest for Component {
    fn run() {
        // First message is the JS bundle to run.
        let bundle = String::from_utf8(actor::receive()).unwrap_or_default();

        let rt = rquickjs::Runtime::new().unwrap();
        let context = rquickjs::Context::full(&rt).unwrap();
        context.with(|ctx| {
            let g = ctx.globals();
            // Each closure is its own type, so a macro (not a helper fn/closure) is
            // what lets us register them uniformly.
            macro_rules! def {
                ($name:expr, $func:expr) => {
                    g.set($name, Function::new(ctx.clone(), $func).unwrap())
                        .unwrap();
                };
            }

            // --- process / messaging ---
            def!("__own_pid", || actor::own_pid().to_string());
            def!("__list", || actor::list_processes()
                .into_iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>());
            def!("__send_text", |to: String, s: String| actor::send(
                to.parse().unwrap_or(0),
                s.as_bytes()
            ));
            def!("__send", js_send);
            def!("__receive", js_receive);
            def!("__receive_text", || String::from_utf8(actor::receive())
                .unwrap_or_default());
            def!("__register", |n: String| actor::register(&n));
            def!("__whereis", |n: String| actor::whereis(&n)
                .map(|p| p.to_string())
                .unwrap_or_default());
            def!("__is_alive", |p: String| actor::is_alive(
                p.parse().unwrap_or(0)
            ));
            def!("__kill", |p: String| actor::kill(p.parse().unwrap_or(0)));
            def!("__set_label", |l: String| actor::set_label(&l));

            // --- streams (handles are small ints carried as JS numbers) ---
            def!("__stream_open", |to: String| actor::stream_open(
                to.parse().unwrap_or(0)
            )
            .map_or(-1.0, |h| h as f64));
            def!("__stream_write", js_stream_write);
            def!("__stream_write_text", |h: f64, s: String| {
                actor::stream_write(h as u64, s.as_bytes())
            });
            def!("__stream_close", |h: f64| actor::stream_close(h as u64));
            def!("__stream_accept", || actor::stream_accept() as f64);
            def!("__stream_read", js_stream_read);

            let _: () = ctx.eval(PRELUDE).unwrap();
            let _: () = ctx.eval(bundle).unwrap();
        });
    }
}

export!(Component);
