//! A per-request HTTP handler component: just `pub fn`s — no `main`, no router, no wire.
//! The host resolves the route and dispatches the matched action here. A 3-arg action
//! (taking `Sse`) streams Server-Sent Events.
use rusm_rs::http::{Params, Request, Response, Sse};

#[rusm_rs::handlers]
pub mod demo {
    use super::*;

    pub fn hello(_req: Request, p: Params) -> Response {
        Response::text(format!("hi {}\n", p.get("name").unwrap_or("world")))
    }

    pub fn echo(req: Request, _p: Params) -> Response {
        Response::new(200, req.body)
    }

    pub fn ticks(_req: Request, _p: Params, sse: Sse) {
        for n in 0..3 {
            if !sse.data(format!("tick {n}").as_bytes()) {
                break;
            }
        }
    }
}
