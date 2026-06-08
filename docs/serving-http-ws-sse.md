# Serving HTTP, WS & SSE from a component (Phase 11)

> **Status: HTTP, WS, and SSE all work — from both RS and TS components.** A WASM
> component is served over real **HTTP** (`WasmRuntime::http_server` —
> [`http_bench`](../examples/http_bench/): lean ~64.5k req/s instance-per-request vs
> ~197k bare-hyper), real **WebSockets** with *one component process per connection*
> (`WasmRuntime::ws_server` — [`ws_bench`](../examples/ws_bench/): ~192k echo
> round-trips/s, 128/128 held, sandbox cost inside noise), and real **SSE** (a
> `wasi:http` streaming body — [`sse_bench`](../examples/sse_bench/): ~1.5M events/s
> across 128 long-lived streams).
>
> **Both guest languages serve all three.** RS components compile straight to
> `wasi:http` (via `wstd`) / the actor world. TS components run on the embedded
> rquickjs runners: `http_server_js` + the **js-http-runner** (a raw-`wasi:http`
> component that runs a TS HTTP **handler** — `export default` a request→response
> function, server-side — including pull-based streaming for SSE), and `ws_server_js`
> (a TS worker on the js-runner, one process per connection).
> The `rusm-otp` core stays Wasm-free throughout (hyper, `tokio-tungstenite`, and
> `wasi:http` live only in `rusm-wasm`).
>
> | | HTTP | WS | SSE |
> |---|---|---|---|
> | **Rust** | ✅ `wstd` / lean `wasi:http` | ✅ `rusm-rs` worker | ✅ `wstd` streaming body |
> | **TypeScript** | ✅ `export default` handler | ✅ `export default` worker | ✅ `Response(ReadableStream)` |

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

## Which Rust API? (`wasi:http` vs `wstd` — not a real choice)

These are **layers, not competitors**:

- **`wasi:http`** is the **standard interface** — the contract a component exports.
  RUSM hosts it; it's what makes a component portable.
- **`wstd`** is an ergonomic **framework** over it (`Request`/`Response`, async
  Rust). It *produces* a standard `wasi:http` component — it's built on the raw
  bindings, not an alternative to them.

**Advice: default to `wstd`** (Rust) or the **`fetch` shape** (TS) — readable,
familiar, standard. The raw `wasi:http` bindings are ~26% faster *only* for a
trivial handler; a real handler (DB / LLM / render) takes milliseconds, so the
framework's ~50µs overhead is noise. Drop to raw `wasi:http` only if a profiler
proves the HTTP layer itself is your bottleneck — rare. Either way the artifact is
the same standard component; the choice is reversible and the developer's alone.

## RS source (`wasm32-wasip2`)

**HTTP** — a standard `wasi:http` server via `wstd` (the Bytecode Alliance's
ergonomic layer; the artifact is a plain wasi:http component RUSM hosts). This is the
[`http-hello`](../crates/rusm-wasm/tests/fixtures/http-hello/) fixture verbatim:

```rust
use wstd::http::body::Body;
use wstd::http::{Request, Response};

#[wstd::http_server]
async fn main(_req: Request<Body>) -> anyhow::Result<Response<Body>> {
    Ok(Response::new("hello from RUSM\n".to_owned().into()))
}
```

**SSE** — set `text/event-stream` and hand the body a stream of frames; the host
flushes each one as the guest yields it. The [`sse-ticker`](../crates/rusm-wasm/tests/fixtures/sse-ticker/)
fixture, condensed:

```rust
use futures_lite::stream::unfold;
use wstd::http::body::{Body, Bytes};

#[wstd::http_server]
async fn main(_req: Request<Body>) -> Result<Response<Body>, Error> {
    let events = unfold(0u32, |n| async move {
        wstd::task::sleep(Duration::from_millis(50)).await;     // backpressured by the client
        Some((Ok::<_, Infallible>(Bytes::from(format!("data: tick {n}\n\n"))), n + 1))
    });
    Ok(Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_try_stream(events))?)
}
```

**WS** — the component is a normal actor: the host owns the socket and delivers each
inbound frame as a mailbox message; message 1 is the connection's **writer pid** (the
process that owns the socket sink). Echo = send each frame back to the writer. This is
the [`rs-ws-echo`](../crates/rusm-wasm/tests/fixtures/rs-ws-echo/) fixture:

```rust
fn run() {
    // Message 1: the writer pid to answer through (the host owns the socket).
    let writer = rusm_rs::Pid(
        String::from_utf8(rusm_rs::receive_bytes()).unwrap().parse().unwrap(),
    );
    // Every later message is one inbound WS frame — echo it straight back.
    loop {
        let frame = rusm_rs::receive_bytes();
        rusm_rs::send_bytes(writer, &frame);
    }
}
```

## TS source (Bun-bundled → the rquickjs runners)

A TS component is a Bun-bundled `.js` run on an embedded rquickjs runner — no jco.
`http_server_js` / `ws_server_js` deliver the bundle; the standard Web types
(`Request`/`Response`/`URL`/`ReadableStream`) are polyfilled. These are the checked-in
fixtures verbatim.

**HTTP** — a server-side **handler**: `export default` a request→response function.
The js-http-runner builds a `Request` from the wasi:http request (URL reconstructed
from `Host` + path, so `new URL(request.url).searchParams` works) and marshals the
`Response` back. (The Workers/Deno `export default { fetch }` shape is also accepted,
so those components port over; we lead with the plain handler — it's server code, not
a client `fetch`.)

```ts
export default function handle(request: Request): Response {
  const who = new URL(request.url).searchParams.get("who") ?? "world";
  return new Response(`hello, ${who}\n`, { headers: { "content-type": "text/plain" } });
}
```

**SSE** — return a `Response` whose body is a `ReadableStream`; the runner pulls each
chunk and flushes it incrementally (chunked, truly streamed — not buffered):

```ts
export default function handle(): Response {
  let n = 0;
  const enc = new TextEncoder();
  const body = new ReadableStream({
    pull(c) {
      if (n >= 5) return c.close();
      c.enqueue(enc.encode(`data: tick ${n++}\n\n`));
    },
  });
  return new Response(body, { headers: { "content-type": "text/event-stream" } });
}
```

**WS** — a TS **worker** (`export default` an async fn), one process per connection.
Its first message is the writer pid (the host owns the socket); echo = send each frame
back. Same actor shape as the Rust fixture:

```ts
export default async function () {
  const writer = BigInt(await Process.receiveText());  // msg 1: the writer pid
  for (;;) {
    const frame = await Process.receive();             // each inbound WS frame
    Process.send(writer, frame);                        // echo
  }
}
```

Both guests stay sandboxed (a serving component gets only the capabilities its
profile grants) and supervised (a crash restarts the handler, never the listener).

## Benchmarks

Each serving topic has a runnable stress example reporting against a **bare-host
baseline** (or the host transport ceiling) so the sandbox overhead is explicit — the
honest, *earned* number. Representative loopback figures:

| Topic | Example | Earned (loopback) |
| --- | --- | --- |
| **HTTP** | [`http_bench`](../examples/http_bench/) | lean raw-`wasi:http` **~64.5k req/s** instance-per-request, wstd ~51k, bare hyper ~197k. Instantiate-only ~11µs (lean) — per-request isolation is cheap, so warm-pooling is **not** worth it. The ~3× vs bare hyper is `wasi:http` component-model marshaling. |
| **WS** | [`ws_bench`](../examples/ws_bench/) | **~192k echo round-trips/s, 128/128 connections held.** One sandboxed component process per connection; the per-message writer→component→writer mailbox hop costs ~nothing — the component path lands **inside noise** of the bare hyper+tungstenite transport. |
| **SSE** | [`sse_bench`](../examples/sse_bench/) | **~1.5M events/s across 128 long-lived streams, all held.** Each stream is its own `wasi:http` instance; a dropped client tears down only its own instance. |

The **http-throughput** scenario is live in the dashboard; **ws-echo** and
**sse-fanout** dashboard scenarios follow the same live-engine pattern (the standalone
examples above are the source of truth for the numbers today).

What "good" looks like, confirmed: HTTP within a small multiple of bare hyper (the
price of per-request memory isolation, paid once — like module-storm vs a bare task);
WS/SSE holding every connection open under load (bounded by RAM, not a fixed cap),
latency flat because the streams are Tokio-backpressured.

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
