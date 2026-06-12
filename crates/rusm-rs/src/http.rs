//! Ergonomics for a **resident** HTTP handler: implement [`Handler`] over your own
//! state and [`serve`] it. Unlike the per-request `wasi:http` path (a fresh instance
//! per request), a resident handler is one long-lived process — so `&mut self` state
//! persists across requests. The host gateway turns each HTTP request into a
//! `"fetch"` request on the actor wire and turns your [`Response`] back into the HTTP
//! response; you just write `handle`.
//!
//! The [`Request`]/[`Response`] types (and their base64 body encoding) are the shared
//! [`rusm_wire`] definitions the host speaks — re-exported here so guest code never
//! drifts from the host.

pub use rusm_wire::{Request, Response};

use crate::wire;

/// A resident HTTP handler. The receiver is `&mut self`, so the implementor owns
/// state that lives across requests (a counter, a cache, sessions, …).
pub trait Handler {
    /// Handle one request and produce a response. Keep this fast: a resident handler
    /// serves requests one at a time, so offload slow work to a spawned helper rather
    /// than blocking here.
    fn handle(&mut self, request: Request) -> Response;
}

/// Runs `handler` as the resident serving loop: receive each `"fetch"` request from
/// the host gateway, dispatch it to [`Handler::handle`], and reply. Never returns —
/// call it from a component's `run`.
pub fn serve<H: Handler>(mut handler: H) -> ! {
    loop {
        let req = wire::next_request();
        if req.op == "fetch" {
            match wire::arg::<Request>(&req, 0) {
                Ok(request) => {
                    let response = handler.handle(request);
                    wire::reply_ok(&req, &response);
                }
                Err(err) => wire::reply_err(&req, &err),
            }
        } else {
            wire::reply_err(&req, &format!("unsupported op: {}", req.op));
        }
    }
}

/// Runs a resident **Server-Sent Events** handler: for each request, `handler` yields
/// the event chunks (each already a full SSE event, e.g. `b"data: hi\n\n"`). The
/// response is a streamed `text/event-stream` body — each chunk is flushed as it's
/// produced, with the byte stream's natural back-pressure. `handler` is `FnMut`, so
/// it can carry state across requests.
///
/// A resident serves one request at a time; a long-lived stream blocks the instance,
/// so an endless SSE feed should run in a process spawned per request that streams to
/// the caller, leaving the resident's loop free.
pub fn serve_sse<F, I>(mut handler: F) -> !
where
    F: FnMut(Request) -> I,
    I: IntoIterator<Item = Vec<u8>>,
{
    loop {
        let req = wire::next_request();
        if req.op != "fetch" {
            wire::reply_err(&req, &format!("unsupported op: {}", req.op));
            continue;
        }
        let request = match wire::arg::<Request>(&req, 0) {
            Ok(request) => request,
            Err(err) => {
                wire::reply_err(&req, &err);
                continue;
            }
        };
        let Some(caller) = req.caller() else {
            continue; // a cast can't receive a streamed body
        };
        // Reply with a streamed head; the body then rides a byte stream to the caller.
        let head = Response {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            body: Vec::new(),
            stream: true,
        };
        wire::reply_ok(&req, &head);
        if let Some(stream) = crate::Stream::open(caller) {
            for chunk in handler(request) {
                if !stream.write(&chunk) {
                    break; // the client (host body) hung up
                }
            }
            stream.close();
        }
    }
}

// ── Offloaded SSE: endless feeds & live fan-out ───────────────────────────────
//
// [`serve_sse`] pumps inline, so the resident serving it is busy for that one
// connection's whole lifetime — fine for a short finite stream, wrong for an
// endless feed (it would serve one client at a time). The offloaded pattern fixes
// that: a resident **acceptor** ([`serve_sse_offloaded`]) replies the SSE head and
// hands each connection to a freshly-spawned **pump** process, then loops on —
// never head-of-line blocked. Each pump owns one connection ([`SseConnection`]):
// it subscribes to an app event source, live-tails it to the client, heartbeats on
// idle, and exits on disconnect — a broker that `monitor`s its subscribers prunes
// it on the resulting `Down` (the crash-safe OTP cleanup, no unsubscribe needed).

/// The acceptor side of offloaded SSE: for each connection reply a streaming
/// `text/event-stream` head, then **offload** the feed to a freshly-spawned
/// `pump_component` (a registered component whose `run` drives an [`SseConnection`]),
/// and loop on — the acceptor is never head-of-line blocked, so one instance serves
/// many concurrent live streams. Requires the `spawn` capability; the pump inherits
/// the acceptor's capabilities (non-escalating). Never returns — call it from `run`.
pub fn serve_sse_offloaded(pump_component: &str) -> ! {
    loop {
        let req = wire::next_request();
        if req.op != "fetch" {
            wire::reply_err(&req, &format!("unsupported op: {}", req.op));
            continue;
        }
        let Some(responder) = req.caller() else {
            continue; // a cast can't receive a streamed body
        };
        // Head first, so it reaches the responder before the pump's byte stream
        // (the responder expects the head message, then the stream).
        let head = Response {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            body: Vec::new(),
            stream: true,
        };
        wire::reply_ok(&req, &head);
        if let Ok(pump) = crate::spawn(pump_component) {
            crate::send_bytes(pump, &responder.0.to_le_bytes());
        }
    }
}

/// One accepted SSE connection, held by a pump process: write framed events to the
/// client until it disconnects. Pairs with [`serve_sse_offloaded`].
pub struct SseConnection {
    stream: crate::Stream,
}

impl SseConnection {
    /// Accept the connection handed over by [`serve_sse_offloaded`] — the first
    /// message is the responder pid; open the outbound stream to it. Call once,
    /// first, in the pump component's `run` (before subscribing / sending a snapshot).
    pub fn accept() -> Self {
        let msg = crate::receive_bytes();
        let raw = msg
            .get(..8)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 8]);
        let responder = crate::Pid(u64::from_le_bytes(raw));
        let stream = crate::Stream::open(responder).expect("responder stream is open");
        Self { stream }
    }

    /// Write a raw SSE frame (e.g. `b"data: hi\n\n"`). `false` once the client is gone.
    pub fn write(&self, frame: &[u8]) -> bool {
        self.stream.write(frame)
    }

    /// Write a `data:` event carrying `payload` (a snapshot / one-off event).
    pub fn data(&self, payload: &[u8]) -> bool {
        self.write(&data_frame(payload))
    }

    /// Live-tail until the client disconnects: each inbound message goes to `map`
    /// (return a frame to emit, or `None` to skip it); an idle `heartbeat_ms` writes
    /// a heartbeat comment. Returns on disconnect — let the pump's `run` then end so
    /// the process exits and a monitoring broker prunes this subscriber.
    pub fn run(self, heartbeat_ms: u64, mut map: impl FnMut(Vec<u8>) -> Option<Vec<u8>>) {
        loop {
            match crate::receive_bytes_timeout(heartbeat_ms) {
                Some(msg) => {
                    if let Some(frame) = map(msg) {
                        if !self.write(&frame) {
                            return;
                        }
                    }
                }
                None => {
                    if !self.write(b": ping\n\n") {
                        return;
                    }
                }
            }
        }
    }
}

/// Build a `data: <payload>\n\n` SSE frame — the common case for [`SseConnection::run`]'s
/// `map` (`|msg| Some(data_frame(&msg))`).
pub fn data_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + b"data: \n\n".len());
    frame.extend_from_slice(b"data: ");
    frame.extend_from_slice(payload);
    frame.extend_from_slice(b"\n\n");
    frame
}

// ── Per-request handlers: the unified serving model ──────────────────────────
//
// The host resolves the `[routes]` table, spawns this component fresh **per request**, and
// sends one `"fetch"` carrying the matched action, the captured path params, and the
// request. `#[rusm_rs::handlers]` dispatches it to the named handler function and replies;
// then the instance exits. All of this is *platform* code — an app author writes only
// `fn action(Request, Params) -> Response`, never the routing or the wire.

/// Path parameters captured from the route pattern (`/users/:id` → `params.get("id")`).
pub struct Params(Vec<(String, String)>);

impl Params {
    /// The value captured for `name`, or `None` if the route had no such parameter.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// The wire the host sends a per-request handler: the matched `action`, captured path
/// `params`, the `request`, and the reply target (`from`/`ref`).
#[derive(serde::Deserialize)]
struct Incoming {
    action: String,
    #[serde(default)]
    params: Vec<(String, String)>,
    from: Option<String>,
    #[serde(rename = "ref")]
    reference: Option<u64>,
    request: Request,
}

/// Receive the one request the host dispatched, route it to a handler via `dispatch`, and
/// reply. Handles exactly one request — process-per-request — then returns so the instance
/// exits. Called by the `#[rusm_rs::handlers]`-generated entrypoint; `dispatch` returns
/// `None` for an unknown action (→ 404, though the host's router makes that unreachable).
pub fn serve_request(dispatch: impl FnOnce(&str, Request, Params) -> Option<Response>) {
    let Ok(inc) = serde_json::from_slice::<Incoming>(&crate::receive_bytes()) else {
        return;
    };
    let response = dispatch(&inc.action, inc.request, Params(inc.params))
        .unwrap_or_else(|| Response::new(404, b"no such action".to_vec()));
    if let (Some(to), Some(reference)) = (inc.from.and_then(|f| f.parse().ok()), inc.reference) {
        let reply = serde_json::json!({ "ref": reference, "ok": response });
        crate::send_bytes(
            crate::Pid(to),
            &serde_json::to_vec(&reply).expect("reply serializes"),
        );
    }
}
