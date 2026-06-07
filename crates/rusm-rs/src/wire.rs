//! The RPC wire layer — the same JSON protocol rusm-ts uses, so a Rust client and
//! a TS service (or vice versa) interoperate: requests `{op, args, from, ref}`
//! (`stream: true` for a streaming call) → replies `{ref, ok}` | `{ref, err}`,
//! with `{op: "__cb", cbref, args}` callback messages. These back the generated
//! `#[service]` dispatch loop and the `#[client]` typed client.

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{me, receive_bytes, send_bytes, stash, Pid, Stream};

/// A decoded service request.
#[derive(serde::Deserialize)]
pub struct Request {
    pub op: String,
    #[serde(default)]
    pub args: serde_json::Value,
    pub from: Option<String>,
    #[serde(rename = "ref")]
    pub reference: Option<u64>,
    #[serde(default)]
    pub stream: bool,
}

impl Request {
    /// Deserialize the positional args as a tuple `T` (e.g. `(i64, i64)`).
    pub fn args<T: DeserializeOwned>(&self) -> Result<T, String> {
        serde_json::from_value(self.args.clone()).map_err(|e| e.to_string())
    }

    /// The caller's pid, if the request expects a reply.
    pub fn caller(&self) -> Option<Pid> {
        self.from.as_ref().and_then(|f| f.parse().ok()).map(Pid)
    }
}

/// Block for the next service request, skipping any malformed message.
pub fn next_request() -> Request {
    loop {
        if let Ok(req) = serde_json::from_slice::<Request>(&receive_bytes()) {
            return req;
        }
    }
}

/// Reply to a call with a value (a no-op for a cast — no `ref`).
pub fn reply_ok<T: Serialize>(req: &Request, value: &T) {
    if let (Some(reference), Some(to)) = (req.reference, req.caller()) {
        let msg = serde_json::json!({ "ref": reference, "ok": value });
        send_bytes(to, &serde_json::to_vec(&msg).expect("reply serializes"));
    }
}

/// Reply to a call with an error message.
pub fn reply_err(req: &Request, message: &str) {
    if let (Some(reference), Some(to)) = (req.reference, req.caller()) {
        let msg = serde_json::json!({ "ref": reference, "err": message });
        send_bytes(to, &serde_json::to_vec(&msg).expect("reply serializes"));
    }
}

fn next_ref() -> u64 {
    thread_local! {
        static REF: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    REF.with(|r| {
        let n = r.get() + 1;
        r.set(n);
        n
    })
}

fn send_request<A: Serialize>(to: Pid, op: &str, args: &A, reference: Option<u64>, stream: bool) {
    let mut req = serde_json::json!({ "op": op, "args": args, "from": me().0.to_string() });
    if let Some(r) = reference {
        req["ref"] = serde_json::json!(r);
    }
    if stream {
        req["stream"] = serde_json::json!(true);
    }
    send_bytes(to, &serde_json::to_vec(&req).expect("request serializes"));
}

/// A blocking **call**: send the request, then wait for the matching reply,
/// stashing any unrelated mail so the app's own `receive` still sees it. `args`
/// serializes as a JSON array (a tuple of the parameters).
pub fn call<A: Serialize, R: DeserializeOwned>(to: Pid, op: &str, args: &A) -> Result<R, String> {
    let reference = next_ref();
    send_request(to, op, args, Some(reference), false);
    loop {
        let raw = receive_bytes();
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else {
            stash(raw);
            continue;
        };
        if v.get("ref").and_then(serde_json::Value::as_u64) == Some(reference) {
            if let Some(err) = v.get("err").and_then(serde_json::Value::as_str) {
                return Err(err.to_string());
            }
            let ok = v.get("ok").cloned().unwrap_or(serde_json::Value::Null);
            return serde_json::from_value(ok).map_err(|e| e.to_string());
        }
        stash(raw); // not our reply — leave it for the app
    }
}

/// A **cast**: fire-and-forget (no reply awaited).
pub fn cast<A: Serialize>(to: Pid, op: &str, args: &A) {
    send_request(to, op, args, None, false);
}

/// Service side of a **streaming** call: open a stream back to the caller and
/// write each item (one JSON value per chunk), then close.
pub fn reply_stream<T, I>(req: &Request, items: I)
where
    T: Serialize,
    I: IntoIterator<Item = T>,
{
    let Some(to) = req.caller() else { return };
    let Some(stream) = Stream::open(to) else {
        return;
    };
    for item in items {
        if !stream.write(&serde_json::to_vec(&item).expect("chunk serializes")) {
            break; // reader gone
        }
    }
    stream.close();
}

/// Client side of a **streaming** call: send a stream request, accept the stream
/// the service opens, and yield each decoded chunk (blocking on each, EOF ends it).
pub fn call_stream<R: DeserializeOwned>(
    to: Pid,
    op: &str,
    args: &impl Serialize,
) -> impl Iterator<Item = R> {
    send_request(to, op, args, None, true);
    let stream = Stream::accept();
    std::iter::from_fn(move || {
        stream
            .read()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    })
}
