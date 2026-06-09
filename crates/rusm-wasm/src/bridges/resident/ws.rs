//! The **resident** WebSocket gateway (see [`super`]): one long-lived instance (or a
//! supervised pool) serves *all* connections and holds shared state — a chat room, a
//! pub/sub hub — vs [`super::super::ws::WsServer`]'s one-process-per-connection. It
//! reuses the same [`ResidentRoute`] as the HTTP gateway, and the WS handshake
//! helpers from [`super::super::ws`].

use std::convert::Infallible;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use http_body_util::Empty;
use hyper::body::Bytes;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use rusm_otp::Runtime;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use super::{Lease, ResidentPool, ResidentRoute};
use crate::bridges::ws::{switching_protocols, upgrade_required, upgraded_ws, ws_accept};
use crate::caps::Capabilities;
use crate::{PreparedComponent, WasmRuntime};

/// Serves **all** WebSocket connections with a single (or a supervised pool of)
/// long-lived **resident** component process(es) that hold shared state. Each
/// connection still gets its own Wasm-free **writer** owning the socket sink; the
/// connection is identified to the resident by its writer pid (`conn`), and the
/// resident replies by sending bytes to that pid. A connection is pinned to its
/// instance for its lifetime; a crashed instance is restarted by the pool.
#[derive(Clone)]
pub struct ResidentWsServer {
    route: ResidentRoute,
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
        let pool = ResidentPool::spawn(&self.spawner, prepared.clone(), caps, None, instances);
        ResidentWsServer {
            route: ResidentRoute::new(pool),
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
        let pool = ResidentPool::spawn(
            &self.spawner,
            self.js_runner().clone(),
            caps,
            Some(bundle),
            instances,
        );
        ResidentWsServer {
            route: ResidentRoute::new(pool),
        }
    }
}

impl ResidentWsServer {
    /// Route connections by a `shard_by` spec (`"header:<name>"` → handshake-header
    /// affinity; `None` → round-robin) so same-key connections land on one instance.
    pub fn shard_by(mut self, spec: Option<&str>) -> Self {
        self.route.shard_by(spec);
        self
    }

    /// Bound concurrent connections per instance to `limit`; excess refuses the
    /// upgrade with 503 — the WS twin of
    /// [`ResidentHttpServer::max_inflight`](super::ResidentHttpServer::max_inflight).
    pub fn max_inflight(mut self, limit: usize) -> Self {
        self.route.max_inflight(limit);
        self
    }

    /// Serve WebSockets on `listener` until it closes — one task per connection,
    /// each routed to a resident instance.
    pub async fn serve(self, listener: TcpListener) {
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
        // Pin the connection to an instance at connect (sticky for its lifetime), and
        // take its in-flight permit — held by the lease for the connection's life.
        let Some(lease) = self.route.route(req.headers()) else {
            return Ok(service_unavailable());
        };
        let rt = self.route.runtime().clone();
        tokio::spawn(async move {
            if let Some(ws) = upgraded_ws(req).await {
                run_resident_connection(rt, lease, ws).await;
            }
        });
        Ok(switching_protocols(accept))
    }
}

/// No resident instance is currently available (e.g. all mid-restart / saturated) —
/// refuse the upgrade with a 503 rather than accept a socket nothing will serve.
fn service_unavailable() -> hyper::Response<Empty<Bytes>> {
    hyper::Response::builder()
        .status(503)
        .body(Empty::new())
        .unwrap()
}

/// Wire one connection to a resident instance: a Wasm-free writer owns the sink and
/// is the connection's `conn`; lifecycle + frames become `open`/`message`/`close`
/// events the resident dispatches, replying by sending bytes to `conn`. The `lease`
/// is held for the whole connection — its in-flight permit frees on disconnect.
async fn run_resident_connection(
    rt: Runtime,
    lease: Lease,
    ws: WebSocketStream<TokioIo<Upgraded>>,
) {
    let resident = lease.pid;
    let (mut sink, mut stream) = ws.split();
    let writer = rt.spawn(move |mut ctx| async move {
        while let Some(message) = ctx.recv().await.message() {
            if sink.send(Message::binary(message)).await.is_err() {
                break;
            }
        }
    });
    let conn = writer.pid().raw();

    rt.send(resident, ws_event(WS_OP_OPEN, conn, &[]));
    while let Some(Ok(message)) = stream.next().await {
        if message.is_close() {
            break;
        }
        if message.is_text() || message.is_binary() {
            let data = message.into_data();
            rt.send(resident, ws_event(WS_OP_MESSAGE, conn, data.as_ref()));
        }
    }
    rt.send(resident, ws_event(WS_OP_CLOSE, conn, &[]));
    writer.kill();
}

// Resident WS event opcodes (the binary wire, host → resident instance).
const WS_OP_OPEN: u8 = 0;
const WS_OP_MESSAGE: u8 = 1;
const WS_OP_CLOSE: u8 = 2;

/// Encode one resident WebSocket event as a compact **binary** frame —
/// `[op: u8][conn: u64 LE][data…]` — instead of a JSON envelope whose `data` would
/// serialize as a per-byte number array. The guest reads `conn` and the raw payload
/// directly (a zero-copy `subarray`), with no per-frame `JSON.parse`/array rebuild.
fn ws_event(op: u8, conn: u64, data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(9 + data.len());
    buf.push(op);
    buf.extend_from_slice(&conn.to_le_bytes());
    buf.extend_from_slice(data);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CapabilityProfile;
    use rusm_otp::Runtime;

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
                    if (msg.is_binary() || msg.is_text()) && msg.into_data()[..] == *want {
                        return;
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
        const WS: &[u8] = include_bytes!("../../../tests/fixtures/rs_resident_ws.wasm");
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
        const TS_WS: &str = include_str!("../../../tests/fixtures/ts_resident_ws.js");
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        let server = wr.resident_ws_server_js(
            TS_WS.as_bytes().to_vec(),
            CapabilityProfile::Sandboxed.capabilities(),
            1,
        );
        resident_ws_broadcast(server).await;
    }
}
