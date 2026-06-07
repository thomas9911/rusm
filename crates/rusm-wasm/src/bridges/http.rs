//! Serving a component as an **HTTP handler** (Phase 11): host the standard
//! `wasi:http/incoming-handler` via hyper + `wasmtime-wasi-http`. One fresh,
//! sandboxed component instance **per request** — cheap on the pooled spawn path,
//! and a trap is just that one request failing. The response is produced **by the
//! guest** (RS via `wstd`, or TS via the js-runner's `fetch` shape); the host only
//! moves bytes.

use std::sync::Arc;

use anyhow::{bail, Result};
use wasmtime::component::Component;
use wasmtime::Store;
use wasmtime_wasi::ResourceTable;
use wasmtime_wasi_http::io::TokioIo;
use wasmtime_wasi_http::p2::bindings::http::types::Scheme;
use wasmtime_wasi_http::p2::bindings::ProxyPre;
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use wasmtime_wasi_http::p2::WasiHttpView;
use wasmtime_wasi_http::WasiHttpCtx;

use super::WasiHost;
use crate::caps::Capabilities;
use crate::{Spawner, WasmRuntime};

/// A `wasi:http` component with its imports resolved and pre-instantiated — the
/// fast path for per-request instantiation.
#[derive(Clone)]
pub struct PreparedHttp {
    pre: ProxyPre<WasiHost>,
}

/// A ready-to-run HTTP server: a prepared component, the spawn core (engine +
/// runtime), and the capability profile each request instance runs under. Cheap to
/// clone (all `Arc`-backed), so it spawns one task per connection.
#[derive(Clone)]
pub struct HttpServer {
    pre: ProxyPre<WasiHost>,
    spawner: Arc<Spawner>,
    caps: Capabilities,
}

impl WasmRuntime {
    /// Prepare a `wasi:http` (proxy) component for serving.
    pub fn prepare_http(&self, component: &Component) -> Result<PreparedHttp> {
        let pre = ProxyPre::new(self.component_linker.instantiate_pre(component)?)?;
        Ok(PreparedHttp { pre })
    }

    /// Build a server that runs each request on a fresh instance under `caps`.
    pub fn http_server(&self, prepared: &PreparedHttp, caps: Capabilities) -> HttpServer {
        HttpServer {
            pre: prepared.pre.clone(),
            spawner: Arc::clone(&self.spawner),
            caps,
        }
    }
}

impl HttpServer {
    /// Serve HTTP/1.1 on `listener` until it closes (one connection per task, one
    /// component instance per request). Abort the task driving this to stop.
    pub async fn serve(self, listener: tokio::net::TcpListener) {
        loop {
            let Ok((stream, _peer)) = listener.accept().await else {
                break;
            };
            stream.set_nodelay(true).ok(); // low request latency, no Nagle batching
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

    /// Run one request through a fresh component instance and return its response.
    async fn handle(
        &self,
        req: hyper::Request<hyper::body::Incoming>,
    ) -> Result<hyper::Response<HyperOutgoingBody>> {
        let host = WasiHost {
            wasi: self.caps.build_wasi()?,
            table: ResourceTable::new(),
            http: WasiHttpCtx::new(),
            pid: 0,
            caps: self.caps.clone(),
            rt: self.spawner.rt.clone(),
            ctx: None,
            spawner: Some(Arc::clone(&self.spawner)),
            out_streams: Default::default(),
            in_streams: Default::default(),
            next_stream: 0,
        };
        let mut store = Store::new(self.pre.engine(), host);
        store.limiter(|host| host as &mut dyn wasmtime::ResourceLimiter);
        // Epoch preemption applies to request handlers too — a runaway guest yields.
        store.set_epoch_deadline(1);
        store.epoch_deadline_async_yield_and_update(1);

        let (tx, rx) = tokio::sync::oneshot::channel();
        let request = store
            .data_mut()
            .http()
            .new_incoming_request(Scheme::Http, req)?;
        let out = store.data_mut().http().new_response_outparam(tx)?;
        let pre = self.pre.clone();

        // The handler runs in its own task: it may keep streaming the body after the
        // status/headers are sent (SSE), so we don't join it before replying.
        let task = tokio::spawn(async move {
            let proxy = pre.instantiate_async(&mut store).await?;
            proxy
                .wasi_http_incoming_handler()
                .call_handle(store, request, out)
                .await
        });

        match rx.await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(code)) => Err(code.into()),
            // The guest dropped the outparam without setting a response — surface why.
            Err(_) => match task.await {
                Ok(Ok(())) => bail!("guest handler returned without setting a response"),
                Ok(Err(err)) => Err(err.into()),
                Err(join) => Err(join.into()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CapabilityProfile;
    use rusm_otp::Runtime;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const HELLO: &[u8] = include_bytes!("../../tests/fixtures/http_hello.wasm");

    /// One raw HTTP/1.1 GET (Connection: close) → the full response text.
    async fn get(addr: std::net::SocketAddr) -> String {
        let mut conn = tokio::net::TcpStream::connect(addr).await.unwrap();
        conn.write_all(b"GET / HTTP/1.1\r\nHost: rusm\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut buf = Vec::new();
        conn.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_wasm_component_serves_an_http_request() {
        let wr = WasmRuntime::new(Runtime::new()).unwrap();
        let prepared = wr
            .prepare_http(&wr.compile_component(HELLO).unwrap())
            .unwrap();
        let server = wr.http_server(&prepared, CapabilityProfile::Trusted.capabilities());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(server.serve(listener));

        let response = get(addr).await;
        assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
        assert!(
            response.contains("hello from RUSM"),
            "the component produced the body: {response}"
        );

        handle.abort();
    }
}
