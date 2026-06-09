//! Serving a component as a **resident** HTTP handler (Phase 11): unlike the
//! per-request `wasi:http` path ([`super::http`]), one long-lived component process
//! serves *every* request and **holds state across them** (a counter, a cache, a
//! session map). Each HTTP request becomes a `"fetch"` request on the actor wire —
//! the same JSON envelope the guest SDKs already speak — sent to the resident
//! process; an ephemeral Wasm-free **responder** process owns a `oneshot` and turns
//! the resident's reply back into the HTTP response. The resident's linear memory
//! lives for the process's lifetime, which is what makes it stateful.
//!
//! With `instances > 1` the requests round-robin across a small pool (each instance
//! keeps its own state). A resident serves requests one at a time, so a handler
//! must stay fast and offload slow work to a spawned helper — see the plan's
//! head-of-line-blocking note.

use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Response, StatusCode};
use rusm_otp::{Pid, ProcessHandle, Received, Runtime};
use serde::Deserialize;
use wasmtime_wasi_http::io::TokioIo;

use crate::bridges::wasip2::PreparedComponent;
use crate::caps::Capabilities;
use crate::WasmRuntime;

/// The response body type the resident gateway produces (a fully-buffered body for
/// now; SSE streaming bodies land in a later step).
type ResBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

/// A resident HTTP server: a pool of one or more long-lived component processes
/// that serve every request and hold state. Cheap to clone (all `Arc`/`Runtime`),
/// so it spawns one task per connection like [`super::http::HttpServer`].
#[derive(Clone)]
pub struct ResidentHttpServer {
    rt: Runtime,
    /// The resident instances; requests round-robin across them.
    instances: Arc<Vec<Pid>>,
    next: Arc<AtomicUsize>,
}

impl WasmRuntime {
    /// Build a resident HTTP server: spawn `instances` (at least one) long-lived
    /// processes from `prepared` under `caps`, each serving requests from its
    /// mailbox and keeping state across them. The component's `run` export should
    /// drive a serving loop (e.g. `rusm_rs::http::serve`).
    pub fn resident_http_server(
        &self,
        prepared: &PreparedComponent,
        caps: Capabilities,
        instances: usize,
    ) -> ResidentHttpServer {
        let n = instances.max(1);
        // Drop each handle: the process is kept live by the runtime table (dropping
        // a handle never kills), and the node's shutdown reaps it.
        let instances: Vec<Pid> = (0..n)
            .map(|_| self.spawn_component_with(prepared, caps.clone()).pid())
            .collect();
        ResidentHttpServer {
            rt: self.spawner.rt.clone(),
            instances: Arc::new(instances),
            next: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Build a resident HTTP server whose handler is a **TypeScript/JS bundle**
    /// (Bun-built) on the embedded js-runner — the TS twin of
    /// [`resident_http_server`](Self::resident_http_server). The guest writes the
    /// *same* `export default { fetch }` (or default handler) it would for the
    /// per-request path; the `RUSM_SERVE_ROLE=http` capability tells the runner to
    /// serve it statefully on one long-lived instance, so module-scope state
    /// persists across requests.
    pub fn resident_http_server_js(
        &self,
        bundle: impl Into<Vec<u8>>,
        caps: Capabilities,
        instances: usize,
    ) -> ResidentHttpServer {
        let bundle: Vec<u8> = bundle.into();
        let caps = caps.env("RUSM_SERVE_ROLE", "http");
        let n = instances.max(1);
        let instances: Vec<Pid> = (0..n)
            .map(|_| self.spawn_js_with(bundle.clone(), caps.clone()).pid())
            .collect();
        ResidentHttpServer {
            rt: self.spawner.rt.clone(),
            instances: Arc::new(instances),
            next: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl ResidentHttpServer {
    /// Serve HTTP/1.1 on `listener` until it closes — one task per connection.
    /// Abort the task driving this to stop.
    pub async fn serve(self, listener: tokio::net::TcpListener) {
        loop {
            let Ok((stream, _peer)) = listener.accept().await else {
                break;
            };
            stream.set_nodelay(true).ok();
            let server = self.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |req| {
                    let server = server.clone();
                    async move { server.handle(req).await }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .keep_alive(true)
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    }

    /// Pick the next resident instance (round-robin).
    fn route(&self) -> Pid {
        let i = self.next.fetch_add(1, Ordering::Relaxed) % self.instances.len();
        self.instances[i]
    }

    /// Turn one HTTP request into a `"fetch"` request to a resident instance and the
    /// reply back into the HTTP response. Always `Ok` — failures become status codes.
    async fn handle(
        &self,
        req: hyper::Request<hyper::body::Incoming>,
    ) -> Result<Response<ResBody>, Infallible> {
        let (parts, body) = req.into_parts();
        let method = parts.method.as_str().to_string();
        let url = parts
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/")
            .to_string();
        let headers: Vec<(String, String)> = parts
            .headers
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let body = match body.collect().await {
            Ok(collected) => collected.to_bytes().to_vec(),
            Err(_) => {
                return Ok(error_response(
                    StatusCode::BAD_REQUEST,
                    "could not read body",
                ))
            }
        };

        // The responder's fresh pid is the reply target; the resident sends exactly
        // one reply to it, so no ref-matching is needed on this side.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let responder = spawn_responder(&self.rt, tx);
        let envelope = serde_json::json!({
            "op": "fetch",
            "ref": 0,
            "from": responder.pid().raw().to_string(),
            "args": [ { "method": method, "url": url, "headers": headers, "body": body } ],
        });
        self.rt.send(
            self.route(),
            serde_json::to_vec(&envelope).expect("envelope serializes"),
        );

        Ok(match rx.await {
            Ok(Ok(resp)) => build_response(resp),
            Ok(Err(message)) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &message),
            Err(_) => error_response(StatusCode::BAD_GATEWAY, "resident handler did not reply"),
        })
    }
}

/// A Wasm-free process that waits for the resident's single reply and completes the
/// `oneshot` — the resident-HTTP twin of the WebSocket writer process.
fn spawn_responder(
    rt: &Runtime,
    tx: tokio::sync::oneshot::Sender<Result<WireResponse, String>>,
) -> ProcessHandle {
    rt.spawn(move |mut ctx| async move {
        loop {
            if let Received::Message(bytes) = ctx.recv().await {
                let _ = tx.send(parse_reply(&bytes));
                return;
            }
        }
    })
}

/// A reply envelope `{ref, ok|err}` as produced by the guest's `reply_ok`/`reply_err`.
#[derive(Deserialize)]
struct WireReply {
    #[serde(default)]
    ok: Option<WireResponse>,
    #[serde(default)]
    err: Option<String>,
}

/// The `ok` payload of a resident handler's reply — mirrors `rusm_rs::http::Response`.
#[derive(Deserialize)]
struct WireResponse {
    status: u16,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    body: Vec<u8>,
}

fn parse_reply(bytes: &[u8]) -> Result<WireResponse, String> {
    let reply: WireReply = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if let Some(err) = reply.err {
        return Err(err);
    }
    reply.ok.ok_or_else(|| "reply missing `ok`".to_string())
}

fn build_response(resp: WireResponse) -> Response<ResBody> {
    let mut builder = Response::builder().status(resp.status);
    for (name, value) in resp.headers {
        builder = builder.header(name, value);
    }
    builder
        .body(Full::new(Bytes::from(resp.body)).boxed())
        .unwrap_or_else(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "invalid response"))
}

fn error_response(status: StatusCode, message: &str) -> Response<ResBody> {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::from(message.to_owned())).boxed())
        .expect("error response builds")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CapabilityProfile;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const COUNT: &[u8] = include_bytes!("../../tests/fixtures/rs_resident_count.wasm");
    const TS_COUNT: &str = include_str!("../../tests/fixtures/ts_resident_count.js");

    /// One raw HTTP/1.1 GET (Connection: close) → the full response text.
    async fn get(addr: std::net::SocketAddr) -> String {
        let mut conn = tokio::net::TcpStream::connect(addr).await.unwrap();
        conn.write_all(b"GET / HTTP/1.1\r\nHost: rusm\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut buf = Vec::new();
        conn.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_resident_rs_handler_holds_state_across_requests() {
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        let prepared = wr
            .prepare_component(&wr.compile_component(COUNT).unwrap(), "run")
            .unwrap();
        // One resident instance, so the counter is shared across every request.
        let server =
            wr.resident_http_server(&prepared, CapabilityProfile::Sandboxed.capabilities(), 1);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(server.serve(listener));

        let first = get(addr).await;
        assert!(first.starts_with("HTTP/1.1 200"), "got: {first}");
        assert!(first.contains("hit #1"), "first request: {first}");

        // The SAME instance answers the second request — state persisted.
        let second = get(addr).await;
        assert!(
            second.contains("hit #2"),
            "state must persist across requests (per-request would say hit #1): {second}"
        );

        handle.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_resident_ts_handler_holds_module_state_across_requests() {
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        // The SAME `export default` handler the per-request path uses — resident
        // deployment alone makes its module-scope counter persist.
        let server = wr.resident_http_server_js(
            TS_COUNT.as_bytes().to_vec(),
            CapabilityProfile::Sandboxed.capabilities(),
            1,
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(server.serve(listener));

        let first = get(addr).await;
        assert!(first.starts_with("HTTP/1.1 200"), "got: {first}");
        assert!(first.contains("hit #1"), "first request: {first}");

        let second = get(addr).await;
        assert!(
            second.contains("hit #2"),
            "module state must persist across requests on the resident js-runner: {second}"
        );

        handle.abort();
    }
}
