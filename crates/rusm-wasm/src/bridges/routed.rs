//! Per-request HTTP serving with the actor world — the unified serving model. For
//! each request the host resolves the route, spawns the matched handler component
//! **fresh** (process-per-request, so head-of-line blocking is impossible by
//! construction), dispatches the matched *action* over the `"fetch"` actor wire, and
//! turns the reply into the HTTP response. The handler component is just a module of
//! `fn action(Request, Params) -> Response` (see `#[rusm_rs::handlers]`); the routing
//! and the wire are entirely platform code.
//!
//! Contrast with the neighbours: [`super::http`] is the handler-less `wasi:http` path
//! (no actor world, no routing), and [`super::resident`] is one long-lived stateful
//! instance. This is the shape RUSM standardizes serving on — stateless, isolated,
//! routable — reusing resident's reply machinery ([`GatewayReply`], [`spawn_responder`],
//! the response builders).

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

use http_body_util::BodyExt;
use hyper::{Response, StatusCode};
use wasmtime_wasi_http::io::TokioIo;

use crate::bridges::resident::{
    build_response, build_streaming_response, error_response, spawn_responder, GatewayReply,
    ResBody,
};
use crate::caps::Capabilities;
use crate::{Spawner, WasmRuntime};

/// The host's per-request routing decision — the engine-local mirror of the manifest
/// route table, so `rusm-wasm` needn't depend on the config crate. `rusm-cli` bridges
/// `rusm_node::RouteTable::resolve` into a [`Resolver`] that yields this.
pub enum Routed {
    /// A route matched: dispatch `action` on `component` with these captured path params.
    Found {
        component: String,
        action: String,
        params: Vec<(String, String)>,
    },
    /// The path matched a route, but not for this method (HTTP 405).
    MethodNotAllowed,
    /// No route matched the path (HTTP 404).
    NotFound,
}

/// Resolves `(method, path)` to a [`Routed`] decision — supplied by the orchestrator
/// (it owns the manifest `[routes]` table; the engine stays routing-agnostic).
pub type Resolver = Arc<dyn Fn(&str, &str) -> Routed + Send + Sync>;

/// A per-request routed HTTP server: resolve the route, spawn the matched handler
/// fresh, dispatch the action, reply. Cheap to clone — one task per connection.
#[derive(Clone)]
pub struct RoutedHttpServer {
    spawner: Arc<Spawner>,
    resolve: Resolver,
    /// The capability profile to spawn each handler component under, by name.
    caps: Arc<HashMap<String, Capabilities>>,
}

impl WasmRuntime {
    /// Build a per-request routed HTTP server. `resolve` maps `(method, path)` to a
    /// [`Routed`] decision (the orchestrator bridges in the manifest `[routes]` table);
    /// `caps` gives the capability profile to spawn each handler component under, keyed
    /// by component name. The handler components must already be registered
    /// ([`register_component`](Self::register_component) /
    /// [`register_js_component`](Self::register_js_component)).
    pub fn routed_http_server(
        &self,
        resolve: Resolver,
        caps: HashMap<String, Capabilities>,
    ) -> RoutedHttpServer {
        RoutedHttpServer {
            spawner: self.spawner.clone(),
            resolve,
            caps: Arc::new(caps),
        }
    }
}

impl RoutedHttpServer {
    /// Serve HTTP/1.1 on `listener` until it closes — one task per connection. Abort
    /// the task driving this to stop.
    pub async fn serve(self, listener: tokio::net::TcpListener) {
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

    /// Resolve one request, spawn the matched handler fresh, dispatch the action over
    /// the `"fetch"` wire, and turn the reply into the response. Always `Ok` — every
    /// failure becomes a status code.
    async fn handle(
        &self,
        req: hyper::Request<hyper::body::Incoming>,
    ) -> Result<Response<ResBody>, Infallible> {
        let (parts, body) = req.into_parts();
        let target = parts
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");
        let (component, action, params) = match (self.resolve)(parts.method.as_str(), target) {
            Routed::Found {
                component,
                action,
                params,
            } => (component, action, params),
            Routed::MethodNotAllowed => {
                return Ok(error_response(
                    StatusCode::METHOD_NOT_ALLOWED,
                    "method not allowed",
                ))
            }
            Routed::NotFound => return Ok(error_response(StatusCode::NOT_FOUND, "not found")),
        };

        // The matched component must be registered and have a capability profile; a
        // mismatch is a manifest error, so 500 (the orchestrator validates up front).
        let Some(entry) = self.spawner.lookup(&component) else {
            return Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("route names unregistered component `{component}`"),
            ));
        };
        let Some(caps) = self.caps.get(&component).cloned() else {
            return Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("no capability profile for `{component}`"),
            ));
        };

        let method = parts.method.as_str().to_string();
        let url = target.to_string();
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
        let request = rusm_wire::Request {
            method,
            url,
            headers,
            body,
        };

        // Process-per-request: a fresh instance handles this one request, then exits.
        let child = self.spawner.spawn_component(&entry.prepared, caps);
        let child_pid = child.pid();
        // A TS handler carries its bundle as message 1 (the js-runner's protocol).
        if let Some(bundle) = &entry.bundle {
            self.spawner.rt.send(child_pid, (**bundle).clone());
        }
        // An ephemeral responder owns the oneshot and turns the reply into the response;
        // the handler sends exactly one reply to it, so no ref-matching is needed here.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let responder = spawn_responder(&self.spawner.rt, tx);
        let envelope = serde_json::json!({
            "op": "fetch",
            "ref": 0u64,
            "from": responder.pid().raw().to_string(),
            "action": action,
            "params": params,
            "request": request,
        });
        self.spawner.rt.send(
            child_pid,
            serde_json::to_vec(&envelope).expect("envelope serializes"),
        );

        Ok(match rx.await {
            Ok(GatewayReply::Buffered(resp)) => build_response(resp),
            Ok(GatewayReply::Streaming {
                status,
                headers,
                handle,
            }) => build_streaming_response(status, headers, handle),
            Ok(GatewayReply::Err(message)) => {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, &message)
            }
            Err(_) => error_response(StatusCode::BAD_GATEWAY, "handler did not reply"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CapabilityProfile;
    use rusm_node::{Resolution, RouteTable};
    use rusm_otp::Runtime;
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // A `#[rusm_rs::handlers] pub mod demo` with `fn hello(_, params)` (→ "hi <name>")
    // and `fn echo(req, _)` (→ the request body). See `tests/fixtures/handlers-demo`.
    const HANDLERS: &[u8] = include_bytes!("../../tests/fixtures/rs_handlers_demo.wasm");

    /// The exact bridge `rusm-cli` builds: the manifest [`RouteTable`] → a [`Resolver`].
    fn resolver(table: RouteTable) -> Resolver {
        Arc::new(
            move |method: &str, path: &str| match table.resolve(method, path) {
                Resolution::Found {
                    component,
                    action,
                    params,
                } => Routed::Found {
                    component,
                    action,
                    params,
                },
                Resolution::MethodNotAllowed => Routed::MethodNotAllowed,
                Resolution::NotFound => Routed::NotFound,
            },
        )
    }

    async fn serve_on(server: RoutedHttpServer) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(server.serve(listener));
        addr
    }

    /// One raw HTTP/1.1 request (Connection: close) → the full response text.
    async fn request(addr: SocketAddr, method: &str, path: &str, body: &str) -> String {
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: rusm\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let mut conn = tokio::net::TcpStream::connect(addr).await.unwrap();
        conn.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        conn.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatches_each_request_by_route_to_a_freshly_spawned_handler() {
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        let prepared = wr
            .prepare_component(&wr.compile_component(HANDLERS).unwrap(), "run")
            .unwrap();
        wr.register_component("demo", prepared);

        let table = RouteTable::from_map(&HashMap::from([
            ("GET /hello/:name".to_string(), "demo#hello".to_string()),
            ("POST /echo".to_string(), "demo#echo".to_string()),
            ("GET /ticks".to_string(), "demo#ticks".to_string()),
        ]))
        .unwrap();
        let caps = HashMap::from([(
            "demo".to_string(),
            CapabilityProfile::Sandboxed.capabilities(),
        )]);
        let addr = serve_on(wr.routed_http_server(resolver(table), caps)).await;

        // A matched route dispatches the named action with the captured path param.
        let hello = request(addr, "GET", "/hello/alice", "").await;
        assert!(hello.starts_with("HTTP/1.1 200"), "got: {hello}");
        assert!(hello.contains("hi alice"), "param dispatched: {hello}");

        // A different action on the same component, carrying the request body.
        let echo = request(addr, "POST", "/echo", "ping").await;
        assert!(echo.starts_with("HTTP/1.1 200"), "got: {echo}");
        assert!(echo.trim_end().ends_with("ping"), "echo body: {echo}");

        // Each request is a fresh instance: the second `/hello` is independent of the
        // first (no shared state to leak), and still resolves correctly.
        let again = request(addr, "GET", "/hello/bob", "").await;
        assert!(
            again.contains("hi bob"),
            "fresh instance per request: {again}"
        );

        // A 3-arg (`Sse`) action streams a chunked text/event-stream body.
        let ticks = request(addr, "GET", "/ticks", "").await;
        let lower = ticks.to_lowercase();
        assert!(ticks.starts_with("HTTP/1.1 200"), "got: {ticks}");
        assert!(
            lower.contains("text/event-stream") && lower.contains("transfer-encoding: chunked"),
            "streamed SSE body: {ticks}"
        );
        for n in 0..3 {
            assert!(
                ticks.contains(&format!("data: tick {n}")),
                "event {n}: {ticks}"
            );
        }

        // Unmatched path → 404; matched path, wrong method → 405.
        assert!(
            request(addr, "GET", "/nope", "")
                .await
                .starts_with("HTTP/1.1 404"),
            "unmatched path is 404"
        );
        assert!(
            request(addr, "DELETE", "/echo", "")
                .await
                .starts_with("HTTP/1.1 405"),
            "matched path + wrong method is 405"
        );
    }
}
