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

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use http_body_util::BodyExt;
use rquickjs::{Ctx, Exception, Function, Promise, TypedArray};
use rusm::runtime::actor;
use wstd::http::{Body, Client, Request};

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
fn js_receive_timeout(ctx: Ctx<'_>, ms: f64) -> rquickjs::Result<Option<TypedArray<'_, u8>>> {
    match actor::receive_timeout(ms.max(0.0) as u64) {
        Some(bytes) => Ok(Some(TypedArray::new(ctx, bytes)?)),
        None => Ok(None),
    }
}
fn js_stream_write(h: f64, data: TypedArray<u8>) -> bool {
    actor::stream_write(h as u64, data.as_bytes().unwrap_or(&[]))
}
fn js_stream_read(ctx: Ctx<'_>, h: f64) -> Option<TypedArray<'_, u8>> {
    actor::stream_read(h as u64).map(|b| TypedArray::new(ctx, b).unwrap())
}
// Cryptographically-secure random bytes from the host (wasi:random) — the basis for
// the `crypto.getRandomValues` / `randomUUID` polyfill the web ecosystem assumes.
fn js_random_bytes(ctx: Ctx<'_>, len: f64) -> rquickjs::Result<TypedArray<'_, u8>> {
    let mut buf = vec![0u8; len.max(0.0) as usize];
    getrandom::fill(&mut buf).expect("host entropy (wasi:random)");
    TypedArray::new(ctx, buf)
}

/// A `fetch` response body, streamed to the guest frame-by-frame. `Bytes`/`anyhow`
/// come via `wstd`; we never buffer the whole body, so a token-by-token LLM response
/// reaches the guest as it arrives.
type FetchBody = http_body_util::combinators::UnsyncBoxBody<bytes::Bytes, wstd::http::Error>;

thread_local! {
    /// Response bodies of in-flight `fetch`es, keyed by the handle handed to JS.
    static FETCH_BODIES: RefCell<HashMap<u64, FetchBody>> = RefCell::new(HashMap::new());
    static FETCH_NEXT: Cell<u64> = const { Cell::new(1) };
}

/// Perform an outbound HTTP request (capability-gated at the host — a sandboxed guest
/// is refused). Returns the response head as a flat list `[status, handle, k, v, …]`;
/// the body then streams via `__fetch_read(handle)`. A transport error (incl. a denied
/// request) throws into JS, so the `fetch()` promise rejects.
fn js_fetch(
    ctx: Ctx<'_>,
    method: String,
    url: String,
    headers: String,
    body: TypedArray<u8>,
) -> rquickjs::Result<Vec<String>> {
    let body = body.as_bytes().unwrap_or(&[]).to_vec();
    let sent = wstd::runtime::block_on(async move {
        let mut builder = Request::builder().method(method.as_str()).uri(url);
        for line in headers.split('\n').filter(|l| !l.is_empty()) {
            if let Some((k, v)) = line.split_once(':') {
                builder = builder.header(k.trim(), v.trim());
            }
        }
        let req = builder.body(Body::from(body)).map_err(|e| e.to_string())?;
        Client::new().send(req).await.map_err(|e| e.to_string())
    });
    let resp = sent.map_err(|e| Exception::throw_message(&ctx, &e))?;

    let handle = FETCH_NEXT.with(|c| {
        let h = c.get();
        c.set(h + 1);
        h
    });
    let mut head = vec![resp.status().as_u16().to_string(), handle.to_string()];
    for (name, value) in resp.headers() {
        head.push(name.as_str().to_owned());
        head.push(value.to_str().unwrap_or_default().to_owned());
    }
    FETCH_BODIES.with(|m| {
        m.borrow_mut()
            .insert(handle, resp.into_body().into_boxed_body())
    });
    Ok(head)
}

/// Read the next non-empty body chunk of a `fetch` response; `None` at end-of-stream
/// (the body is then dropped, releasing the connection). Each read suspends the fiber
/// until a frame arrives — natural back-pressure.
fn js_fetch_read(ctx: Ctx<'_>, handle: f64) -> rquickjs::Result<Option<TypedArray<'_, u8>>> {
    let key = handle as u64;
    let Some(mut body) = FETCH_BODIES.with(|m| m.borrow_mut().remove(&key)) else {
        return Ok(None);
    };
    let chunk = wstd::runtime::block_on(async {
        loop {
            match body.frame().await {
                Some(Ok(frame)) => match frame.into_data() {
                    Ok(data) if !data.is_empty() => return Some(data),
                    _ => continue, // empty data or trailers — keep reading
                },
                _ => return None, // end-of-stream or transport error
            }
        }
    });
    match chunk {
        Some(data) => {
            FETCH_BODIES.with(|m| m.borrow_mut().insert(key, body));
            Ok(Some(TypedArray::new(ctx, data.to_vec())?))
        }
        None => Ok(None),
    }
}

/// Abandon a `fetch` response body (abort / GC), releasing its connection.
fn js_fetch_close(handle: f64) {
    FETCH_BODIES.with(|m| {
        m.borrow_mut().remove(&(handle as u64));
    });
}

// Durable key-value storage over the `kv-*` actor ABI (gated by `storage`). A
// denied/failed op throws into JS (the `kv.js` bridge surfaces these as a thrown
// Error); `get` returns `undefined` (→ null in JS) when the key is absent.
fn js_kv_get(ctx: Ctx<'_>, bucket: String, key: String) -> rquickjs::Result<Option<TypedArray<'_, u8>>> {
    match actor::kv_get(&bucket, &key) {
        Ok(Some(bytes)) => Ok(Some(TypedArray::new(ctx, bytes)?)),
        Ok(None) => Ok(None),
        Err(e) => Err(Exception::throw_message(&ctx, &e)),
    }
}
fn js_kv_set(ctx: Ctx<'_>, bucket: String, key: String, value: TypedArray<u8>) -> rquickjs::Result<()> {
    actor::kv_set(&bucket, &key, value.as_bytes().unwrap_or(&[]))
        .map_err(|e| Exception::throw_message(&ctx, &e))
}
fn js_kv_delete(ctx: Ctx<'_>, bucket: String, key: String) -> rquickjs::Result<bool> {
    actor::kv_delete(&bucket, &key).map_err(|e| Exception::throw_message(&ctx, &e))
}
fn js_kv_exists(ctx: Ctx<'_>, bucket: String, key: String) -> rquickjs::Result<bool> {
    actor::kv_exists(&bucket, &key).map_err(|e| Exception::throw_message(&ctx, &e))
}
fn js_kv_list(ctx: Ctx<'_>, bucket: String) -> rquickjs::Result<Vec<String>> {
    actor::kv_list(&bucket).map_err(|e| Exception::throw_message(&ctx, &e))
}
// Spawn a registered component by name; a denied/unknown spawn throws into JS
// (surfacing the host's error message) rather than returning a sentinel.
fn js_spawn(ctx: Ctx<'_>, name: String) -> rquickjs::Result<String> {
    match actor::spawn(&name) {
        Ok(pid) => Ok(pid.to_string()),
        Err(e) => Err(Exception::throw_message(&ctx, &e)),
    }
}

/// The guest JS environment, split by concern (see `bridge/`): Web API polyfills
/// (standards-only) then the `Process`/`Stream` actor API (over the host `__*`
/// primitives). Both are evaluated before the user's bundle.
const WEBAPI_JS: &str = include_str!("../bridge/webapi.js");
const PROCESS_JS: &str = include_str!("../bridge/process.js");
const KV_JS: &str = include_str!("../bridge/kv.js");
const RPC_JS: &str = include_str!("../bridge/rpc.js");

impl Guest for Component {
    fn run() {
        // First message is the JS bundle: either raw JS source, or `rusm-jsc`
        // precompiled QuickJS bytecode (prefixed with the `QJSB` magic). Kept as raw
        // bytes so bytecode (not UTF-8) survives.
        let bundle_bytes = actor::receive();

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
            // `receive … after`: the next message, or `undefined` on timeout.
            def!("__receive_timeout", js_receive_timeout);
            def!("__register", |n: String| actor::register(&n));
            def!("__whereis", |n: String| actor::whereis(&n)
                .map(|p| p.to_string())
                .unwrap_or_default());
            def!("__is_alive", |p: String| actor::is_alive(
                p.parse().unwrap_or(0)
            ));
            def!("__kill", |p: String| actor::kill(p.parse().unwrap_or(0)));
            def!("__set_label", |l: String| actor::set_label(&l));
            def!("__spawn", js_spawn);
            def!("__monitor", |p: String| actor::monitor(p.parse().unwrap_or(0)));
            // Delegate guest `supervise` to the host's single native supervisor.
            def!(
                "__supervise",
                |strategy: String, children: Vec<String>, max_restarts: f64, within_ms: f64| {
                    let strategy = match strategy.as_str() {
                        "one_for_all" => actor::SuperviseStrategy::OneForAll,
                        "rest_for_one" => actor::SuperviseStrategy::RestForOne,
                        _ => actor::SuperviseStrategy::OneForOne,
                    };
                    let _ = actor::supervise(
                        strategy,
                        &children,
                        max_restarts as u32,
                        within_ms as u32,
                    );
                }
            );

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
            // Secure randomness for the `crypto` polyfill (webapi.js).
            def!("__random_bytes", js_random_bytes);
            // Outbound HTTP for the `fetch` polyfill (capability-gated at the host).
            def!("__fetch", js_fetch);
            def!("__fetch_read", js_fetch_read);
            def!("__fetch_close", js_fetch_close);
            // Durable key-value storage for the `kv` polyfill (capability-gated).
            def!("__kv_get", js_kv_get);
            def!("__kv_set", js_kv_set);
            def!("__kv_delete", js_kv_delete);
            def!("__kv_exists", js_kv_exists);
            def!("__kv_list", js_kv_list);

            // Web API polyfills, the raw actor API, durable storage, then the
            // RPC/service layer.
            let _: () = ctx.eval(WEBAPI_JS).unwrap();
            let _: () = ctx.eval(PROCESS_JS).unwrap();
            let _: () = ctx.eval(KV_JS).unwrap();
            let _: () = ctx.eval(RPC_JS).unwrap();
            // A CommonJS surface so a Bun-bundled (`--format=cjs`) service/worker can
            // populate `module.exports`; a bare script just ignores it.
            let _: () = ctx
                .eval("globalThis.module={exports:{}};globalThis.exports=module.exports;")
                .unwrap();
            // The user's bundle, in a CommonJS module scope. Wrapping in a function
            // keeps its top-level `var`s (e.g. a bundled `var spawn`) out of the
            // global object, where a classic eval would leak them and clobber the
            // runner's globals (Process/spawn/…). A bare script runs now; a
            // service/worker registers its exports for __rusm_entry to drive.
            //
            // Two delivery forms: precompiled **bytecode** (`QJSB` magic — skip the
            // parser, load the module + eval; the IIFE wrapper was applied at build
            // time by `rusm-jsc`), or raw **source** (wrap + eval as before). Both end
            // up populating `globalThis.module.exports` identically.
            const BYTECODE_MAGIC: &[u8] = b"QJSB";
            if let Some(bytecode) = bundle_bytes.strip_prefix(BYTECODE_MAGIC) {
                // SAFETY: bytecode is produced by `rusm-jsc` with the same rquickjs
                // version (0.9.0) the runner embeds; it is a valid module object.
                let module = unsafe { rquickjs::Module::load(ctx.clone(), bytecode) }.unwrap();
                let (_m, promise) = module.eval().unwrap();
                let _ = promise.finish::<()>();
            } else {
                let bundle = String::from_utf8(bundle_bytes).unwrap_or_default();
                let wrapped = format!(
                    "(function(module,exports){{\n{bundle}\n}})(globalThis.module,globalThis.module.exports);"
                );
                let _: () = ctx.eval(wrapped).unwrap();
            }
            // The host sets a serving role (e.g. "http") via the `RUSM_SERVE_ROLE`
            // env capability for a resident server; `__rusm_entry` reads it to pick
            // the serving loop. Absent for ordinary services/workers.
            let role = std::env::var("RUSM_SERVE_ROLE").unwrap_or_default();
            // `{:?}` quotes/escapes the (host-controlled) role as a JS string literal.
            let _: () = ctx
                .eval(format!("globalThis.__rusm_role={role:?};"))
                .unwrap();
            // Drive the entry point (service dispatch / worker `default`) to
            // completion. finish() pumps the QuickJS job queue; a long-running
            // service blocks here (each receive suspends the fiber) until killed.
            let entry: Function = ctx.globals().get("__rusm_entry").unwrap();
            let outcome: Promise = entry.call(()).unwrap();
            let _ = outcome.finish::<()>();
        });
    }
}

export!(Component);
