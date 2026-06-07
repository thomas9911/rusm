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

/// The guest JS environment, split by concern (see `bridge/`): Web API polyfills
/// (standards-only) then the `Process`/`Stream` actor API (over the host `__*`
/// primitives). Both are evaluated before the user's bundle.
const WEBAPI_JS: &str = include_str!("../bridge/webapi.js");
const PROCESS_JS: &str = include_str!("../bridge/process.js");

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
            // console output → WASI stderr (shown only if the `inherit_stdio`
            // capability is granted; discarded for a sandboxed guest).
            def!("__print", |s: String| eprintln!("{s}"));

            // Web API polyfills, then the actor API, then the user's bundle.
            let _: () = ctx.eval(WEBAPI_JS).unwrap();
            let _: () = ctx.eval(PROCESS_JS).unwrap();
            let _: () = ctx.eval(bundle).unwrap();
        });
    }
}

export!(Component);
