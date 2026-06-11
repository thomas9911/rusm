# Serving HTTP, WS & SSE from a component (Phase 11)

> **Status: HTTP, WS, and SSE all work — from both RS and TS components, served on
> real ports by `rusm serve`.** A WASM component is served over real **HTTP**
> (`WasmRuntime::http_server`), real **WebSockets** with *one component process per
> connection* (`WasmRuntime::ws_server`), and real **SSE** (a `wasi:http` streaming
> body). The **fair, credible headline numbers** are measured **out-of-process** by
> [`rusm-loadtest`](https://github.com/archan937/rusm/tree/main/bench/rusm-loadtest)
> against a live `rusm serve` port (loopback): HTTP **~46k req/s** at 0% errors, WS
> **~146k round-trips/s** across 256 held connections, SSE **~609k events/s** across
> 256 held streams, and **~34k sandboxed-process-per-connection WS
> establishments/sec** (`rusm-loadtest`'s `conn` mode — each connection spawns a full
> component). The dashboard also carries **six co-resident live demo tiles**
> (`http-throughput`, `ws-echo`, `sse-fanout` and their `*-ts` twins): each spins up
> the same real in-process WASM server and drives it through the same load path as
> `rusm-loadtest` (balter for HTTP request-rate, a connection-capacity harness for
> WS/SSE held connections), with load generator and server sharing the node process —
> so the tile figures (http-throughput ~20k req/s, ws-echo ~195k rt/s, sse-fanout
> ~695k events/s) differ by design from the fair out-of-process headlines. (The
> in-process `http_bench` / `ws_bench` / `sse_bench` examples still exist to measure
> the engine against the bare-host ceiling.)
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

## The chosen design: server on the host, handler in one of two shapes

This is the design RUSM commits to. The **server always lives on the host** — hyper
(HTTP/SSE), `tokio-tungstenite` (WS), all in `rusm-wasm`; the Wasm-free `rusm-otp`
core never sees them. What varies is the **handler** behind it, and there are exactly
two shapes, chosen per endpoint by `mode` in `rusm.toml`:

| | **Spawned** — `mode = "per-request"` (default) | **Resident** — `mode = "resident"` |
|---|---|---|
| **Lifetime** | a fresh sandboxed instance **per request** (HTTP/SSE) or **per connection** (WS) | one — or a pooled set of — **long-lived** instance(s) serve everything |
| **State** | none; discarded after the unit (isolation) | **held across requests** in linear memory (counter, cache, sessions, a chat room) |
| **Faults** | a trap fails just that one request/connection | a crash is **restarted by a supervisor**; the endpoint keeps serving (in-memory state is lost — persist it if it matters) |
| **Concurrency** | scales ~N×cores in parallel | serialized per instance (one mailbox); scale with `instances = N` (+ optional `shard_by`) |
| **Elixir analogy** | a fresh process per request | a **GenServer** — a named process owning state; requests are messages |
| **Use for** | stateless handling, maximal isolation & throughput | stateful services: caches, pub/sub hubs, game/chat rooms, rate limiters |

Both shapes are **sandboxed** (capability profile) and **supervised**, and both work
identically for **RS and TS** guests — no favoured language. The default is
`per-request`; opt into `resident` when the handler must remember things between
requests.

### Resident is *also* faster (when it fits)

A resident handler pays component instantiation **once**, then each request is a
mailbox round-trip + a small base64 JSON envelope — all on `rusm-otp`'s ~21M-msg/s
path. The per-request shape pays a fresh `wasi:http` instantiation every request.
Measured (`http_bench`, one machine, 64 keep-alive clients): a single resident
instance sustains **~144k req/s** vs **~63k** for per-request `wasi:http` (≈2.3×), at
≈2.4× lower p50 — within ~1.3× of bare, no-Wasm hyper. The "serialized single
mailbox" worry is moot for fast handlers (the mailbox isn't the bottleneck); it only
bites if a handler blocks — which is what the **offload** contract (spawn a worker,
reply from it) and `instances` / **503** back-pressure are for. So resident isn't a
slow-but-stateful compromise: for a fast handler it's the faster *and* stateful shape.

> **Reconciles the older note.** "Instance-per-request is optimal" (bottom of this
> page) is about the *stateless* case — not running a warm pool that would leak state
> to save ~30µs. Resident is the deliberate choice when you *want* shared state, and
> it removes the per-request instantiation entirely.

### How a resident handler is wired

The host gateway turns each request/frame into a message on the **existing JSON actor
wire** and routes it to the resident process; the reply returns over a Wasm-free
**responder** (HTTP/SSE) or **writer** (WS) process — the same idiom WS already uses —
so the sandboxed guest never touches a socket. A resident pool is a one-for-one
**supervised** set of instances addressed by a registry slot name, so a restarted
instance (a fresh pid) is picked up automatically. `instances = N` shards across the
pool (round-robin, or `shard_by = "header:<name>"` for session affinity); an
overloaded or mid-restart instance sheds to **503** (HTTP) / a refused upgrade (WS).

```toml
# rusm.toml — a stateful resident endpoint, sharded by session, 4 instances
[[serve]]
name      = "rooms"
protocol  = "ws"            # http | sse | ws
listen    = "127.0.0.1:9000"
mode      = "resident"      # default is "per-request" (stateless)
instances = 4               # supervised pool; omit for a singleton
shard_by  = "header:x-session"  # same session → same instance (omit → round-robin)
```

Resident guests reuse the SDKs' serving loops — `rusm_rs::http::serve` /
`rusm_rs::http::serve_sse` / `rusm_rs::ws::serve` (Rust), and the *same*
`export default { fetch }` / `{ websocket }` a TS dev already writes (it becomes
stateful purely by running resident). See [Resident handlers](#resident-handlers-stateful)
below.

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

`rusm.toml` declares one or more **`[[serve]]`** entries — `name`, `protocol`
(`http` | `sse` | `ws`), `listen` (e.g. `"127.0.0.1:8080"`), and `capability`
(defaults to `sandboxed`). **`rusm serve`** binds each on its real TCP port, loading
the component from `wasm/<name>.{wasm,js}` (HTTP and SSE via the `http_server` path,
WS via `ws_server`) — the same app model and supervision as any other component. The
node only serves; it never generates load. Start from scratch with
**`rusm new <name>`**, which scaffolds a zero-dependency TS HTTP component
(`components/api/index.ts`, a default `Request`→`Response` handler), a `rusm.toml`
with a `[[serve]]` entry, `.gitignore`, and a README:

```sh
rusm new hello && cd hello
rusm build
rusm serve
curl http://127.0.0.1:8080/
```

(Serving TLS is still to land; HTTPS/WSS terminate with the same rustls stack as the
cluster once wired.)

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
[`http-hello`](https://github.com/archan937/rusm/tree/main/crates/rusm-wasm/tests/fixtures/http-hello) fixture verbatim:

```rust
use wstd::http::body::Body;
use wstd::http::{Request, Response};

#[wstd::http_server]
async fn main(_req: Request<Body>) -> anyhow::Result<Response<Body>> {
    Ok(Response::new("hello from RUSM\n".to_owned().into()))
}
```

**SSE** — set `text/event-stream` and hand the body a stream of frames; the host
flushes each one as the guest yields it. The [`sse-ticker`](https://github.com/archan937/rusm/tree/main/crates/rusm-wasm/tests/fixtures/sse-ticker)
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
the [`rs-ws-echo`](https://github.com/archan937/rusm/tree/main/crates/rusm-wasm/tests/fixtures/rs-ws-echo) fixture:

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

## Resident handlers (stateful)

The above are *spawned* (stateless) handlers. A **resident** handler holds state
across requests; the authoring surface is the same for both languages (no favoured
guest), and the host wiring is identical — only `mode = "resident"` differs.

**Rust** — implement a small trait with `&mut self` state and `serve` it (no
`wasi:http`; this is an actor component driving `run`). The
[`rs-resident-count`](https://github.com/archan937/rusm/tree/main/crates/rusm-wasm/tests/fixtures/rs-resident-count)
fixture, verbatim:

```rust
struct Counter { hits: u64 }
impl rusm_rs::http::Handler for Counter {
    fn handle(&mut self, _req: rusm_rs::http::Request) -> rusm_rs::http::Response {
        self.hits += 1;                       // state persists across requests
        rusm_rs::http::Response::text(format!("hit #{}\n", self.hits))
    }
}
fn run() { rusm_rs::http::serve(Counter { hits: 0 }); }   // never returns
```

- **SSE**: `rusm_rs::http::serve_sse(|req| events)` — yield the `text/event-stream`
  event chunks; each rides the byte stream to the client (offload an endless feed to
  a spawned worker so the instance's loop stays free).
- **WS**: implement `rusm_rs::ws::Handler` (`open`/`message`/`close`, each taking the
  connection's `conn` pid) and `rusm_rs::ws::serve(handler)`; reply with
  `rusm_rs::ws::send(conn, &frame)`. One instance multiplexes every connection — a
  message from one can broadcast to all (a chat room, [`rs-resident-ws`](https://github.com/archan937/rusm/tree/main/crates/rusm-wasm/tests/fixtures/rs-resident-ws)).

**TypeScript** — the **same** `export default { fetch }` / `{ websocket }` shape; it
becomes stateful purely by running resident (module-scope state persists). The
[`ts-resident-*`](https://github.com/archan937/rusm/tree/main/crates/rusm-wasm/tests/fixtures) fixtures:

```ts
let hits = 0;                                   // module state — persists (resident)
export default {
  fetch(_req) { hits++; return new Response(`hit #${hits}\n`); },
};

// WebSocket chat room — one instance multiplexes every connection:
const members = [];
export default {
  websocket: {
    open(conn)  { members.push(conn); Process.send(conn, new TextEncoder().encode("welcome")); },
    message(_c, data) { for (const m of members) Process.send(m, data); },  // broadcast
    close(conn) { const i = members.indexOf(conn); if (i >= 0) members.splice(i, 1); },
  },
};
```

(SSE is the same `export default { fetch }` returning a `Response(ReadableStream)`;
resident streams each chunk just like the per-request runner.)

Per-request bodies cross the actor wire as **base64** (compact + binary-safe); SSE
event chunks ride the raw byte stream. The resident pool is **supervised**: kill an
instance and the supervisor restarts it, routing picks up the fresh pid, and the
endpoint keeps serving (with reset state).

### Live SSE fan-out — endless feeds, one publisher → N clients

`serve_sse(|req| events)` computes one finite stream per request and pumps it
inline — perfect for a short stream, wrong for an **endless** feed (the instance
would serve one client at a time). For a live feed broadcast to many clients
(dashboards, log tails, the kind of thing wasmCloud's lattice makes painful), the
idiomatic shape is **offload + pub/sub**, and RUSM ships it turnkey:

- **`rusm_rs::http::serve_sse_offloaded("pump")`** — the acceptor: per connection it
  replies the SSE head and hands the socket to a freshly-spawned **pump** process,
  then loops on — never head-of-line blocked, so one instance fronts many concurrent
  live streams.
- **`rusm_rs::http::SseConnection`** — the pump side: `accept()` the connection,
  subscribe to your event source, then `run(heartbeat, map)` to live-tail it (each
  message → an SSE frame, a heartbeat comment on idle, exits on disconnect).
- **`rusm_rs::pubsub::Topics`** — the event source: keyed `subscribe` / `publish`,
  with **monitor-based pruning** — a disconnected pump exits and is dropped from the
  topic automatically (crash-safe, no unsubscribe bookkeeping).

So a publisher calls `topics.publish("room/42", &event)` and every connected client
on that topic gets it live; the app writes *what* to broadcast, never the
subscriber/fan-out/cleanup machinery. (Proven end-to-end: one publish reaches every
open SSE connection, and a disconnect prunes its subscriber.)

## Benchmarks

Serving is benchmarked the **fair** way: **out-of-process**, by the
[`rusm-loadtest`](https://github.com/archan937/rusm/tree/main/bench/rusm-loadtest)
binary driven against a real `rusm serve` port. The load generator runs in a separate
process — so it never shares the server's CPU — and crosses a real socket.

- **HTTP** uses the **balter** crate (a Tokio-native load-testing framework) as a
  **fixed-rate sweep**: drive increasing target req/s and, at each level, measure
  achieved throughput + tail latency + error rate; climb until the SLA breaks or
  throughput plateaus, and report the max the server genuinely sustained. (balter's
  auto-saturation control loop is too cautious in the sub-millisecond loopback regime,
  so we use its reliable constant-rate controller and sweep ourselves — every number
  is a direct measurement, none extrapolated.)
- **WS & SSE** use a tokio-native **connection-capacity harness** (held connections
  sustaining echo round-trips / draining events), because these are
  connection-capacity workloads, not request-rate — reported as concurrency +
  sustained ops/sec + p50/p99.
- **Connection establishment** uses the `conn` mode — a connection-establishment storm
  that opens fresh WS connections as fast as the server can accept them. Each
  connection spawns a full sandboxed component process, so this is a richer claim than
  a raw TCP accept rate.

Measured out-of-process (loopback):

| Topic | Method | Measured |
| --- | --- | --- |
| **HTTP** | balter fixed-rate sweep | **~46k req/s at 0% errors.** |
| **WS** | connection-capacity harness | **~146k round-trips/s across 256 held connections.** One sandboxed component process per connection; the per-message writer→component→writer mailbox hop costs ~nothing. |
| **SSE** | connection-capacity harness | **~609k events/s across 256 held streams.** Each stream is its own `wasi:http` instance; a dropped client tears down only its own instance. |
| **Connections** | `conn` establishment storm | **~34k sandboxed-process-per-connection WS establishments/sec.** Connection establishment is OS-bound; each connection spawns a full component. |

The dashboard also carries the **six co-resident serving demo tiles**
(`http-throughput`, `ws-echo`, `sse-fanout` and their `*-ts` twins): each spins up the
same real in-process WASM server and drives it through the **same load path** as
`rusm-loadtest` (balter for HTTP request-rate, the connection-capacity harness for
WS/SSE), with the load generator and server sharing the node process. They are honest
**live demos** — useful to watch a real server take load on the dashboard — but
because they share CPU and hide the network behind loopback, their figures
(http-throughput ~20k req/s, ws-echo ~195k rt/s, sse-fanout ~695k events/s) differ by
design from the fair out-of-process headlines above, which remain the source of truth
for *served* throughput. The in-process `http_bench` / `ws_bench` / `sse_bench`
examples still exist to measure the **engine** against the bare-host transport ceiling
(so sandbox overhead is explicit).

What "good" looks like, confirmed: HTTP serving thousands of isolated
instance-per-request handlers a second over a real socket at zero errors; WS/SSE
holding every connection open under load (bounded by RAM, not a fixed cap), latency
flat because the streams are Tokio-backpressured.

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
