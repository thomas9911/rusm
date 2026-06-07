//! The **rusm-ts js-http-runner**: a raw `wasi:http` component that embeds rquickjs
//! and runs a Bun-built TS/JS bundle's `fetch` handler — both request/response HTTP
//! and **streaming SSE**. The TypeScript twin of the wstd HTTP path.
//!
//! Why raw `wasi:http` (not wstd)? To stream, the response body must lazily pull each
//! event from the JS reader *as bytes are sent* — i.e. touch the rquickjs context.
//! rquickjs's context is `!Send`, and wstd's `Body` is `Send + 'static`, so a wstd
//! streaming body can't hold it. The raw handler is synchronous `handle(request,
//! out)`: we set headers, take the body's output-stream, and write chunks ourselves —
//! the context just lives in a local for the duration, nothing crosses a thread
//! boundary, no `Send` bound. We own the bridge.
//!
//! The bundle is delivered per-request via the `RUSM_JS_BUNDLE` env capability; the
//! guest writes the Workers/Deno `export default { fetch }` shape. A static response
//! is written in one shot; a `ReadableStream` body is pulled chunk-by-chunk and
//! flushed as it's produced (back-pressured by the socket via blocking writes).

use anyhow::{anyhow, Result};
use rquickjs::{Array, Context, Ctx, Function, Object, Promise, Runtime, TypedArray};
use wasip2::exports::wasi::http::incoming_handler::Guest;
use wasip2::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use wasip2::io::streams::OutputStream;

const WEBAPI_JS: &str = include_str!("../../js-runner/bridge/webapi.js");
const HTTP_JS: &str = include_str!("../bridge/http.js");
/// wasi:io `blocking-write-and-flush` accepts at most 4096 bytes per call.
const WRITE_CHUNK: usize = 4096;

/// The handler's response metadata; the body is either fully-known bytes (static) or
/// pulled incrementally from the JS reader (`streaming`).
struct Outcome {
    status: u16,
    headers: Vec<(String, String)>,
    streaming: bool,
    body: Vec<u8>,
}

struct Handler;

impl Guest for Handler {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let bundle = std::env::var("RUSM_JS_BUNDLE").unwrap_or_default();
        let (method, url, req_headers, req_body) = read_request(request);

        // One QuickJS runtime per request (instance-per-request isolation). Held for
        // the whole handler so streaming pulls can re-enter it.
        let Ok(runtime) = Runtime::new() else {
            return fail(response_out, "quickjs runtime unavailable");
        };
        let Ok(context) = Context::full(&runtime) else {
            return fail(response_out, "quickjs context unavailable");
        };

        // Phase 1: evaluate the bundle and run `fetch`, settling status + headers
        // (+ a static body, or arming the stream reader).
        let outcome = context.with(|ctx| {
            run_fetch(&ctx, &bundle, &method, &url, &req_headers, &req_body)
        });
        let outcome = match outcome {
            Ok(o) => o,
            Err(e) => return fail(response_out, &format!("{e}")),
        };

        // Phase 2: hand off the response head, then write the body.
        let response = build_response(outcome.status, &outcome.headers);
        let Ok(out_body) = response.body() else {
            return fail(response_out, "response has no body");
        };
        ResponseOutparam::set(response_out, Ok(response));
        let Ok(stream) = out_body.write() else { return };

        if outcome.streaming {
            // Pull each chunk from the JS reader and flush it — true incremental SSE.
            // A write error means the client hung up; stop pulling.
            loop {
                match context.with(|ctx| pull_chunk(&ctx)) {
                    Ok(Some(chunk)) => {
                        if write_all(&stream, &chunk).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        } else {
            let _ = write_all(&stream, &outcome.body);
        }

        drop(stream); // release the borrow before finishing the body
        let _ = OutgoingBody::finish(out_body, None);
    }
}

/// Evaluate the bundle, build the JS `Request`, run `fetch`, and read back the
/// response head. For a static body the bytes come back now; for a stream the JS
/// reader is armed and `pull_chunk` drives it.
fn run_fetch(
    ctx: &Ctx<'_>,
    bundle: &str,
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<Outcome> {
    let g = ctx.globals();
    g.set(
        "__print",
        Function::new(ctx.clone(), |s: String| eprintln!("{s}"))
            .map_err(|e| anyhow!("define __print: {e}"))?,
    )
    .ok();

    eval(ctx, WEBAPI_JS, "webapi.js")?;
    eval(ctx, HTTP_JS, "http.js")?;
    eval(
        ctx,
        "globalThis.module={exports:{}};globalThis.exports=module.exports;",
        "cjs-shim",
    )?;
    // Wrap the bundle in a CommonJS scope so its top-level vars don't leak.
    let wrapped = format!(
        "(function(module,exports){{\n{bundle}\n}})(globalThis.module,globalThis.module.exports);"
    );
    eval(ctx, &wrapped, "bundle")?;

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
        .map_err(|e| js_err(ctx, e, "call fetch handler"))?;
    let result: Object = promise
        .finish()
        .map_err(|e| js_err(ctx, e, "resolve fetch"))?;

    let status: i32 = result.get("status").map_err(|e| anyhow!("status: {e}"))?;
    let headers = read_headers(result.get("headers").map_err(|e| anyhow!("headers: {e}"))?)?;
    let streaming: bool = result.get("streaming").unwrap_or(false);
    let body = if streaming {
        Vec::new()
    } else {
        let ta: TypedArray<u8> = result.get("body").map_err(|e| anyhow!("body: {e}"))?;
        ta.as_bytes().unwrap_or(&[]).to_vec()
    };

    Ok(Outcome {
        status: status as u16,
        headers,
        streaming,
        body,
    })
}

/// Pull the next streamed chunk from the JS reader: `Some(bytes)` or `None` at end.
fn pull_chunk(ctx: &Ctx<'_>) -> Result<Option<Vec<u8>>> {
    let func: Function = ctx
        .globals()
        .get("__rusm_http_pull")
        .map_err(|e| anyhow!("__rusm_http_pull: {e}"))?;
    let promise: Promise = func.call(()).map_err(|e| js_err(ctx, e, "pull"))?;
    let chunk: Option<TypedArray<u8>> = promise.finish().map_err(|e| js_err(ctx, e, "pull resolve"))?;
    Ok(chunk.map(|ta| ta.as_bytes().unwrap_or(&[]).to_vec()))
}

/// Read the request's method, full URL, headers, and body bytes.
fn read_request(request: IncomingRequest) -> (String, String, Vec<(String, String)>, Vec<u8>) {
    let method = method_str(request.method());
    let headers: Vec<(String, String)> = request
        .headers()
        .entries()
        .into_iter()
        .map(|(k, v)| (k, String::from_utf8_lossy(&v).into_owned()))
        .collect();
    // Reconstruct a full URL (scheme + host + path) so the guest's `new URL(req.url)`
    // parses the query — wasi:http only carries the path-with-query.
    let path = request.path_with_query().unwrap_or_else(|| "/".to_owned());
    let host = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "localhost".to_owned());
    let url = format!("http://{host}{path}");

    let body = read_body(&request);
    (method, url, headers, body)
}

fn read_body(request: &IncomingRequest) -> Vec<u8> {
    let Ok(incoming) = request.consume() else {
        return Vec::new();
    };
    let Ok(stream) = incoming.stream() else {
        return Vec::new();
    };
    let mut body = Vec::new();
    // `blocking_read` blocks until data or end-of-stream (Err = closed/EOF).
    while let Ok(chunk) = stream.blocking_read(64 * 1024) {
        if chunk.is_empty() {
            break;
        }
        body.extend_from_slice(&chunk);
    }
    body
}

fn build_response(status: u16, headers: &[(String, String)]) -> OutgoingResponse {
    let fields = Fields::new();
    for (k, v) in headers {
        // append (not set) so multi-value headers survive; ignore invalid names.
        let _ = fields.append(k, v.as_bytes());
    }
    let response = OutgoingResponse::new(fields);
    let _ = response.set_status_code(status);
    response
}

/// Write all of `data` in ≤4096-byte flushes (the wasi:io per-call limit).
fn write_all(stream: &OutputStream, data: &[u8]) -> Result<()> {
    for chunk in data.chunks(WRITE_CHUNK) {
        stream
            .blocking_write_and_flush(chunk)
            .map_err(|e| anyhow!("write: {e:?}"))?;
    }
    Ok(())
}

/// Send a 500 with the cause as the body — a guest error is a 500, never a dropped
/// socket. Only valid before the response head has been handed off.
fn fail(response_out: ResponseOutparam, message: &str) {
    let fields = Fields::new();
    let _ = fields.append("content-type", b"text/plain");
    let response = OutgoingResponse::new(fields);
    let _ = response.set_status_code(500);
    let body = response.body().ok();
    ResponseOutparam::set(response_out, Ok(response));
    if let Some(body) = body {
        if let Ok(stream) = body.write() {
            let _ = write_all(&stream, format!("js-http-runner error: {message}").as_bytes());
            drop(stream);
            let _ = OutgoingBody::finish(body, None);
        }
    }
}

fn method_str(method: Method) -> String {
    match method {
        Method::Get => "GET".to_owned(),
        Method::Head => "HEAD".to_owned(),
        Method::Post => "POST".to_owned(),
        Method::Put => "PUT".to_owned(),
        Method::Delete => "DELETE".to_owned(),
        Method::Connect => "CONNECT".to_owned(),
        Method::Options => "OPTIONS".to_owned(),
        Method::Trace => "TRACE".to_owned(),
        Method::Patch => "PATCH".to_owned(),
        Method::Other(s) => s,
    }
}

fn eval(ctx: &Ctx<'_>, src: &str, what: &str) -> Result<()> {
    ctx.eval::<(), _>(src)
        .map_err(|e| js_err(ctx, e, &format!("eval {what}")))
}

/// Turn a rquickjs error into one carrying the actual JS exception message + stack
/// (the bare `Error::Exception` says nothing — the detail is in `ctx.catch()`).
fn js_err(ctx: &Ctx<'_>, err: rquickjs::Error, what: &str) -> anyhow::Error {
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

wasip2::http::proxy::export!(Handler with_types_in wasip2);
