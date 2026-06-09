//! Ergonomics for a **resident** HTTP handler: implement [`Handler`] over your own
//! state and [`serve`] it. Unlike the per-request `wasi:http` path (a fresh
//! instance per request), a resident handler is one long-lived process — so
//! `&mut self` state persists across requests. The host gateway turns each HTTP
//! request into a `"fetch"` request on the actor wire and turns your [`Response`]
//! back into the HTTP response; you just write `handle`.

use serde::{Deserialize, Serialize};

use crate::wire;

/// An incoming HTTP request, as delivered to a resident [`Handler`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Request {
    /// The HTTP method (`GET`, `POST`, …).
    pub method: String,
    /// The request target — path and query (e.g. `/items?q=1`).
    pub url: String,
    /// Header name/value pairs, in arrival order.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// The raw request body.
    #[serde(default)]
    pub body: Vec<u8>,
}

/// The response a [`Handler`] returns for a [`Request`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Response {
    /// HTTP status code.
    pub status: u16,
    /// Header name/value pairs.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// The response body.
    #[serde(default)]
    pub body: Vec<u8>,
}

impl Response {
    /// A `200 OK` `text/plain` response.
    pub fn text(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
            body: body.into().into_bytes(),
        }
    }

    /// A response with an explicit status and raw body (no default headers).
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    /// Adds a header, builder-style.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// A resident HTTP handler. The receiver is `&mut self`, so the implementor owns
/// state that lives across requests (a counter, a cache, sessions, …).
pub trait Handler {
    /// Handle one request and produce a response. Keep this fast: a resident
    /// handler serves requests one at a time, so offload slow work to a spawned
    /// helper rather than blocking here.
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
