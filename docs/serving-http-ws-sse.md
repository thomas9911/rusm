# Serving HTTP, WS & SSE from a component (Phase 11)

> **Status: HTTP is built and measured; WS & SSE are design.** An RS (`wstd`) WASM
> component is **served over real HTTP today** (`WasmRuntime::http_server`, the
> [`http_bench`](../examples/http_bench/) benchmark — ~50k req/s instance-per-request
> vs ~198k bare-hyper). The WS/SSE sections below are still design previews; their
> code blocks are illustrative until those paths land.

RUSM's end goal is to run a component as a high-throughput **HTTP(S) / WS(S) / SSE
server** — a sandboxed, supervised process answering requests. Phase 11 delivers
this **standards-first**: a guest exports the standard `wasi:http` handler (or, for
TS, the familiar `fetch` shape), and RUSM hosts it. The actor world stays opt-in.

## The model

```
client ─TCP/TLS─▶ listener process ─hyper─▶ wasi:http ─▶ component instance
                  (supervised,                (wasmtime-      (sandboxed, per
                   process-per-conn)           wasi-http)       capability profile)
```

- **Transport — reuse what's there.** The listener is a supervised RUSM process
  accepting connections **process-per-connection** (Phase 5 TCP). **HTTPS/WSS**
  terminate with **rustls** — the exact stack the cluster transport already uses.
- **HTTP — instance-per-request, the standard way.** Each request is served by a
  **fresh, pooled component instance** running the standard
  `wasi:http/incoming-handler`, bridged from **hyper** by the official
  **`wasmtime-wasi-http`** crate. Instance-per-request is cheap on RUSM's pooled
  spawn path (~440k spawns/s), and a trap is just that one request failing — total
  isolation between requests. A guest built with **`wstd`** runs unchanged.
- **SSE — a streaming response body.** A `wasi:http` response body *is* an
  output-stream. For `text/event-stream`, the instance stays alive and **writes
  events over time** to that stream — backpressured by RUSM's Tokio-backed
  [byte streams](./concepts/byte-streams.md) (Phase 7), so a slow client slows the
  producer instead of growing memory.
- **WS — entirely host-side, not via `wasi:http` at all.** A WebSocket is only HTTP
  for its handshake; after the `Upgrade` it's a raw bidirectional stream — and the
  handshake + upgrade live on the host, which RUSM fully controls. **hyper** surfaces
  the upgrade (`hyper::upgrade::on`), **`tokio-tungstenite`** runs the WS protocol
  (handshake, masking/opcodes, ping/pong, fragmentation, close), and the guest
  receives a clean **message stream** over the same Phase 7 byte-stream primitive —
  delivered to a **long-lived component process**. The guest never sees raw frames or
  `wasi:http`. So `wasi:http` having no WebSocket surface is irrelevant: WS doesn't
  go through it. The guest's socket API is a RUSM convention (like the actor world) —
  and since there's no *wasi* WS standard, there's nothing to be non-portable against.

### Why this leans on Phase 10

SSE and WS connections are **long-lived instances** (one per open stream). That's
exactly what Phase 10's **on-demand instance tier** is for — thousands of concurrent
SSE/WS streams aren't capped by the fixed pool, they spill to the on-demand engine,
bounded by RAM. And **bounded mailboxes** give per-connection overload back-pressure.
The hardening phase was the groundwork for serving at scale.

### How you run it

`rusm.toml` gains an `[[http]]` block (address, the component to serve, capability
profile, TLS cert), and `rusm serve` / `rusm dev` bind it — the same app model and
supervision as any other component.

## RS source (`wasm32-wasip2`)

**HTTP** — a standard `wasi:http` server via `wstd` (the Bytecode Alliance's
ergonomic layer; the artifact is a plain wasi:http component RUSM hosts):

```rust
use wstd::http::{Body, IntoBody, Request, Response};
use wstd::http::server::{Finished, Responder};

#[wstd::http_server]
async fn main(req: Request<impl Body>, res: Responder) -> Finished {
    let who = req.uri().query().unwrap_or("world");
    res.respond(Response::new(format!("hello, {who}\n").into_body())).await
}
```

**SSE** — set the content type and write events to the streaming body over time:

```rust
#[wstd::http_server]
async fn main(_req: Request<impl Body>, res: Responder) -> Finished {
    let mut body = res.start(Response::builder()
        .header("content-type", "text/event-stream")
        .body(()).unwrap());           // streaming response body
    for i in 0.. {
        body.write_all(format!("data: tick {i}\n\n").as_bytes()).await?;
        wstd::task::sleep(Duration::from_secs(1)).await;   // backpressured by the client
    }
}
```

**WS** — RUSM hands the upgraded connection to the component as a `rusm_rs::Stream`;
the guest reads/writes frames (echo):

```rust
fn main() {
    // The host completes the handshake and delivers the socket as a byte stream.
    let mut socket: rusm_rs::Stream = rusm_rs::receive_stream();
    while let Some(frame) = socket.read() {
        socket.write(&frame);          // echo
    }
}
```

## TS source (Bun-bundled → the js-runner)

**HTTP** — the familiar Service-Worker / Deno / Workers `fetch` shape. RUSM maps
`wasi:http/incoming-handler` → this `default.fetch`; `Request`/`Response`/`URL` are
the Web types already polyfilled in Phase 8:

```ts
export default {
  async fetch(req: Request): Promise<Response> {
    const who = new URL(req.url).searchParams.get("who") ?? "world";
    return new Response(`hello, ${who}\n`);
  },
};
```

**SSE** — return a `Response` whose body is a `ReadableStream` (already polyfilled):

```ts
export default {
  async fetch(_req: Request): Promise<Response> {
    let i = 0;
    const body = new ReadableStream({
      async pull(c) {
        c.enqueue(`data: tick ${i++}\n\n`);
        await sleep(1000);            // backpressured by the client
      },
    });
    return new Response(body, { headers: { "content-type": "text/event-stream" } });
  },
};
```

**WS** — the Deno-style `upgradeWebSocket` (RUSM does the handshake, the socket is a
view over its stream primitive):

```ts
export default {
  async fetch(req: Request): Promise<Response> {
    if (req.headers.get("upgrade") !== "websocket")
      return new Response("expected websocket", { status: 426 });
    const { socket, response } = Process.upgradeWebSocket(req);
    socket.onmessage = (e) => socket.send(e.data);   // echo
    return response;
  },
};
```

Both guests stay sandboxed (a serving component gets only the capabilities its
profile grants) and supervised (a crash restarts the handler, never the listener).

## Benchmark plan

Three new dashboard scenarios, following the live-engine pattern of the existing
nine. Each reports against a **bare-host baseline** (hyper with no Wasm) so the
sandbox overhead is explicit — the honest number.

| Scenario | Drives | Headline metrics |
| --- | --- | --- |
| **http-throughput** ✅ *(live dashboard scenario + [`http_bench`](../examples/http_bench/))* | many keep-alive clients hitting a 200-OK component, instance-per-request | **measured: ~52k req/s, p50 ~160µs** (moderate load). Breakdown: instantiation is only **~30µs** — per-request isolation is cheap; the rest is the wstd guest + `wasi:http` marshaling. So a warm-instance pool is **not** worth it (it would trade isolation for ~30µs); instance-per-request is optimal. |
| **sse-fanout** | N concurrent SSE subscribers, each fed M events/sec from long-lived instances | sustained events/sec, concurrent streams held, per-event p50/p99 — stresses the long-lived-instance + overflow tier + stream backpressure |
| **ws-echo** | N concurrent WS connections, echo round-trip | messages/sec, round-trip p50/p99, concurrent sockets |

What "good" looks like: http-throughput within a small multiple of bare hyper (the
price of per-request memory isolation, paid once — like module-storm vs a bare
task); sse-fanout/ws-echo holding **tens of thousands** of concurrent streams
(bounded by RAM via the overflow tier, not a fixed cap), with latency flat under load
because the streams are Tokio-backpressured.

## Battle-proven foundations (no reinvention)

- **hyper** — HTTP/1.1 + HTTP/2 parsing and connection management.
- **`wasmtime-wasi-http`** — the official hyper ↔ `wasi:http` bridge (default host
  impl; we can hand-roll the same interface if needed — see above).
- **`tokio-tungstenite`** — the battle-proven Rust WebSocket protocol (handshake,
  framing, ping/pong, close); the host runs it, the guest sees clean messages.
- **`wstd`** — the Bytecode Alliance ergonomic layer for RS `wasi:http` guests.
- **Web `fetch`/`Response`/`ReadableStream`** — the Workers/Deno shape for TS, a DX
  millions of developers already know; the polyfills exist (Phase 8).
- **rustls + ring** — HTTPS/WSS termination, the same stack as the cluster.
- **RUSM's own** — pooled instance-per-request spawn, Tokio-backpressured byte
  streams, the on-demand overflow tier, capability profiles, and supervision.

## Implementation choices — all host-side, under our control

**None of these are capability blockers.** RUSM owns the host bridge (hyper + Tokio +
the WASI host bindings it already hand-wires via `bindgen!`), so each is a
"which path" decision, not a "can we" one.

- **Who provides the `wasi:http` host side.** Default to the off-the-shelf
  **`wasmtime-wasi-http`** (the official hyper ↔ `wasi:http` bridge). If it falls
  short on any point — e.g. async streaming response bodies under the p3 component
  model — we implement the `wasi:http` host interface **ourselves** over hyper + our
  stream primitive, exactly as we already hand-wire WASI p2/p3 and the actor world.
  The guest contract (the standard `wasi:http` WIT) is fixed either way; only our
  host implementation varies. So HTTP serving is never gated on someone else's crate.
- **WebSocket** — fully host-side (hyper upgrade + `tokio-tungstenite` + the stream
  primitive); it doesn't go through `wasi:http` at all. A RUSM convention, not a
  missing capability.
- **Instance-per-request vs keep-warm — resolved by measurement.** We measured it:
  instantiation is **~30µs** (pooling + CoW), a small slice of a ~160µs request. A
  warm pool's only payoff is avoiding that 30µs, at the cost of leaking state between
  requests — trading isolation for ~30µs. Not worth it. **Instance-per-request is
  optimal**, not just simplest. The remaining cost is the guest handler + marshaling;
  the no-compromise lever there is a leaner guest (raw `wasi:http` vs wstd), which is
  the developer's choice.
