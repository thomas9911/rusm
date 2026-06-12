//! HTTP serving ergonomics for the **per-request** model. The host resolves the
//! `[routes]` table, spawns the matched handler component fresh **per request**, and
//! dispatches the matched action here; `#[rusm_rs::handlers]` turns a module of
//! `fn action(Request, Params) -> Response` into that component. A 3-arg action
//! (`fn action(Request, Params, Sse)`) streams Server-Sent Events instead.
//!
//! The [`Request`]/[`Response`] types (and their base64 body encoding) are the shared
//! [`rusm_wire`] definitions the host speaks — re-exported here so guest code never
//! drifts from the host.

pub use rusm_wire::{Request, Response};

/// Build a `data: <payload>\n\n` SSE frame — the common case for an [`Sse`] event.
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

/// A live **Server-Sent Events** stream to the client, handed to a streaming action
/// (`fn action(Request, Params, Sse)`). Each request runs in its own process, so the
/// action may block here for the whole connection — write events as they occur, then
/// return (the stream closes on drop).
pub struct Sse {
    stream: Option<crate::Stream>,
}

impl Sse {
    /// Write a raw SSE frame (e.g. `b"data: hi\n\n"`). `false` once the client is gone.
    pub fn write(&self, frame: &[u8]) -> bool {
        self.stream.as_ref().is_some_and(|s| s.write(frame))
    }

    /// Write a `data: <payload>\n\n` event (a snapshot / one-off).
    pub fn data(&self, payload: &[u8]) -> bool {
        self.write(&data_frame(payload))
    }

    /// Live-tail until the client disconnects: each inbound message goes to `map`
    /// (return a frame to emit, or `None` to skip); an idle `heartbeat_ms` writes a
    /// heartbeat comment. Returns on disconnect — let the action then end so the
    /// process exits and a monitoring broker prunes this subscriber.
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

impl Drop for Sse {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            stream.close(); // signal end-of-stream to the client
        }
    }
}

/// What a dispatched action produces: a buffered [`Response`], or a streaming SSE body
/// the action pumps. The `#[rusm_rs::handlers]` macro builds this — a 2-arg action
/// `fn(Request, Params) -> Response` is [`Buffered`](Self::Buffered); a 3-arg action
/// `fn(Request, Params, Sse)` is [`Streamed`](Self::Streamed).
pub enum Outcome {
    /// A complete buffered response.
    Buffered(Response),
    /// A streaming `text/event-stream` body: the closure pumps events into the [`Sse`].
    Streamed(Box<dyn FnOnce(Sse)>),
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

/// Send the head reply `{ref, ok: response}` to the responder pid.
fn reply_head(to: crate::Pid, reference: u64, response: &Response) {
    let reply = serde_json::json!({ "ref": reference, "ok": response });
    crate::send_bytes(to, &serde_json::to_vec(&reply).expect("reply serializes"));
}

/// Receive the one request the host dispatched, route it to a handler via `dispatch`, and
/// reply. Handles exactly one request — process-per-request — then returns so the instance
/// exits. Called by the `#[rusm_rs::handlers]`-generated entrypoint; `dispatch` returns
/// `None` for an unknown action (→ 404, though the host's router makes that unreachable).
/// A [`Outcome::Streamed`] action replies a `text/event-stream` head, then pumps events
/// over a byte stream to the host responder (true SSE, the host's back-pressure carrying).
pub fn serve_request(dispatch: impl FnOnce(&str, Request, Params) -> Option<Outcome>) {
    let Ok(inc) = serde_json::from_slice::<Incoming>(&crate::receive_bytes()) else {
        return;
    };
    let reply_to = inc
        .from
        .as_deref()
        .and_then(|f| f.parse().ok())
        .map(crate::Pid)
        .zip(inc.reference);
    let outcome = dispatch(&inc.action, inc.request, Params(inc.params));
    let Some((to, reference)) = reply_to else {
        return; // a cast (no reply target) can't be answered
    };
    match outcome {
        Some(Outcome::Buffered(response)) => reply_head(to, reference, &response),
        None => reply_head(
            to,
            reference,
            &Response::new(404, b"no such action".to_vec()),
        ),
        Some(Outcome::Streamed(pump)) => {
            // Head first (a streamed `text/event-stream`), then the body rides a byte
            // stream to the responder — which the host drains directly into the chunked
            // HTTP body. The order matters: the responder expects the head, then the stream.
            let head = Response {
                status: 200,
                headers: vec![("content-type".into(), "text/event-stream".into())],
                body: Vec::new(),
                stream: true,
            };
            reply_head(to, reference, &head);
            pump(Sse {
                stream: crate::Stream::open(to),
            });
        }
    }
}
