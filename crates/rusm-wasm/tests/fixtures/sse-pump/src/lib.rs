//! A per-connection **SSE pump** (one process per connection — the offload target
//! of `sse-acceptor`). It:
//!   1. receives the responder pid (the connection's stream target),
//!   2. subscribes to the `broker` with its own pid,
//!   3. writes a `data: ready` frame so a test can publish only once every
//!      subscriber is registered (deterministic, no sleep race),
//!   4. live-tails: each broadcast event becomes a `data:` frame; an idle timeout
//!      writes a heartbeat comment (via `receive-timeout`). Stops when the client
//!      disconnects (a stream write returns `false`).
//! This is the live 1-publisher → N-subscriber SSE fan-out, built from primitives.

wit_bindgen::generate!({
    world: "process",
    path: "wit",
    with: { "rusm:runtime/actor@0.1.0": rusm_rs::rusm::runtime::actor },
});

/// Heartbeat cadence: a comment frame keeps the connection alive when idle.
const HEARTBEAT_MS: u64 = 1_000;
/// Broker op: subscribe this pid as a fan-out target.
const OP_SUBSCRIBE: u8 = 0;

struct Component;

impl Guest for Component {
    fn run() {
        // Accept the offloaded connection, subscribe to the broker, announce readiness
        // (so a test can publish only after every subscriber is registered), then let
        // the SDK live-tail: each broadcast event → a `data:` frame, heartbeat on idle,
        // exit on disconnect (the broker prunes us via its monitor).
        let conn = rusm_rs::http::SseConnection::accept();
        let broker = rusm_rs::whereis("broker").expect("broker is registered");
        let mut subscribe = vec![OP_SUBSCRIBE];
        subscribe.extend_from_slice(&rusm_rs::me().0.to_le_bytes());
        rusm_rs::send_bytes(broker, &subscribe);
        conn.data(b"ready");
        conn.run(HEARTBEAT_MS, |event| Some(rusm_rs::http::data_frame(&event)));
    }
}

export!(Component);
