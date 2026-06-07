//! The **rusm-ts js-runner**: a component that embeds rquickjs (QuickJS) and runs a
//! JavaScript bundle, exposing a `Process` global bridged to the `rusm:runtime`
//! actor world. A TypeScript app is just a Bun-bundled `.js` — the runner is one
//! shared, sandboxed, capability-gated wasm process per JS instance.
//!
//! Protocol: the runner's **first** message is the JS bundle (UTF-8 source);
//! everything after is the app's own mailbox, read via `Process.receive()`.
//!
//! The JS↔actor bridge is sync from JS's view: `Process.receive()` calls the host
//! `receive`, which suspends the whole instance's fiber until a message arrives —
//! so blocking JS code "just works" without async, exactly like a Rust guest.

wit_bindgen::generate!({
    world: "process",
    path: "wit",
});

use rusm::runtime::actor;

struct Component;

/// The `Process` API surface, defined in JS over the host primitives below.
const PRELUDE: &str = r#"
globalThis.Process = {
  self()        { return BigInt(__own_pid()); },
  list()        { return __list().map(BigInt); },
  send(to, msg) { __send(String(to), msg); },
  receive()     { return __receive(); },
  register(n)   { return __register(n); },
  whereis(n)    { const p = __whereis(n); return p === "" ? null : BigInt(p); },
  isAlive(p)    { return __is_alive(String(p)); },
  kill(p)       { return __kill(String(p)); },
  setLabel(l)   { __set_label(l); },
};
"#;

impl Guest for Component {
    fn run() {
        // First message is the JS bundle to run.
        let bundle = String::from_utf8(actor::receive()).unwrap_or_default();

        let rt = rquickjs::Runtime::new().unwrap();
        let ctx = rquickjs::Context::full(&rt).unwrap();
        ctx.with(|ctx| {
            let g = ctx.globals();
            // Pids cross as decimal strings (a u64 doesn't fit a JS number); JS wraps
            // them in BigInt. Messages cross as UTF-8 strings for the bridge.
            g.set(
                "__own_pid",
                rquickjs::Function::new(ctx.clone(), || actor::own_pid().to_string()).unwrap(),
            )
            .unwrap();
            g.set(
                "__list",
                rquickjs::Function::new(ctx.clone(), || {
                    actor::list_processes()
                        .into_iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap(),
            )
            .unwrap();
            g.set(
                "__send",
                rquickjs::Function::new(ctx.clone(), |to: String, msg: String| {
                    actor::send(to.parse().unwrap_or(0), msg.as_bytes());
                })
                .unwrap(),
            )
            .unwrap();
            g.set(
                "__receive",
                rquickjs::Function::new(ctx.clone(), || {
                    String::from_utf8(actor::receive()).unwrap_or_default()
                })
                .unwrap(),
            )
            .unwrap();
            g.set(
                "__register",
                rquickjs::Function::new(ctx.clone(), |name: String| actor::register(&name)).unwrap(),
            )
            .unwrap();
            g.set(
                "__whereis",
                rquickjs::Function::new(ctx.clone(), |name: String| {
                    actor::whereis(&name).map(|p| p.to_string()).unwrap_or_default()
                })
                .unwrap(),
            )
            .unwrap();
            g.set(
                "__is_alive",
                rquickjs::Function::new(ctx.clone(), |p: String| {
                    actor::is_alive(p.parse().unwrap_or(0))
                })
                .unwrap(),
            )
            .unwrap();
            g.set(
                "__kill",
                rquickjs::Function::new(ctx.clone(), |p: String| actor::kill(p.parse().unwrap_or(0)))
                    .unwrap(),
            )
            .unwrap();
            g.set(
                "__set_label",
                rquickjs::Function::new(ctx.clone(), |l: String| actor::set_label(&l)).unwrap(),
            )
            .unwrap();

            let _: () = ctx.eval(PRELUDE).unwrap();
            let _: () = ctx.eval(bundle).unwrap();
        });
    }
}

export!(Component);
