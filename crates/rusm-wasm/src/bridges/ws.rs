//! Serving **WebSockets** (Phase 11). A WebSocket is only HTTP for its handshake;
//! after the `Upgrade` it's a raw bidirectional stream — and the handshake + the
//! protocol live entirely on the host, which RUSM controls. So WS never goes
//! through `wasi:http`: **hyper** surfaces the upgrade, **`tokio-tungstenite`** runs
//! the WS protocol (framing, ping/pong, close), and each connection is its own
//! supervised task — a failure drops only that socket, never the listener.
//!
//! Two entry points: [`serve_ws_echo`] is a host-side echo (the transport baseline);
//! [`WsServer`] runs an actual **WASM component process** per connection — each
//! inbound frame becomes one mailbox message, replies flow back through a Wasm-free
//! writer process that owns the socket sink. Wasmtime stays inside this crate; the
//! `rusm-otp` core never sees hyper, tungstenite, or `wasi:http`.

use std::convert::Infallible;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use http_body_util::Empty;
use hyper::body::Bytes;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use rusm_otp::{Pid, Runtime};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::bridges::resident::{ResidentPool, Shard};
use crate::caps::Capabilities;
use crate::{PreparedComponent, Spawner, WasmRuntime};

/// Serve a WebSocket **echo** on `listener` until it closes — one supervised task
/// per connection. Abort the task driving this to stop.
pub async fn serve_ws_echo(listener: TcpListener) {
    loop {
        let Ok((stream, _peer)) = listener.accept().await else {
            break;
        };
        stream.set_nodelay(true).ok();
        tokio::spawn(async move {
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(
                    TokioIo::new(stream),
                    hyper::service::service_fn(echo_upgrade),
                )
                // `with_upgrades` is what lets `hyper::upgrade::on` hand us the
                // raw stream after the 101.
                .with_upgrades()
                .await;
        });
    }
}

/// Answer the HTTP `Upgrade` with a 101 and spawn a host-side echo task. A request
/// without a WebSocket key gets a plain 426.
async fn echo_upgrade(
    req: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<Empty<Bytes>>, Infallible> {
    let Some(accept) = ws_accept(&req) else {
        return Ok(upgrade_required());
    };
    tokio::spawn(async move {
        let Some(mut ws) = upgraded_ws(req).await else {
            return;
        };
        while let Some(Ok(message)) = ws.next().await {
            if message.is_close() {
                break;
            }
            if (message.is_text() || message.is_binary()) && ws.send(message).await.is_err() {
                break;
            }
        }
    });
    Ok(switching_protocols(accept))
}

/// The `Sec-WebSocket-Accept` for a request, or `None` if it carries no WS key.
fn ws_accept(req: &hyper::Request<hyper::body::Incoming>) -> Option<String> {
    req.headers()
        .get("sec-websocket-key")
        .and_then(|k| k.to_str().ok())
        .map(|key| derive_accept_key(key.as_bytes()))
}

/// Complete the `Upgrade` and wrap the raw stream as a server-side `WebSocketStream`.
async fn upgraded_ws(
    req: hyper::Request<hyper::body::Incoming>,
) -> Option<WebSocketStream<TokioIo<Upgraded>>> {
    let upgraded = hyper::upgrade::on(req).await.ok()?;
    Some(WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, None).await)
}

fn switching_protocols(accept: String) -> hyper::Response<Empty<Bytes>> {
    hyper::Response::builder()
        .status(101)
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-accept", accept)
        .body(Empty::new())
        .unwrap()
}

fn upgrade_required() -> hyper::Response<Empty<Bytes>> {
    hyper::Response::builder()
        .status(426)
        .body(Empty::new())
        .unwrap()
}

/// No resident instance is currently available (e.g. all mid-restart) — refuse the
/// upgrade with a 503 rather than accept a socket nothing will serve.
fn service_unavailable() -> hyper::Response<Empty<Bytes>> {
    hyper::Response::builder()
        .status(503)
        .body(Empty::new())
        .unwrap()
}

/// Serves each WebSocket connection with a **WASM component process** — the actor
/// way. A connection's inbound messages land in the component's mailbox (one
/// message = one frame); its replies go to a per-connection **writer** process that
/// owns the socket sink. The component is pure sandboxed logic (no IO); the writer
/// and reader are Wasm-free `rusm-otp` glue. A handler crash drops only that
/// connection's processes — never the listener or other sockets.
#[derive(Clone)]
pub struct WsServer {
    prepared: PreparedComponent,
    /// `Some` when the handler is a **TS/JS bundle** on the shared js-runner: the
    /// bundle is sent as the runner's first message (its protocol), so the writer
    /// pid becomes the guest's *first* `Process.receive()`. `None` = a plain
    /// `rusm:runtime` component that gets the writer pid as message 1 directly.
    bundle: Option<Arc<Vec<u8>>>,
    spawner: Arc<Spawner>,
    caps: Capabilities,
}

impl WasmRuntime {
    /// Build a WebSocket server that runs `prepared` (a `rusm:runtime` actor
    /// component) as the handler process for each connection, under `caps`.
    pub fn ws_server(&self, prepared: &PreparedComponent, caps: Capabilities) -> WsServer {
        WsServer {
            prepared: prepared.clone(),
            bundle: None,
            spawner: Arc::clone(&self.spawner),
            caps,
        }
    }

    /// Build a WebSocket server whose per-connection handler is a **TypeScript/JS
    /// bundle** (Bun-built) running on the embedded js-runner — the TS twin of
    /// [`ws_server`](Self::ws_server). The guest is a worker (`export default`): its
    /// first `Process.receive()` is the writer pid, then each inbound frame.
    pub fn ws_server_js(&self, bundle: impl Into<Vec<u8>>, caps: Capabilities) -> WsServer {
        WsServer {
            prepared: self.js_runner().clone(),
            bundle: Some(Arc::new(bundle.into())),
            spawner: Arc::clone(&self.spawner),
            caps,
        }
    }
}

impl WsServer {
    /// Serve WebSockets on `listener` until it closes — one connection per task.
    pub async fn serve(self, listener: TcpListener) {
        loop {
            let Ok((stream, _peer)) = listener.accept().await else {
                break;
            };
            stream.set_nodelay(true).ok();
            let server = self.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |req| {
                    let server = server.clone();
                    async move { server.upgrade(req).await }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .with_upgrades()
                    .await;
            });
        }
    }

    async fn upgrade(
        &self,
        req: hyper::Request<hyper::body::Incoming>,
    ) -> Result<hyper::Response<Empty<Bytes>>, Infallible> {
        let Some(accept) = ws_accept(&req) else {
            return Ok(upgrade_required());
        };
        let server = self.clone();
        tokio::spawn(async move {
            if let Some(ws) = upgraded_ws(req).await {
                server.run_connection(ws).await;
            }
        });
        Ok(switching_protocols(accept))
    }

    /// Wire one upgraded connection to a fresh component process.
    async fn run_connection(&self, ws: WebSocketStream<TokioIo<Upgraded>>) {
        let (mut sink, mut stream) = ws.split();
        let rt = self.spawner.rt.clone();

        // Writer: a Wasm-free process owning the socket sink; it frames whatever the
        // component sends it. (Keeps all IO out of the sandboxed component.)
        let writer = rt.spawn(move |mut ctx| async move {
            while let Some(message) = ctx.recv().await.message() {
                if sink.send(Message::binary(message)).await.is_err() {
                    break;
                }
            }
        });

        // The sandboxed handler. For a JS bundle, the runner's first message is the
        // bundle itself; the writer pid then lands as the guest's first receive.
        let component = self
            .spawner
            .spawn_component(&self.prepared, self.caps.clone());
        if let Some(bundle) = &self.bundle {
            rt.send(component.pid(), bundle.as_ref().clone());
        }
        rt.send(component.pid(), writer.pid().raw().to_string().into_bytes());

        // Pump inbound frames into the component's mailbox (one message per frame).
        while let Some(Ok(message)) = stream.next().await {
            if message.is_close() {
                break;
            }
            if message.is_text() || message.is_binary() {
                rt.send(component.pid(), message.into_data().to_vec());
            }
        }

        // Connection done — tear down just this connection's processes.
        component.kill();
        writer.kill();
    }
}

/// Serves **all** WebSocket connections with a single (or a supervised pool of)
/// long-lived **resident** component process(es) that hold shared state — a chat
/// room, a pub/sub hub — vs [`WsServer`]'s one-process-per-connection. Each
/// connection still gets its own Wasm-free **writer** owning the socket sink; the
/// connection is identified to the resident by its writer pid (`conn`), and the
/// resident replies by sending bytes to that pid. A connection is pinned to its
/// instance for its lifetime; a crashed instance is restarted by the pool.
#[derive(Clone)]
pub struct ResidentWsServer {
    pool: ResidentPool,
    shard: Shard,
}

impl WasmRuntime {
    /// Build a resident WebSocket server: a supervised pool of `instances` (≥1)
    /// long-lived processes from `prepared` under `caps`, each serving many
    /// connections and holding shared state. The component's `run` should drive
    /// `rusm_rs::ws::serve`.
    pub fn resident_ws_server(
        &self,
        prepared: &PreparedComponent,
        caps: Capabilities,
        instances: usize,
    ) -> ResidentWsServer {
        ResidentWsServer {
            pool: ResidentPool::spawn(&self.spawner, prepared.clone(), caps, None, instances),
            shard: Shard::RoundRobin,
        }
    }

    /// The TS twin of [`resident_ws_server`](Self::resident_ws_server): the handler
    /// is a JS bundle on the persistent js-runner, signalled into WebSocket-serving
    /// mode (`export default { websocket: { open, message, close } }`) via the
    /// `RUSM_SERVE_ROLE=ws` capability.
    pub fn resident_ws_server_js(
        &self,
        bundle: impl Into<Vec<u8>>,
        caps: Capabilities,
        instances: usize,
    ) -> ResidentWsServer {
        let caps = caps.env("RUSM_SERVE_ROLE", "ws");
        let bundle = Arc::new(bundle.into());
        ResidentWsServer {
            pool: ResidentPool::spawn(
                &self.spawner,
                self.js_runner().clone(),
                caps,
                Some(bundle),
                instances,
            ),
            shard: Shard::RoundRobin,
        }
    }
}

impl ResidentWsServer {
    /// Route connections by a `shard_by` spec (`"header:<name>"` → handshake-header
    /// affinity; `None` → round-robin) so same-key connections land on one instance.
    pub fn shard_by(mut self, spec: Option<&str>) -> Self {
        self.shard = Shard::parse(spec);
        self
    }

    /// Serve WebSockets on `listener` until it closes — one task per connection,
    /// each routed to a resident instance.
    pub async fn serve(self, listener: TcpListener) {
        self.pool.ready().await; // don't accept until the pool is up
        loop {
            let Ok((stream, _peer)) = listener.accept().await else {
                break;
            };
            stream.set_nodelay(true).ok();
            let server = self.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |req| {
                    let server = server.clone();
                    async move { server.upgrade(req).await }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .with_upgrades()
                    .await;
            });
        }
    }

    async fn upgrade(
        &self,
        req: hyper::Request<hyper::body::Incoming>,
    ) -> Result<hyper::Response<Empty<Bytes>>, Infallible> {
        let Some(accept) = ws_accept(&req) else {
            return Ok(upgrade_required());
        };
        // Pin the connection to an instance at connect (sticky for its lifetime):
        // round-robin, or by a handshake header for session affinity.
        let resident = match &self.shard {
            Shard::RoundRobin => self.pool.route(),
            Shard::Header(name) => {
                let key = req
                    .headers()
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                self.pool.route_keyed(key)
            }
        };
        let Some(resident) = resident else {
            return Ok(service_unavailable());
        };
        let rt = self.pool.runtime().clone();
        tokio::spawn(async move {
            if let Some(ws) = upgraded_ws(req).await {
                run_resident_connection(rt, resident, ws).await;
            }
        });
        Ok(switching_protocols(accept))
    }
}

/// Wire one connection to a resident instance: a Wasm-free writer owns the sink and
/// is the connection's `conn`; lifecycle + frames become `open`/`message`/`close`
/// events the resident dispatches, replying by sending bytes to `conn`.
async fn run_resident_connection(
    rt: Runtime,
    resident: Pid,
    ws: WebSocketStream<TokioIo<Upgraded>>,
) {
    let (mut sink, mut stream) = ws.split();
    let writer = rt.spawn(move |mut ctx| async move {
        while let Some(message) = ctx.recv().await.message() {
            if sink.send(Message::binary(message)).await.is_err() {
                break;
            }
        }
    });
    let conn = writer.pid().raw().to_string();

    rt.send(
        resident,
        serde_json::to_vec(&serde_json::json!({ "op": "open", "conn": conn }))
            .expect("envelope serializes"),
    );
    while let Some(Ok(message)) = stream.next().await {
        if message.is_close() {
            break;
        }
        if message.is_text() || message.is_binary() {
            let data = message.into_data().to_vec();
            rt.send(
                resident,
                serde_json::to_vec(
                    &serde_json::json!({ "op": "message", "conn": conn, "data": data }),
                )
                .expect("envelope serializes"),
            );
        }
    }
    rt.send(
        resident,
        serde_json::to_vec(&serde_json::json!({ "op": "close", "conn": conn }))
            .expect("envelope serializes"),
    );
    writer.kill();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::Message;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn echoes_a_websocket_message() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_ws_echo(listener));

        let (mut ws, _resp) = tokio_tungstenite::connect_async(format!("ws://{addr}/"))
            .await
            .unwrap();
        ws.send(Message::text("hello ws")).await.unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        assert_eq!(&reply.into_data()[..], b"hello ws");

        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_wasm_component_handles_a_websocket() {
        use crate::{CapabilityProfile, WasmRuntime};
        use rusm_otp::Runtime;

        // The reply comes from a sandboxed WASM component (rs-ws-echo), not the host.
        const WS_ECHO: &[u8] = include_bytes!("../../tests/fixtures/rs_ws_echo.wasm");
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        let prepared = wr
            .prepare_component(&wr.compile_component(WS_ECHO).unwrap(), "run")
            .unwrap();
        let server = wr.ws_server(&prepared, CapabilityProfile::Trusted.capabilities());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(server.serve(listener));

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/"))
            .await
            .unwrap();
        ws.send(Message::text("hi component")).await.unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        assert_eq!(&reply.into_data()[..], b"hi component");

        handle.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_reclaims_every_held_process() {
        // The control USP: components parked on `receive` (here, handlers awaiting a
        // writer pid that never comes) must not leak — `shutdown` aborts them all and
        // frees their pooled instances, so a dropped engine never starves the next.
        use crate::{CapabilityProfile, WasmRuntime};
        use rusm_otp::Runtime;
        use std::time::Duration;

        const WS_ECHO: &[u8] = include_bytes!("../../tests/fixtures/rs_ws_echo.wasm");
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let prepared = wr
            .prepare_component(&wr.compile_component(WS_ECHO).unwrap(), "run")
            .unwrap();

        let n = 8u64;
        for _ in 0..n {
            // Drop the handle on purpose — the process stays parked (a leak, without
            // shutdown). Trusted just to keep the spawn unconditional.
            let _ = wr.spawn_component_with(&prepared, CapabilityProfile::Trusted.capabilities());
        }
        for _ in 0..200 {
            if rt.process_count() as u64 >= n {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            rt.process_count() as u64 >= n,
            "the parked handlers are alive"
        );

        assert!(
            wr.shutdown() as u64 >= n,
            "shutdown reports the processes it aborted"
        );
        for _ in 0..200 {
            if rt.process_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(rt.process_count(), 0, "shutdown reclaimed every process");
    }

    /// Connect, then read until `want` arrives (skipping anything else) — so a test
    /// isn't order-sensitive about an initial ack vs a later broadcast.
    async fn read_until(
        ws: &mut WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        want: &[u8],
    ) {
        use std::time::Duration;
        let deadline = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => panic!("timed out waiting for {:?}", String::from_utf8_lossy(want)),
                msg = ws.next() => {
                    let msg = msg.expect("stream open").expect("frame ok");
                    if msg.is_binary() || msg.is_text() {
                        if msg.into_data()[..] == *want { return; }
                    }
                }
            }
        }
    }

    async fn resident_ws_broadcast(server: ResidentWsServer) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(server.serve(listener));

        // Two connections to the SAME resident instance.
        let (mut a, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/"))
            .await
            .unwrap();
        let (mut b, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/"))
            .await
            .unwrap();
        // Each gets a "welcome" once the resident has registered it — so by the time
        // both are seen, both are members and a broadcast must reach both.
        read_until(&mut a, b"welcome").await;
        read_until(&mut b, b"welcome").await;

        // A frame from A is broadcast by the shared resident to BOTH connections.
        a.send(Message::text("ping")).await.unwrap();
        read_until(&mut a, b"ping").await;
        read_until(&mut b, b"ping").await;

        handle.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_websockets_share_one_resident_and_broadcast_rs() {
        use crate::{CapabilityProfile, WasmRuntime};
        use rusm_otp::Runtime;

        const WS: &[u8] = include_bytes!("../../tests/fixtures/rs_resident_ws.wasm");
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        let prepared = wr
            .prepare_component(&wr.compile_component(WS).unwrap(), "run")
            .unwrap();
        let server =
            wr.resident_ws_server(&prepared, CapabilityProfile::Sandboxed.capabilities(), 1);
        resident_ws_broadcast(server).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_websockets_share_one_resident_and_broadcast_ts() {
        use crate::{CapabilityProfile, WasmRuntime};
        use rusm_otp::Runtime;

        const TS_WS: &str = include_str!("../../tests/fixtures/ts_resident_ws.js");
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        let server = wr.resident_ws_server_js(
            TS_WS.as_bytes().to_vec(),
            CapabilityProfile::Sandboxed.capabilities(),
            1,
        );
        resident_ws_broadcast(server).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_typescript_component_handles_a_websocket() {
        use crate::{CapabilityProfile, WasmRuntime};
        use rusm_otp::Runtime;

        // The reply comes from a TypeScript worker (Bun-built) on the js-runner.
        const TS_WS_ECHO: &str = include_str!("../../tests/fixtures/ts_ws_echo.js");
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        let server = wr.ws_server_js(
            TS_WS_ECHO.as_bytes().to_vec(),
            CapabilityProfile::Trusted.capabilities(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(server.serve(listener));

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/"))
            .await
            .unwrap();
        ws.send(Message::text("hi from TS")).await.unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        assert_eq!(&reply.into_data()[..], b"hi from TS");

        handle.abort();
    }
}
