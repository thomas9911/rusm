# Lifecycle — HTTP component

A per-request HTTP handler: a `#[rusm_rs::handlers]` action (Rust) or a `wasi:http`
`export default { fetch }` (TypeScript). **A fresh sandboxed instance per request**,
which handles exactly one request and exits. See the
[overview](./component-lifecycle.md) for the shared two-domain model and failure
vocabulary this chapter builds on.

## Shape (what you write)

```rust
use rusm_rs::http::{Params, Request, Response};

#[rusm_rs::handlers]
pub mod api {
    use super::*;
    // routed from `[serve.routes]`:  "GET /users/:id" = "api#show"
    pub fn show(_req: Request, p: Params) -> Response {
        Response::text(format!("user {}", p.get("id").unwrap_or("?")))
    }
}
```

That's the **application domain** in full — a function from `Request` + path `Params`
to a `Response`. No `main`, no router, no request/response wire.

## Platform owns / you write

- **Platform owns:** accepting the connection, parsing HTTP, resolving the route from
  this listener's `[serve.routes]`, spawning the handler fresh, sending the request over
  the actor wire, the ephemeral reply *responder*, writing the HTTP response, and
  reclaiming the instance.
- **You write:** `fn action(Request, Params) -> Response`.

## Lifecycle events

| Event | Platform domain | Application domain | Result |
| --- | --- | --- | --- |
| **Normal** | route → spawn fresh → dispatch → build the HTTP response → reclaim the instance | the action returns a `Response` | the response; the instance is gone |
| **Error status** (e.g. `Response::new(400, …)`) | writes it verbatim | chose to return it | that status — **not** a failure |
| **No route / wrong method** | answers **404** / **405** | (never spawned) | 404 / 405 |
| **Unregistered component** in a route | answers **500** | (never spawned) | 500 — a manifest error |
| **Connection error** (unreadable body, malformed request line) | answers **400**, or drops the malformed connection, *before* dispatch | (usually never spawned) | 400 / dropped |
| **Client disconnect** before the reply | the response write fails; the connection is dropped | the action still ran to completion — it can't tell | nothing sent; **no crash** |
| **Crash (trap)** | the responder's reply channel drops → answers **502** → the process is Crashed and reclaimed | the `panic!` / `.unwrap()` / `unreachable` | **502**; only this request affected |
| **Memory crash (OOM)** | the `StoreLimiter` cap trips a trap → handled like any crash → **502** | exceeded `max-memory-mb` | **502**; the instance is discarded |

## Notes

- **Not supervised — and that's correct.** There is nothing to restart: the next
  request gets a brand-new instance regardless. A crash or OOM is contained to one
  request; the listener and every other in-flight request are untouched (each is its
  own instance with its own memory).
- **Where state goes.** A handler is stateless and disposable. Cross-request state
  lives in a [service component](./lifecycle-service.md) (reached via `whereis` +
  `call`) or in durable `kv` — never in the handler.
- **TypeScript** uses the web-standard `export default function handle(req): Response`
  on the `wasi:http` path; same per-request lifecycle, no `[serve.routes]` needed (the
  component dispatches itself).

Next: [SSE component](./lifecycle-sse.md) · [WebSocket component](./lifecycle-websocket.md)
