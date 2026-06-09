//! Serving a component as a **resident** handler (Phase 11): unlike the per-request
//! `wasi:http` path ([`super::http`]), one long-lived component process serves
//! *every* request and **holds state across them** (a counter, a cache, a session
//! map). Each HTTP request becomes a `"fetch"` request on the actor wire — the same
//! JSON envelope the guest SDKs already speak — sent to the resident process; an
//! ephemeral Wasm-free **responder** process owns a `oneshot` and turns the reply
//! back into the HTTP response (a buffered body, or a streamed/SSE body that drains
//! the guest's byte stream directly).
//!
//! A [`ResidentPool`] runs each instance under its **own** one-for-one supervisor —
//! so a crash-looping instance is restarted in isolation and can never take down a
//! healthy sibling — addressed by a registered slot name (a restarted instance's
//! fresh pid is picked up automatically). [`ResidentRoute`] is the single routing
//! decision over a pool: round-robin or header affinity (`shard_by`), with an
//! optional per-instance **in-flight permit** (`max_inflight`) that sheds to 503
//! under overload. Both the HTTP and WS resident servers are thin shells over it.

use std::collections::hash_map::DefaultHasher;
use std::convert::Infallible;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Bytes, Frame};
use hyper::{Response, StatusCode};
use rusm_otp::{Pid, ProcessHandle, Received, Runtime, Strategy, StreamHandle};
use serde::Deserialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use wasmtime_wasi_http::io::TokioIo;

use crate::bridges::wasip2::PreparedComponent;
use crate::caps::Capabilities;
use crate::{Spawner, WasmRuntime};

/// The response body type the resident gateway produces — a boxed body, so a
/// buffered (`Full`) and a streamed/SSE (`StreamBody`) response share one type.
type ResBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

/// Namespaces slot names so independent pools never collide.
static POOL_SEQ: AtomicU64 = AtomicU64::new(0);

/// A pool of long-lived resident component instances, each under its **own**
/// one-for-one supervisor and addressed by a registry slot name. Per-instance
/// supervision means one crash-looping instance is isolated — it never trips a
/// shared budget and takes down healthy siblings. Cheap to clone (`Arc`/`Runtime`).
#[derive(Clone)]
pub(crate) struct ResidentPool {
    rt: Runtime,
    /// One registry name per instance; routing resolves a slot to its live pid, so a
    /// restarted instance (new pid) is found without bookkeeping.
    slots: Arc<Vec<String>>,
    /// One supervisor per instance (held so they aren't dropped), each with its own
    /// independent restart budget.
    _supervisors: Arc<Vec<ProcessHandle>>,
}

impl ResidentPool {
    /// Spawn `instances` (≥1) instances of `prepared`, each under its own one-for-one
    /// supervisor that restarts only that instance; each registers a slot name (and,
    /// for a JS runner, is fed `bundle` as its first message).
    pub(crate) fn spawn(
        spawner: &Arc<Spawner>,
        prepared: PreparedComponent,
        caps: Capabilities,
        bundle: Option<Arc<Vec<u8>>>,
        instances: usize,
    ) -> Self {
        let rt = spawner.rt.clone();
        let n = instances.max(1);
        let uid = POOL_SEQ.fetch_add(1, Ordering::Relaxed);
        let slots: Vec<String> = (0..n).map(|i| format!("__resident.{uid}.{i}")).collect();

        let supervisors: Vec<ProcessHandle> = slots
            .iter()
            .map(|slot| {
                let spawner = Arc::clone(spawner);
                let prepared = prepared.clone();
                let caps = caps.clone();
                let bundle = bundle.clone();
                let slot = slot.clone();
                // Each instance is its own supervised child — an isolated restart budget.
                rt.supervisor(Strategy::OneForOne)
                    .child(move |rt: &Runtime| {
                        let handle = spawner.spawn_component(&prepared, caps.clone());
                        if let Some(bundle) = &bundle {
                            rt.send(handle.pid(), (**bundle).clone()); // js-runner: bundle first
                        }
                        // Register the slot so routing always finds the *current*
                        // instance, even after a restart gives it a new pid. The dead
                        // instance released this name before its `Down` reached the
                        // supervisor, so a restart can't clash on it.
                        rt.register(slot.clone(), handle.pid());
                        handle
                    })
                    .start()
            })
            .collect();

        ResidentPool {
            rt,
            slots: Arc::new(slots),
            _supervisors: Arc::new(supervisors),
        }
    }

    /// Number of instance slots.
    fn len(&self) -> usize {
        self.slots.len()
    }

    /// The live pid in slot `i`, or `None` if it's absent (mid-restart / gave up).
    fn whereis(&self, i: usize) -> Option<Pid> {
        self.rt.whereis(&self.slots[i])
    }

    /// The pool's runtime handle (for sending into instances from a connection task).
    pub(crate) fn runtime(&self) -> &Runtime {
        &self.rt
    }

    /// Wait (bounded) until every instance has registered, so accepting traffic never
    /// races a request ahead of a ready instance.
    pub(crate) async fn ready(&self) {
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            for slot in self.slots.iter() {
                while self.rt.whereis(slot).is_none() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
        })
        .await;
    }

    /// The current live instance pids (introspection / tests).
    pub(crate) fn pids(&self) -> Vec<Pid> {
        self.slots
            .iter()
            .filter_map(|slot| self.rt.whereis(slot))
            .collect()
    }
}

/// How a resident server spreads requests/connections across its pool.
#[derive(Clone)]
enum Shard {
    /// Round-robin across the pool (the default).
    RoundRobin,
    /// Pin by a request header value: the same value always maps to the same
    /// instance (session affinity), so per-key state lives on one instance.
    Header(String),
}

impl Shard {
    /// Parse a `shard_by` config spec: `"header:<name>"` → header affinity; `None`
    /// (or an unrecognised spec) → round-robin.
    fn parse(spec: Option<&str>) -> Self {
        match spec.and_then(|s| s.strip_prefix("header:")) {
            Some(name) => Shard::Header(name.trim().to_ascii_lowercase()),
            None => Shard::RoundRobin,
        }
    }
}

/// The single routing decision over a [`ResidentPool`], shared by the HTTP and WS
/// resident servers: pick an instance (round-robin or header affinity) and, if a
/// `max_inflight` limit is set, take a per-instance permit — held by the returned
/// [`Lease`] for the request/connection's lifetime, so an overloaded instance sheds.
#[derive(Clone)]
pub(crate) struct ResidentRoute {
    pool: ResidentPool,
    shard: Shard,
    next: Arc<AtomicUsize>,
    /// Per-instance in-flight permits; `None` = unbounded. A permit is held by the
    /// [`Lease`] until the request/connection completes, so this bounds concurrent
    /// in-flight work per instance (always-on — no runtime opt-in needed).
    inflight: Option<Arc<Vec<Arc<Semaphore>>>>,
}

/// Holds the routed instance and (when `max_inflight` is set) its in-flight permit;
/// dropping it releases the permit. Keep it for the request/connection's lifetime.
pub(crate) struct Lease {
    pub(crate) pid: Pid,
    _permit: Option<OwnedSemaphorePermit>,
}

impl ResidentRoute {
    pub(crate) fn new(pool: ResidentPool) -> Self {
        Self {
            pool,
            shard: Shard::RoundRobin,
            next: Arc::new(AtomicUsize::new(0)),
            inflight: None,
        }
    }

    /// Set the shard policy from a `shard_by` spec.
    pub(crate) fn shard_by(&mut self, spec: Option<&str>) {
        self.shard = Shard::parse(spec);
    }

    /// Bound concurrent in-flight requests/connections per instance to `limit`;
    /// excess sheds to 503 / a refused upgrade.
    pub(crate) fn max_inflight(&mut self, limit: usize) {
        let sems = (0..self.pool.len())
            .map(|_| Arc::new(Semaphore::new(limit)))
            .collect();
        self.inflight = Some(Arc::new(sems));
    }

    /// The slot a request/connection routes to (round-robin or header affinity).
    fn slot_index(&self, headers: &hyper::HeaderMap) -> usize {
        let n = self.pool.len();
        match &self.shard {
            Shard::RoundRobin => self.next.fetch_add(1, Ordering::Relaxed) % n,
            Shard::Header(name) => {
                let key = headers
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                let mut hasher = DefaultHasher::new();
                key.hash(&mut hasher);
                (hasher.finish() % n as u64) as usize
            }
        }
    }

    /// Route to an instance and take its in-flight permit; `None` if the instance is
    /// absent (mid-restart) or saturated — the caller turns that into a 503.
    pub(crate) fn route(&self, headers: &hyper::HeaderMap) -> Option<Lease> {
        let i = self.slot_index(headers);
        let pid = self.pool.whereis(i)?;
        let permit = match &self.inflight {
            Some(sems) => match Arc::clone(&sems[i]).try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => return None, // saturated — shed
            },
            None => None,
        };
        Some(Lease {
            pid,
            _permit: permit,
        })
    }

    pub(crate) fn runtime(&self) -> &Runtime {
        self.pool.runtime()
    }

    pub(crate) async fn ready(&self) {
        self.pool.ready().await
    }

    pub(crate) fn pids(&self) -> Vec<Pid> {
        self.pool.pids()
    }
}

/// A resident HTTP server: a supervised pool of long-lived instances that serve
/// every request and hold state. Cheap to clone, so it spawns one task per
/// connection like [`super::http::HttpServer`].
#[derive(Clone)]
pub struct ResidentHttpServer {
    route: ResidentRoute,
}

impl WasmRuntime {
    /// Build a resident HTTP server: a supervised pool of `instances` (≥1) long-lived
    /// processes from `prepared` under `caps`, each serving requests from its mailbox
    /// and keeping state across them. The component's `run` should drive a serving
    /// loop (e.g. `rusm_rs::http::serve`). Round-robins across the pool by default;
    /// see [`shard_by`](ResidentHttpServer::shard_by) for affinity.
    pub fn resident_http_server(
        &self,
        prepared: &PreparedComponent,
        caps: Capabilities,
        instances: usize,
    ) -> ResidentHttpServer {
        let pool = ResidentPool::spawn(&self.spawner, prepared.clone(), caps, None, instances);
        ResidentHttpServer {
            route: ResidentRoute::new(pool),
        }
    }

    /// The TS twin of [`resident_http_server`](Self::resident_http_server): the
    /// handler is the *same* `export default { fetch }` on the persistent js-runner,
    /// served statefully via the `RUSM_SERVE_ROLE=http` capability.
    pub fn resident_http_server_js(
        &self,
        bundle: impl Into<Vec<u8>>,
        caps: Capabilities,
        instances: usize,
    ) -> ResidentHttpServer {
        let caps = caps.env("RUSM_SERVE_ROLE", "http");
        let bundle = Arc::new(bundle.into());
        let pool = ResidentPool::spawn(
            &self.spawner,
            self.js_runner().clone(),
            caps,
            Some(bundle),
            instances,
        );
        ResidentHttpServer {
            route: ResidentRoute::new(pool),
        }
    }
}

impl ResidentHttpServer {
    /// Route requests by a `shard_by` spec (`"header:<name>"` → header affinity;
    /// `None` → round-robin) so same-key requests reach the same instance.
    pub fn shard_by(mut self, spec: Option<&str>) -> Self {
        self.route.shard_by(spec);
        self
    }

    /// Bound concurrent in-flight requests per instance to `limit`; excess sheds to
    /// 503. Always-on back-pressure, independent of runtime depth tracking.
    pub fn max_inflight(mut self, limit: usize) -> Self {
        self.route.max_inflight(limit);
        self
    }

    /// The current live instance pids (introspection / tests).
    pub fn instance_pids(&self) -> Vec<Pid> {
        self.route.pids()
    }

    /// Serve HTTP/1.1 on `listener` until it closes — one task per connection.
    /// Abort the task driving this to stop.
    pub async fn serve(self, listener: tokio::net::TcpListener) {
        self.route.ready().await; // don't accept until the pool is up
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

    /// Turn one HTTP request into a `"fetch"` request to a resident instance and the
    /// reply back into the HTTP response. Always `Ok` — failures become status codes.
    async fn handle(
        &self,
        req: hyper::Request<hyper::body::Incoming>,
    ) -> Result<Response<ResBody>, Infallible> {
        let (parts, body) = req.into_parts();
        // Route + take an in-flight permit; the lease is held for the whole request.
        let Some(lease) = self.route.route(&parts.headers) else {
            return Ok(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "no resident instance available",
            ));
        };

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
        let responder = spawn_responder(self.route.runtime(), tx);
        let envelope = serde_json::json!({
            "op": "fetch",
            "ref": 0,
            "from": responder.pid().raw().to_string(),
            // Body crosses as base64 (compact + binary-safe), matching the guest SDK.
            "args": [ { "method": method, "url": url, "headers": headers, "body": STANDARD.encode(&body) } ],
        });
        self.route.runtime().send(
            lease.pid,
            serde_json::to_vec(&envelope).expect("envelope serializes"),
        );

        let reply = rx.await;
        drop(lease); // release the in-flight permit once the reply (head) is in
        Ok(match reply {
            Ok(ResidentReply::Buffered(resp)) => build_response(resp),
            Ok(ResidentReply::Streaming {
                status,
                headers,
                handle,
            }) => build_streaming_response(status, headers, handle),
            Ok(ResidentReply::Err(message)) => {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, &message)
            }
            Err(_) => error_response(StatusCode::BAD_GATEWAY, "resident handler did not reply"),
        })
    }
}

/// The resident's reply, as the responder hands it to the HTTP task.
enum ResidentReply {
    /// A complete buffered response.
    Buffered(WireResponse),
    /// A streaming response (SSE): the head, plus the guest's byte stream which the
    /// HTTP task drains directly into a chunked body — no intermediate channel.
    Streaming {
        status: u16,
        headers: Vec<(String, String)>,
        handle: StreamHandle,
    },
    /// The handler errored.
    Err(String),
}

/// A Wasm-free process that waits for the resident's reply and hands it to the HTTP
/// task — the resident-HTTP twin of the WebSocket writer process. For a streaming
/// reply the guest sends the head, then opens a byte stream to us (`Received::Stream`);
/// we forward the stream **handle** itself (already a back-pressured Tokio channel),
/// so the HTTP body reads the guest directly with no extra hop.
fn spawn_responder(rt: &Runtime, tx: tokio::sync::oneshot::Sender<ResidentReply>) -> ProcessHandle {
    rt.spawn(move |mut ctx| async move {
        // The head reply (a plain message).
        let head = loop {
            match ctx.recv().await {
                Received::Message(bytes) => break bytes,
                _ => continue,
            }
        };
        let resp = match parse_reply(&head) {
            Ok(resp) => resp,
            Err(err) => {
                let _ = tx.send(ResidentReply::Err(err));
                return;
            }
        };
        if !resp.stream {
            let _ = tx.send(ResidentReply::Buffered(resp));
            return;
        }
        // Streaming: the guest opens a byte stream to us next; hand its read end to
        // the HTTP task and exit (no forwarding loop).
        loop {
            if let Received::Stream(handle) = ctx.recv().await {
                let _ = tx.send(ResidentReply::Streaming {
                    status: resp.status,
                    headers: resp.headers,
                    handle,
                });
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

/// The `ok` payload of a resident handler's reply — mirrors `rusm_rs::http::Response`,
/// plus a `stream` flag the SSE path sets (the body then rides a byte stream).
#[derive(Deserialize)]
struct WireResponse {
    status: u16,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default, with = "body_b64")]
    body: Vec<u8>,
    #[serde(default)]
    stream: bool,
}

/// Decode a base64 body string (the guest SDK encodes response bodies this way).
mod body_b64 {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(d)?;
        STANDARD.decode(encoded).map_err(serde::de::Error::custom)
    }
}

fn parse_reply(bytes: &[u8]) -> Result<WireResponse, String> {
    let reply: WireReply = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if let Some(err) = reply.err {
        return Err(err);
    }
    reply.ok.ok_or_else(|| "reply missing `ok`".to_string())
}

fn response_builder(status: u16, headers: Vec<(String, String)>) -> hyper::http::response::Builder {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder
}

fn build_response(resp: WireResponse) -> Response<ResBody> {
    response_builder(resp.status, resp.headers)
        .body(Full::new(Bytes::from(resp.body)).boxed())
        .unwrap_or_else(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "invalid response"))
}

/// Build a chunked, streamed response by draining the guest's byte stream — each
/// chunk becomes a body frame as it's produced (true SSE), with the stream's own
/// Tokio back-pressure carrying through.
fn build_streaming_response(
    status: u16,
    headers: Vec<(String, String)>,
    handle: StreamHandle,
) -> Response<ResBody> {
    let body = futures_util::stream::unfold(handle, |mut handle| async move {
        handle
            .read()
            .await
            .map(|chunk| (Ok::<_, Infallible>(Frame::data(Bytes::from(chunk))), handle))
    });
    response_builder(status, headers)
        .body(StreamBody::new(body).boxed())
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
    use rusm_otp::Runtime;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const COUNT: &[u8] = include_bytes!("../../tests/fixtures/rs_resident_count.wasm");
    const TS_COUNT: &str = include_str!("../../tests/fixtures/ts_resident_count.js");
    const SSE: &[u8] = include_bytes!("../../tests/fixtures/rs_resident_sse.wasm");
    const TS_SSE: &str = include_str!("../../tests/fixtures/ts_resident_sse.js");

    /// One raw HTTP/1.1 GET (Connection: close) → the full response text.
    async fn get(addr: std::net::SocketAddr) -> String {
        get_with(addr, "").await
    }

    /// `get`, with extra raw request headers (each `"Name: value"`, no CRLF).
    async fn get_with(addr: std::net::SocketAddr, extra_headers: &str) -> String {
        let mut req = String::from("GET / HTTP/1.1\r\nHost: rusm\r\nConnection: close\r\n");
        for line in extra_headers.lines().filter(|l| !l.is_empty()) {
            req.push_str(line);
            req.push_str("\r\n");
        }
        req.push_str("\r\n");
        let mut conn = tokio::net::TcpStream::connect(addr).await.unwrap();
        conn.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        conn.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// Bind an ephemeral port, drive the server on a task, return the address.
    async fn serve_on(server: ResidentHttpServer) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(server.serve(listener));
        addr
    }

    fn count_server(wr: &WasmRuntime, instances: usize) -> ResidentHttpServer {
        let prepared = wr
            .prepare_component(&wr.compile_component(COUNT).unwrap(), "run")
            .unwrap();
        wr.resident_http_server(
            &prepared,
            CapabilityProfile::Sandboxed.capabilities(),
            instances,
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_resident_rs_handler_holds_state_across_requests() {
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        let addr = serve_on(count_server(&wr, 1)).await;

        let first = get(addr).await;
        assert!(first.starts_with("HTTP/1.1 200"), "got: {first}");
        assert!(first.contains("hit #1"), "first request: {first}");
        // The SAME instance answers the second request — state persisted.
        assert!(
            get(addr).await.contains("hit #2"),
            "state must persist across requests (per-request would say hit #1)"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_resident_rs_handler_streams_server_sent_events() {
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        let prepared = wr
            .prepare_component(&wr.compile_component(SSE).unwrap(), "run")
            .unwrap();
        let server =
            wr.resident_http_server(&prepared, CapabilityProfile::Sandboxed.capabilities(), 1);
        let addr = serve_on(server).await;

        let response = get(addr).await;
        let lower = response.to_lowercase();
        assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
        assert!(
            lower.contains("text/event-stream"),
            "SSE content-type from the resident handler: {response}"
        );
        assert!(
            lower.contains("transfer-encoding: chunked"),
            "streamed (chunked), not a buffered Content-Length body: {response}"
        );
        for n in 0..5 {
            assert!(
                response.contains(&format!("data: tick {n}")),
                "missing event {n}: {response}"
            );
        }
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
        let addr = serve_on(server).await;

        assert!(get(addr).await.contains("hit #1"));
        assert!(
            get(addr).await.contains("hit #2"),
            "module state must persist across requests on the resident js-runner"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_resident_ts_handler_streams_server_sent_events() {
        // The same `export default { fetch }` returning a streaming ReadableStream —
        // served statefully on the resident js-runner, streamed (chunked) by the host.
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        let server = wr.resident_http_server_js(
            TS_SSE.as_bytes().to_vec(),
            CapabilityProfile::Sandboxed.capabilities(),
            1,
        );
        let addr = serve_on(server).await;

        let response = get(addr).await;
        let lower = response.to_lowercase();
        assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
        assert!(
            lower.contains("text/event-stream"),
            "SSE content-type: {response}"
        );
        assert!(
            lower.contains("transfer-encoding: chunked"),
            "streamed (chunked), not buffered: {response}"
        );
        for n in 0..5 {
            assert!(
                response.contains(&format!("data: tick {n}")),
                "missing event {n}: {response}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_robin_spreads_across_the_pool() {
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        // Two instances, round-robin: consecutive requests hit different instances,
        // each with its own counter — so the first two are both "hit #1".
        let addr = serve_on(count_server(&wr, 2)).await;
        assert!(get(addr).await.contains("hit #1"), "instance A, fresh");
        assert!(get(addr).await.contains("hit #1"), "instance B, fresh");
        assert!(
            get(addr).await.contains("hit #2"),
            "wraps back to instance A"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shard_by_header_pins_a_key_to_one_instance() {
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        // Two instances, but a fixed shard key pins every request to one — so its
        // counter increments monotonically (round-robin would alternate instances).
        let addr = serve_on(count_server(&wr, 2).shard_by(Some("header:x-shard"))).await;
        assert!(get_with(addr, "x-shard: alice").await.contains("hit #1"));
        assert!(get_with(addr, "x-shard: alice").await.contains("hit #2"));
        assert!(
            get_with(addr, "x-shard: alice").await.contains("hit #3"),
            "same key always reaches the same instance"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_crashed_resident_is_restarted_and_keeps_serving() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let server = count_server(&wr, 1);
        let addr = serve_on(server.clone()).await;

        assert!(get(addr).await.contains("hit #1"));
        assert!(get(addr).await.contains("hit #2"));

        // Kill the live instance; its supervisor must restart it.
        let before = server.instance_pids();
        assert_eq!(before.len(), 1, "one instance");
        rt.kill(before[0]);

        // Wait for a fresh instance (a new pid) to be registered into the slot.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let now = server.instance_pids();
            if now.len() == 1 && now[0] != before[0] {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "supervisor did not restart the instance"
            );
            tokio::time::sleep(Duration::from_millis(2)).await;
        }

        let after = get(addr).await;
        assert!(after.starts_with("HTTP/1.1 200"), "still serving: {after}");
        assert!(
            after.contains("hit #1"),
            "restarted instance has fresh state: {after}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_crash_looping_instance_does_not_take_down_siblings() {
        // Per-instance supervision: hammering one slot past its restart budget makes
        // *that* slot give up, but a healthy sibling keeps running — a shared budget
        // would have taken the whole pool down.
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let server = count_server(&wr, 2);
        let _addr = serve_on(server.clone()).await;
        server.route.ready().await; // both instances registered before we inspect

        let slot0_initial = server.route.pool.whereis(0).expect("slot 0 is up");
        let slot1_initial = server.route.pool.whereis(1).expect("slot 1 is up");

        // Hammer slot 0 within its window (default 3 restarts / 5s). The robust,
        // timing-independent property: whatever happens to slot 0 (restart loop or
        // give-up), slot 1 is *never* disturbed — a shared restart budget would have
        // taken the sibling down too.
        for _ in 0..6 {
            if let Some(pid) = server.route.pool.whereis(0) {
                rt.kill(pid);
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_ne!(
            server.route.pool.whereis(0),
            Some(slot0_initial),
            "slot 0 was actually killed/restarted by the hammering"
        );
        assert_eq!(
            server.route.pool.whereis(1),
            Some(slot1_initial),
            "the healthy sibling is untouched — per-instance (isolated) restart budgets"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_saturated_instance_sheds_to_503() {
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        // Zero in-flight permits → every request is shed (proves the gateway sheds
        // when an instance has no capacity, without needing a blocking handler).
        let addr = serve_on(count_server(&wr, 1).max_inflight(0)).await;
        assert!(
            get(addr).await.starts_with("HTTP/1.1 503"),
            "a saturated instance sheds to 503"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_in_flight_permit_is_released_after_each_request() {
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        // One permit, used sequentially: each request releases it, so all succeed
        // (a leak would 503 the second request).
        let addr = serve_on(count_server(&wr, 1).max_inflight(1)).await;
        assert!(get(addr).await.contains("hit #1"));
        assert!(get(addr).await.contains("hit #2"));
        assert!(get(addr).await.contains("hit #3"));
    }
}
