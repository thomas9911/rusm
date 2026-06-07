//! The **rusm-ts js-http-runner**: a `wasi:http` component that embeds rquickjs and
//! runs a Bun-built TS/JS bundle's `fetch` handler — the TypeScript twin of the wstd
//! HTTP path. The bundle arrives via the `RUSM_JS_BUNDLE` env var (a capability the
//! host grants per served component); each request instantiates fresh, evaluates the
//! bundle, builds a `Request`, runs `fetch`, and marshals the `Response` back.
//!
//! Standards-first: the guest writes the Workers/Deno `export default { fetch }`
//! shape (or `export default (req) => Response`); the host owns the wasi:http bridge.

use anyhow::{anyhow, Result};
use http_body_util::BodyExt;
use rquickjs::{Array, Context, Function, Object, Promise, Runtime, TypedArray};
use wstd::http::body::Body;
use wstd::http::{Request, Response};

/// Shared standards polyfills (TextEncoder/URL/Headers/…), then the fetch bridge.
const WEBAPI_JS: &str = include_str!("../../js-runner/bridge/webapi.js");
const HTTP_JS: &str = include_str!("../bridge/http.js");

#[wstd::http_server]
async fn main(request: Request<Body>) -> Result<Response<Body>> {
    let bundle = std::env::var("RUSM_JS_BUNDLE").unwrap_or_default();

    let method = request.method().as_str().to_owned();
    let url = request.uri().to_string();
    let headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
        .collect();
    let body = request
        .into_body()
        .into_boxed_body()
        .collect()
        .await?
        .to_bytes();

    let (status, resp_headers, resp_body) = match run_fetch(&bundle, &method, &url, &headers, &body)
    {
        Ok(triple) => triple,
        // Surface the cause as a 500 body rather than dropping the connection — a
        // guest fetch error shouldn't look like a dead socket.
        Err(e) => (
            500,
            vec![("content-type".to_owned(), "text/plain".to_owned())],
            format!("js-http-runner error: {e}").into_bytes(),
        ),
    };

    let mut builder = Response::builder().status(status);
    for (k, v) in resp_headers {
        builder = builder.header(k, v);
    }
    Ok(builder.body(Body::from(resp_body))?)
}

/// Evaluate `bundle` and run its `fetch` handler against the request, returning the
/// response status, headers, and body bytes. All rquickjs work is synchronous CPU
/// work on this request's fiber (instance-per-request, so no sharing).
fn run_fetch(
    bundle: &str,
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<(u16, Vec<(String, String)>, Vec<u8>)> {
    let rt = Runtime::new().map_err(|e| anyhow!("quickjs runtime: {e}"))?;
    let context = Context::full(&rt).map_err(|e| anyhow!("quickjs context: {e}"))?;

    let mut out: Option<(u16, Vec<(String, String)>, Vec<u8>)> = None;
    context.with(|ctx| -> Result<()> {
        let g = ctx.globals();
        g.set(
            "__print",
            Function::new(ctx.clone(), |s: String| eprintln!("{s}"))
                .map_err(|e| anyhow!("define __print: {e}"))?,
        )
        .ok();

        eval(&ctx, WEBAPI_JS, "webapi.js")?;
        eval(&ctx, HTTP_JS, "http.js")?;
        eval(
            &ctx,
            "globalThis.module={exports:{}};globalThis.exports=module.exports;",
            "cjs-shim",
        )?;
        // Wrap the bundle in a CommonJS scope so its top-level vars don't leak.
        let wrapped = format!(
            "(function(module,exports){{\n{bundle}\n}})(globalThis.module,globalThis.module.exports);"
        );
        eval(&ctx, &wrapped, "bundle")?;

        // Marshal the request: headers as [[k,v],…], body as a Uint8Array.
        let header_arr = Array::new(ctx.clone()).map_err(|e| anyhow!("header array: {e}"))?;
        for (i, (k, v)) in headers.iter().enumerate() {
            let pair = Array::new(ctx.clone()).map_err(|e| anyhow!("header pair: {e}"))?;
            pair.set(0, k.as_str()).ok();
            pair.set(1, v.as_str()).ok();
            header_arr.set(i, pair).ok();
        }
        let body_ta = TypedArray::new(ctx.clone(), body).map_err(|e| anyhow!("body array: {e}"))?;

        let func: Function = g.get("__rusm_http").map_err(|e| anyhow!("__rusm_http: {e}"))?;
        let promise: Promise = func
            .call((method, url, header_arr, body_ta))
            .map_err(|e| js_err(&ctx, e, "call fetch handler"))?;
        // Our handlers resolve from the QuickJS job queue (no host async), so
        // `finish` drives the queue and returns the settled response.
        let result: Object = promise
            .finish()
            .map_err(|e| js_err(&ctx, e, "resolve fetch"))?;

        let status: i32 = result.get("status").map_err(|e| anyhow!("status: {e}"))?;
        let resp_headers = read_headers(result.get("headers").map_err(|e| anyhow!("headers: {e}"))?)?;
        let body_ta: TypedArray<u8> = result.get("body").map_err(|e| anyhow!("body: {e}"))?;
        let resp_body = body_ta.as_bytes().unwrap_or(&[]).to_vec();

        out = Some((status as u16, resp_headers, resp_body));
        Ok(())
    })?;

    out.ok_or_else(|| anyhow!("fetch produced no response"))
}

fn eval(ctx: &rquickjs::Ctx<'_>, src: &str, what: &str) -> Result<()> {
    ctx.eval::<(), _>(src)
        .map_err(|e| js_err(ctx, e, &format!("eval {what}")))
}

/// Turn a rquickjs error into one carrying the actual JS exception message/stack
/// (the bare `Error::Exception` says nothing — the detail is in `ctx.catch()`).
fn js_err(ctx: &rquickjs::Ctx<'_>, err: rquickjs::Error, what: &str) -> anyhow::Error {
    if !matches!(err, rquickjs::Error::Exception) {
        return anyhow!("{what}: {err}");
    }
    let caught = ctx.catch();
    if let Some(ex) = caught.as_exception() {
        let msg = ex.message().unwrap_or_default();
        let stack = ex.stack().unwrap_or_default();
        return anyhow!("{what}: {msg}\n{stack}");
    }
    anyhow!("{what}: {caught:?}")
}

fn read_headers(arr: Array<'_>) -> Result<Vec<(String, String)>> {
    let mut out = Vec::with_capacity(arr.len());
    for pair in arr.iter::<Array>() {
        let pair = pair.map_err(|e| anyhow!("header pair: {e}"))?;
        let k: String = pair.get(0).map_err(|e| anyhow!("header key: {e}"))?;
        let v: String = pair.get(1).map_err(|e| anyhow!("header value: {e}"))?;
        out.push((k, v));
    }
    Ok(out)
}
