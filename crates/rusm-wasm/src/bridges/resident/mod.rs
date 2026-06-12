//! Serving a component as a **resident** handler (Phase 11): unlike the per-request
//! `wasi:http` path ([`super::http`]), one long-lived component process serves
//! *every* request and **holds state across them** (a counter, a cache, a session
//! map). Each HTTP request becomes a `"fetch"` request on the actor wire — the same
//! JSON envelope the guest SDKs already speak — sent to the resident process; an
//! ephemeral Wasm-free **responder** process owns a `oneshot` and turns the reply
//! back into the HTTP response (a buffered body, or a streamed/SSE body that drains
//! the guest's byte stream directly).
//!
//! Structure (mirrors `bridges/`, one concern per file):
//! - [`pool`] — the supervised instance pool (per-instance restart isolation).
//! - [`route`] — the routing decision (shard policy + in-flight permits).
//! - this module — the HTTP gateway + the JSON reply wire. The resident WebSocket
//!   gateway lives in [`super::ws`] (it reuses the same [`ResidentRoute`]).

mod pool;
mod route;
mod ws;

pub(crate) use pool::ResidentPool;
pub(crate) use route::{Lease, ResidentRoute};
pub use ws::ResidentWsServer;

use std::convert::Infallible;
use std::sync::Arc;

use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Bytes, Frame};
use hyper::{Response, StatusCode};
use rusm_otp::{Pid, ProcessHandle, Received, Runtime, StreamHandle};
use serde::Deserialize;
use wasmtime_wasi_http::io::TokioIo;

use crate::bridges::wasip2::PreparedComponent;
use crate::caps::Capabilities;
use crate::WasmRuntime;

/// The response body type an actor-wire HTTP gateway produces — a boxed body, so a
/// buffered (`Full`) and a streamed/SSE (`StreamBody`) response share one type.
/// Shared with the per-request [`routed`](super::routed) gateway.
pub(crate) type ResBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

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
        // The shared `rusm_wire::Request` serializes the body as base64 — one wire
        // definition for host and guest.
        let request = rusm_wire::Request {
            method,
            url,
            headers,
            body,
        };
        let envelope = serde_json::json!({
            "op": "fetch",
            "ref": 0,
            "from": responder.pid().raw().to_string(),
            "args": [ request ],
        });
        self.route.runtime().send(
            lease.pid,
            serde_json::to_vec(&envelope).expect("envelope serializes"),
        );

        let reply = rx.await;
        drop(lease); // release the in-flight permit once the reply (head) is in
        Ok(match reply {
            Ok(GatewayReply::Buffered(resp)) => build_response(resp),
            Ok(GatewayReply::Streaming {
                status,
                headers,
                handle,
            }) => build_streaming_response(status, headers, handle),
            Ok(GatewayReply::Err(message)) => {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, &message)
            }
            Err(_) => error_response(StatusCode::BAD_GATEWAY, "resident handler did not reply"),
        })
    }
}

/// A handler's reply, as the responder hands it to the HTTP task. Shared by the
/// resident and per-request ([`routed`](super::routed)) gateways.
pub(crate) enum GatewayReply {
    /// A complete buffered response.
    Buffered(rusm_wire::Response),
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
pub(crate) fn spawn_responder(
    rt: &Runtime,
    tx: tokio::sync::oneshot::Sender<GatewayReply>,
) -> ProcessHandle {
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
                let _ = tx.send(GatewayReply::Err(err));
                return;
            }
        };
        if !resp.stream {
            let _ = tx.send(GatewayReply::Buffered(resp));
            return;
        }
        // Streaming: the guest opens a byte stream to us next; hand its read end to
        // the HTTP task and exit (no forwarding loop).
        loop {
            if let Received::Stream(handle) = ctx.recv().await {
                let _ = tx.send(GatewayReply::Streaming {
                    status: resp.status,
                    headers: resp.headers,
                    handle,
                });
                return;
            }
        }
    })
}

/// A reply envelope `{ref, ok|err}` as produced by the guest's `reply_ok`/`reply_err`;
/// `ok` is the shared [`rusm_wire::Response`].
#[derive(Deserialize)]
struct WireReply {
    #[serde(default)]
    ok: Option<rusm_wire::Response>,
    #[serde(default)]
    err: Option<String>,
}

fn parse_reply(bytes: &[u8]) -> Result<rusm_wire::Response, String> {
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

pub(crate) fn build_response(resp: rusm_wire::Response) -> Response<ResBody> {
    response_builder(resp.status, resp.headers)
        .body(Full::new(Bytes::from(resp.body)).boxed())
        .unwrap_or_else(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "invalid response"))
}

/// Build a chunked, streamed response by draining the guest's byte stream — each
/// chunk becomes a body frame as it's produced (true SSE), with the stream's own
/// Tokio back-pressure carrying through.
pub(crate) fn build_streaming_response(
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

pub(crate) fn error_response(status: StatusCode, message: &str) -> Response<ResBody> {
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

    const COUNT: &[u8] = include_bytes!("../../../tests/fixtures/rs_resident_count.wasm");
    const TS_COUNT: &str = include_str!("../../../tests/fixtures/ts_resident_count.js");
    const SSE: &[u8] = include_bytes!("../../../tests/fixtures/rs_resident_sse.wasm");
    const TS_SSE: &str = include_str!("../../../tests/fixtures/ts_resident_sse.js");

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
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let now = server.instance_pids();
            if now.len() == 1 && now[0] != before[0] {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "supervisor did not restart the instance"
            );
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
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

        let slot0_initial = server.route.slot_pid(0).expect("slot 0 is up");
        let slot1_initial = server.route.slot_pid(1).expect("slot 1 is up");

        // Hammer slot 0 within its window (default 3 restarts / 5s). The robust,
        // timing-independent property: whatever happens to slot 0 (restart loop or
        // give-up), slot 1 is *never* disturbed — a shared restart budget would have
        // taken the sibling down too.
        for _ in 0..6 {
            if let Some(pid) = server.route.slot_pid(0) {
                rt.kill(pid);
            }
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        assert_ne!(
            server.route.slot_pid(0),
            Some(slot0_initial),
            "slot 0 was actually killed/restarted by the hammering"
        );
        assert_eq!(
            server.route.slot_pid(1),
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

    /// Open a keep-alive SSE connection and return the live socket.
    async fn open_sse(addr: std::net::SocketAddr) -> tokio::net::TcpStream {
        let mut conn = tokio::net::TcpStream::connect(addr).await.unwrap();
        conn.set_nodelay(true).ok();
        conn.write_all(b"GET /events HTTP/1.1\r\nHost: rusm\r\nAccept: text/event-stream\r\n\r\n")
            .await
            .unwrap();
        conn
    }

    /// Read from `conn` into `acc` until `marker` appears (or time out, failing).
    async fn read_until(conn: &mut tokio::net::TcpStream, acc: &mut String, marker: &str) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut buf = [0u8; 1024];
        while !acc.contains(marker) {
            let n = tokio::time::timeout_at(deadline, conn.read(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {marker:?}; got:\n{acc}"))
                .unwrap();
            assert!(n > 0, "connection closed before {marker:?}; got:\n{acc}");
            acc.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    }

    /// Query the broker's live subscriber count (`[2][reply pid]` → `[count]`).
    async fn broker_count(rt: &Runtime, broker: Pid) -> u8 {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let qrt = rt.clone();
        rt.spawn(move |mut ctx| async move {
            let mut query = vec![2u8];
            query.extend_from_slice(&ctx.pid().raw().to_le_bytes());
            qrt.send(broker, query);
            if let Received::Message(m) = ctx.recv().await {
                let _ = tx.send(m.first().copied().unwrap_or(0));
            }
        });
        rx.await.unwrap_or(0)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_sse_fan_out_broadcasts_to_every_connection() {
        // The endless-SSE pattern `serve_sse`'s docs prescribe, proven end-to-end: a
        // resident *acceptor* offloads each connection to a *pump* process that
        // subscribes to a broker and live-tails it to the client's stream. One publish
        // must reach *every* open connection — 1-publisher → N-subscriber fan-out, with
        // a single acceptor instance never head-of-line blocked. (In genius-rusm the
        // broker is meta-json; here it's a small native process.)
        const ACCEPTOR: &[u8] = include_bytes!("../../../tests/fixtures/sse_acceptor.wasm");
        const PUMP: &[u8] = include_bytes!("../../../tests/fixtures/sse_pump.wasm");
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();

        // Broker: subscribe `[0][pid]` (and **monitor** it, so a dead subscriber is
        // pruned — the crash-safe OTP cleanup), publish `[1][payload]` (fan out), and
        // a count query `[2][reply pid]` → `[count]` (for the test to observe pruning).
        let broker_rt = rt.clone();
        let broker = rt.spawn(move |mut ctx| async move {
            let me = ctx.pid();
            let mut subs: Vec<Pid> = Vec::new();
            loop {
                match ctx.recv().await {
                    Received::Message(m) => match m.first() {
                        Some(0) if m.len() >= 9 => {
                            let pid =
                                Pid::from_raw(u64::from_le_bytes(m[1..9].try_into().unwrap()));
                            broker_rt.monitor(me, pid); // prune it when it dies
                            subs.push(pid);
                        }
                        Some(1) => {
                            let payload = m[1..].to_vec();
                            for sub in &subs {
                                broker_rt.send(*sub, payload.clone());
                            }
                        }
                        Some(2) if m.len() >= 9 => {
                            let to = Pid::from_raw(u64::from_le_bytes(m[1..9].try_into().unwrap()));
                            broker_rt.send(to, vec![subs.len() as u8]);
                        }
                        _ => {}
                    },
                    // A subscriber exited (clean disconnect or crash) — prune it.
                    Received::Down { pid, .. } => subs.retain(|s| *s != pid),
                    _ => {}
                }
            }
        });
        assert!(rt.register("broker".to_string(), broker.pid()));

        // The pump is spawnable by name; the acceptor is a resident SSE server granted
        // exactly `spawn` (least privilege) — the pump inherits it (non-escalating).
        let pump = wr
            .prepare_component(&wr.compile_component(PUMP).unwrap(), "run")
            .unwrap();
        wr.register_component("sse-pump", pump);
        let acceptor = wr
            .prepare_component(&wr.compile_component(ACCEPTOR).unwrap(), "run")
            .unwrap();
        let server =
            wr.resident_http_server(&acceptor, Capabilities::nothing().allow_spawn(true), 1);
        let addr = serve_on(server).await;

        // Two concurrent SSE clients. Wait for each pump's `ready` (sent only after it
        // subscribes) so the publish can't race subscription — fully deterministic.
        let (mut a, mut b) = (open_sse(addr).await, open_sse(addr).await);
        let (mut sa, mut sb) = (String::new(), String::new());
        read_until(&mut a, &mut sa, "data: ready").await;
        read_until(&mut b, &mut sb, "data: ready").await;

        // One publish → both connections receive it live.
        let mut publish = vec![1u8];
        publish.extend_from_slice(b"hello");
        rt.send(broker.pid(), publish);

        read_until(&mut a, &mut sa, "data: hello").await;
        read_until(&mut b, &mut sb, "data: hello").await;
        assert!(
            sa.to_lowercase().contains("text/event-stream"),
            "A is an SSE stream"
        );
        assert!(
            sb.to_lowercase().contains("text/event-stream"),
            "B is an SSE stream"
        );

        // Cleanup (the second caveat, closed): disconnect A. The broker monitors its
        // subscribers, so pump A's exit — its next write fails once the socket is gone —
        // fires a `Down` that prunes it. The survivor keeps receiving; the count drops.
        assert_eq!(broker_count(&rt, broker.pid()).await, 2, "both subscribed");
        drop(a); // close A's socket
        let mut publish = vec![1u8];
        publish.extend_from_slice(b"p2");
        rt.send(broker.pid(), publish); // pump A's write now fails → it exits
        read_until(&mut b, &mut sb, "data: p2").await; // the survivor is unaffected

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let pruned = loop {
            if broker_count(&rt, broker.pid()).await == 1 {
                break true;
            }
            if tokio::time::Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert!(
            pruned,
            "the disconnected subscriber is pruned (broker monitor → Down)"
        );
    }
}
