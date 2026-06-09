# Getting started {#overview}

This page takes you from a clean machine to running real RUSM processes — first
the pure-Rust actor core, then hosting WebAssembly, then writing your own
components. Every command and snippet here is real and current; anything not yet
built is marked **Roadmap**.

## Install

RUSM isn't published to crates.io yet, so you **clone the repo**:

```sh
git clone https://github.com/archan937/rusm
cd rusm
```

Prerequisites:

- **Rust** 1.94+ via [`rustup`](https://rustup.rs). To build guest
  components/modules, add the Wasm targets:
  ```sh
  rustup target add wasm32-wasip2 wasm32-wasip1
  ```
  (`wasm32-wasip2` for components, `wasm32-wasip1` for core modules.)
- **Bun** 1.3+ ([bun.sh](https://bun.sh)) — the dashboard and docs site use Bun,
  never Node.js. Only needed for those.

Verify the build:

```sh
cargo test          # the whole workspace
```

## Quick start

The fastest way to *see* RUSM is the live dashboard:

```sh
make dashboard      # builds + starts a node, then the dashboard — open the printed URL
```

Pick a scenario (e.g. **spawn storm**, **stream pipe**), hit **Run**, and watch
real throughput, latency, and the live observer. Everything is driven by the real
runtime.

### Make commands

| Command | What it does |
| --- | --- |
| `make dashboard` | Build + start a node, then the dashboard (the headline demo). |
| `make node` | Start a node on `ws://127.0.0.1:4000` (release). |
| `make ui` | Start only the dashboard (expects a node already running). |
| `make attach` | Attach a live REPL to the local node (like `iex --remsh`). |
| `make run SCENARIO=… SECONDS=…` | Run a benchmark scenario in the terminal. |
| `make example EX=…` | Run a bundled example (`host_components`, …). |
| `make test` / `make cov` | All Rust + dashboard tests / coverage. |
| `make fmt` / `make fmt-check` | Format / check (Rust + dashboard). |
| `make docs` / `make docs-build` | Live-preview / build this docs site. |

Run `make` with no target for the full list.

## Two ways to use RUSM

1. **As a library you embed** — depend on the `rusm-otp` core (and optionally the
   `rusm-wasm` backend) and drive it from your own Rust binary. Best when RUSM is a
   piece of a larger app.
2. **As an app you run** — declare components in `rusm.toml` and let the `rusm` CLI
   build, load, and supervise them. Best for a RUSM-first project.

The next sections cover both.

## 1. Without a Wasm runtime — the OTP core

RUSM's heart is a **Wasm-free** Erlang/OTP actor library, `rusm-otp`. You can use
it on its own — real lightweight processes, message passing, links, monitors,
supervision, a registry, and timers — with **no WebAssembly at all**. This is the
model RUSM is built on, and it stands alone (the dependency graph guarantees it).

```rust
use rusm_otp::{ExitReason, Received, Runtime};

#[tokio::main]
async fn main() {
    let rt = Runtime::new();

    // A worker: receive one message, then exit.
    let worker = rt.spawn(|mut ctx| async move {
        if let Received::Message(bytes) = ctx.recv().await {
            println!("worker got {} bytes", bytes.len());
        }
    });

    // Supervise it: monitor delivers a `Down` with the exit reason.
    let (tx, rx) = tokio::sync::oneshot::channel();
    let watcher = rt
        .spawn(move |mut ctx| async move {
            if let Received::Down { reason, .. } = ctx.recv().await {
                let _ = tx.send(reason);
            }
        })
        .pid();
    rt.monitor(watcher, worker.pid());

    rt.send(worker.pid(), b"hello".to_vec()); // messages are bytes (Vec<u8>)
    assert_eq!(rx.await.unwrap(), ExitReason::Normal);
}
```

You also get `spawn_link` (crash propagation), `trap_exit`, a named **registry**
(`register`/`whereis`), **timers** (`send_after`/`cancel`), graceful `shutdown`,
and **TCP** (`listen`/`connect`, one process per connection) — all in `rusm-otp`,
all without touching Wasm. See [links & supervision](./concepts/links-and-supervision.md).

## 2. With an already-compiled `.wasm` — embedding

Add the `rusm-wasm` backend and host a prebuilt component as a process. A
`WasmRuntime` wraps an `rusm-otp` `Runtime`; **construct it inside a Tokio runtime**
(it starts the epoch ticker).

```rust
use rusm_otp::Runtime;
use rusm_wasm::{Capabilities, WasmRuntime};

#[tokio::main]
async fn main() {
    let rt = Runtime::new();
    let wasm = WasmRuntime::new(rt.clone()).unwrap();

    // compile once → prepare once (imports + entry export resolved) → spawn many.
    let bytes = std::fs::read("wasm/worker.wasm").unwrap();
    let prepared = wasm
        .prepare_component(&wasm.compile_component(&bytes).unwrap(), "run")
        .unwrap();

    // Default-deny Sandboxed profile…
    wasm.spawn_component(&prepared).join().await;

    // …or grant capabilities explicitly (here: an 8 MiB heap cap):
    let caps = Capabilities::nothing().max_memory(8 << 20);
    wasm.spawn_component_with(&prepared, caps).join().await;
}
```

A trap (or a denied capability the guest turns into a trap) exits the process
`Crashed`, so links and supervisors react exactly as for a native process. The
runnable [`host_components`](https://github.com/archan937/rusm/tree/main/examples/host_components)
example (`make example EX=host_components`) shows this end to end, including a
memory-cap denial.

> **Core modules.** A `wasm32-wasip1` core module works the same way with
> `compile` / `prepare(module, "run")` / `spawn` (see the wasip1 bridge).

## 3. With an already-compiled `.wasm` — the app model

For a RUSM-first project, declare components in `rusm.toml` and let the CLI load
and supervise them from `./wasm/`:

```toml
# rusm.toml
listen = "127.0.0.1:4000"
profile = "balanced"

[[components]]
name = "worker"          # loaded from ./wasm/worker.wasm
capability = "sandboxed" # a built-in or a custom profile (below)
restart = true           # supervise: restart if it exits
```

```sh
rusm run          # load every [[components]] from ./wasm/, spawn under its profile
```

**Serving on a real port.** To run a component as an HTTP / WS / SSE server, declare
a `[[serve]]` entry and run `rusm serve` — it binds each on its TCP `listen` address,
loading the component from `wasm/<name>.{wasm,js}` (HTTP and SSE via the `http_server`
path, WS via `ws_server`). The fastest way in is **`rusm new <name>`**, which
scaffolds a ready-to-serve app (a zero-dependency TS HTTP component, a `rusm.toml`
with a `[[serve]]` entry, `.gitignore`, README):

```toml
[[serve]]
name = "api"              # loaded from ./wasm/api.{wasm,js}
protocol = "http"         # "http" | "sse" | "ws"
listen = "127.0.0.1:8080"
capability = "sandboxed"  # defaults to sandboxed
```

```sh
rusm new hello && cd hello
rusm build
rusm serve
curl http://127.0.0.1:8080/
```

**Custom capability profiles.** Beyond the three built-ins (`sandboxed` /
`network-client` / `trusted`), you can define your own — like Cargo's
`[profile.<name>]`. A profile `inherits` a built-in base (default `sandboxed`,
default-deny) and overrides only the grants it sets; a component references it by
name. A spawned child never exceeds its spawner's capabilities (no escalation).

```toml
[capabilities.agent]
inherits = "network-client"   # base; omit for sandboxed (default-deny)
spawn = true                  # may spawn other components by name
max-memory-mb = 256
env = ["OPENAI_API_KEY"]      # grant these keys (values from process env / .env)
preopen = [{ host = "./data", guest = "/data", read-only = false }]

[[components]]
name = "pages-agent"
capability = "agent"          # resolves to the custom profile above
```

## 4. A Rust WASM component (source only)

Write the source, let RUSM build it. A component lives under `components/<name>/`:

```
my-app/
├── rusm.toml
├── components/
│   └── worker/
│       ├── Cargo.toml      # crate-type = ["cdylib"], wit-bindgen
│       ├── wit/            # the rusm:runtime world (vendored from crates/rusm-wasm/wit)
│       └── src/lib.rs
└── wasm/                   # rusm build writes worker.wasm here
```

`src/lib.rs` binds the `rusm:runtime` actor world with `wit-bindgen` and exports
`run`:

```rust
wit_bindgen::generate!({ world: "process", path: "wit" });

use rusm::runtime::actor;

struct Component;

impl Guest for Component {
    fn run() {
        actor::set_label("worker");
        let msg = actor::receive();              // block for a message (bytes)
        actor::send(actor::own_pid(), &msg);     // echo to self, etc.
    }
}

export!(Component);
```

Build and run the whole app:

```sh
rusm build        # cargo build --target wasm32-wasip2 per components/* → ./wasm/
rusm run          # spawn them per rusm.toml
rusm dev          # build + run, then watch ./components and reload on edit
```

One toolchain, no jco, no cargo-component — `cargo build --target wasm32-wasip2`
componentizes directly. **`rusm dev`** keeps running: edit a component and save,
and it rebuilds + reloads it automatically (a dependency-free mtime watch).

## 5. A TS / JS WASM component (source only)

TypeScript guests are **first-class, sandboxed RUSM processes** — the
`genius-wasmcloud` model, **no jco**. RUSM ships one **js-runner** component: it
embeds [rquickjs](https://github.com/DelSkayn/rquickjs) (QuickJS, compiled to
`wasm32-wasip2`, ~720 KB) and runs your JS, exposing a `Process` global bridged to
the actor world. You write TS; **Bun** bundles it to one `.js`; the runner
executes it inside the same sandbox (capabilities, memory cap, epoch preemption)
as a Rust component. A TS component is just a folder with an `index.ts`:

A TS component comes in two shapes. A **service** just exports functions — RUSM
runs the receive→dispatch→reply loop around them:

```ts
// components/calc/index.ts
export function add(a: number, b: number): number { return a + b; }
export async function greet({ name }: { name: string }) { return `hi ${name}`; }

// Publish the contract — derived from the functions above, so it never drifts.
export type Calc = typeof import("./index");
```

A **worker** exports a `default` (async) function — RUSM runs it once. It reaches a
service through the **typed client**: `spawn<Calc>("calc")` returns a proxy whose
calls are real cross-process messages, hidden behind `await`. The caller imports only
the service's **contract** — a `type`, erased at build, so `calc` is never bundled in;
it stays a separate component reached over messages:

```ts
// components/commander/index.ts
import { spawn } from "rusm";
import type { Calc } from "../calc";          // type-only — the contract, not the code

export default async function () {
  const calc = spawn<Calc>("calc");            // spawn-from-guest, capability-gated
  console.log("2 + 3 =", await calc.add(2, 3)); // call: spawn + send + receive, hidden

  // A generator handler streams: `for await` its chunks.
  for await (const n of calc.countTo(3)) console.log(n);

  // A function argument is a callback — it stays here; the service's calls come
  // back as messages routed to it.
  await calc.work((pct) => console.log(`progress ${pct}`));
}
```

Declare both in `rusm.toml`, with capability profiles (the commander needs the
`spawn` capability — here a custom profile inheriting `trusted`):

```toml
[capabilities.orchestrator]
inherits = "trusted"

[[components]]
name = "calc"
capability = "sandboxed"

[[components]]
name = "commander"
capability = "orchestrator"
```

The `Process` API and `spawn` come from the **`rusm` package** — add it to your
app's `package.json` (by relative path until it's published):

```json
{ "dependencies": { "rusm": "file:../../packages/rusm" } }
```

`rusm build` runs `bun install` (if needed), then detects each `index.ts` and runs
`bun build --format=cjs` → `wasm/<name>.js` (a Rust component builds to
`wasm/<name>.wasm` instead — same manifest, same loader). `rusm run` loads `.js`
artifacts on the shared js-runner and prints:

```
2 + 3 = 5
hi RUSM
```

`receive`/`receiveText` and `Stream.read` are **async** (`await`) — the host call
still suspends the whole instance's fiber (freeing the worker), so it's cheap and
composes with Promises. The full `Process` API (`self`/`list`/`spawn`/`send`/
`receive`/`receiveText`/`register`/`whereis`/`isAlive`/`kill`/`setLabel`/
`openStream`/`acceptStream`), the `spawn<T>()` typed client (call / `for await`
stream / callback args / `.cast` / `.stop()`), binary (`Uint8Array`) messages, and
[byte streams](#streaming-from-a-component) are all typed by the **`rusm` package**.
The Web APIs the runner polyfills (`URL`, `TextEncoder`, `Headers`,
`ReadableStream`, `console`) are typed by the standard `DOM` lib — add it to your
`tsconfig.json` (`"lib": ["ES2022", "DOM"]`). See the runnable `ts-app` example
(Bun-built service + commander, with streaming + a callback) and `host_ts_component`.

> **Outbound `fetch` is deferred.** *Serving* HTTP works today (§6 — a TS or Rust
> handler answers requests over `wasi:http`). What's not wired yet is a guest making
> an *outbound* client `fetch`: it rejects with a clear error rather than silently
> failing.

**The Rust twin — `rusm-rs`.** A Rust guest gets the same story without raw
wit-bindgen: `Pid`/`send`/`receive` (serde)/`spawn`/registry/`Stream`, plus a
`#[rusm_rs::service]` macro over a module of free functions (mirroring TS's
`export function`s) that generates a `serve()` dispatch loop and a typed `Client`:

```rust
#[rusm_rs::service]
pub mod calc {
    pub fn add(a: i64, b: i64) -> i64 { a + b }
    pub fn count_to(n: i64) -> impl Iterator<Item = i64> { 1..=n }   // streaming
    pub fn work(progress: rusm_rs::Callback<i64>) -> String {        // callback
        for pct in [25, 50, 100] { progress.call(pct); } "done".into()
    }
}
// caller:  let calc = calc::Client::spawn("calc")?;  calc.add(2, 3)?;
```

Same JSON wire as rusm-ts, so a Rust client and a TS service interoperate. See the
`rusm-rs` crate README and the `rs-service` fixture.

## 6. Serve a component over HTTP (TypeScript or Rust)

Any component can be a high-throughput **HTTP / WS / SSE** server — declare a
`[[serve]]` entry and run `rusm serve`. The fastest start is `rusm new <name>`; here
is the whole shape so you can copy and adapt it. We'll build a tiny **stateful
counter** to show the resident model.

The layout (a TS component shown; a Rust one swaps `index.ts` for `Cargo.toml` +
`src/lib.rs`):

```text
my-api/
├── rusm.toml
├── .env                      # optional — env vars (the real process env always wins)
├── package.json              # TS only — pulls in the `rusm` types
├── components/
│   └── api/
│       └── index.ts          # the handler (TS)  ·  or src/{lib.rs} + Cargo.toml (Rust)
└── wasm/                     # rusm build writes api.{qjsbc,js,wasm} here
```

`rusm.toml` — one entry hosts the component on a real port:

```toml
[[serve]]
name = "api"                 # → ./wasm/api.{qjsbc,js,wasm}
protocol = "http"            # http | sse | ws
listen = "127.0.0.1:8080"
capability = "sandboxed"
mode = "resident"            # a warm pool that holds state — or "per-request" (isolated)
instances = 4                # resident pool size
```

The handler — same job, your language. Module/`&mut self` state persists because a
**resident** instance is long-lived (a per-request instance would always say `hit #1`):

::: code-group

```ts [components/api/index.ts]
// A resident HTTP handler. `hits` lives in module scope, so it persists across
// requests on the warm instance.
let hits = 0;

export default function handle(_request: Request): Response {
  hits++;
  return new Response(`hit #${hits}\n`, {
    headers: { "content-type": "text/plain" },
  });
}
```

```rust [components/api/src/lib.rs]
wit_bindgen::generate!({
    world: "process", path: "wit",
    with: { "rusm:runtime/actor@0.1.0": rusm_rs::rusm::runtime::actor },
});
use rusm_rs::http::{Handler, Request, Response};

struct Counter { hits: u64 }                 // &mut self state, persists across requests

impl Handler for Counter {
    fn handle(&mut self, _req: Request) -> Response {
        self.hits += 1;
        Response::text(format!("hit #{}\n", self.hits))
    }
}

struct Component;
impl Guest for Component {
    fn run() { rusm_rs::http::serve(Counter { hits: 0 }); }
}
export!(Component);
```

:::

Build and serve:

```sh
rusm build
rusm serve
# serving 1 endpoint(s):
#   api              http://127.0.0.1:8080
curl http://127.0.0.1:8080/      # hit #1, then hit #2, hit #3 …
```

To adapt: set `mode = "per-request"` for a fresh, isolated instance per request (a
trap fails just that request); or set `protocol = "ws"` / `"sse"` and implement the
WebSocket / SSE handler (`rusm_rs::ws::Handler` / `rusm_rs::http::serve_sse`, or the
matching TS `export default { websocket }` / streaming `Response`). See
[the serving model](./concepts/serving-model.md).

## Process management from inside a component (Rust)

A component imports the `rusm:runtime/actor` interface and calls the Erlang
`Process` API directly — the same operations the host has:

```rust
use rusm::runtime::actor;

let me = actor::own_pid();                 // self()
actor::register("worker");                 // name yourself in the registry
let who = actor::whereis("worker");        // look a name up → Option<pid>
let all = actor::list_processes();         // every live pid (find all)
let info = actor::info(me);                // Option<process-info>: links, label, mailbox depth…
let alive = actor::is_alive(some_pid);
actor::send(some_pid, &bytes);             // message-pass (bytes)
let incoming = actor::receive();           // block for the next message
actor::kill(some_pid);                     // terminate another process
actor::unregister("worker");
actor::set_label("worker#1");              // a human label for the observer
```

The runnable proof is the `actor-echo` test fixture, which drives **every** op
from inside a real component.

> **Spawn-from-guest is supported — capability-gated.** A component declared in
> `rusm.toml` can be `spawn`ed **by name** from inside another component (`spawn`
> in the actor ABI), so you get per-request workers and concealed typed clients —
> the Erlang model. It's default-deny (the `spawn` capability) and **non-escalating**:
> a child inherits the spawner's capabilities, never more. Components still find
> long-lived peers with `register`/`whereis` and talk with `send`/`receive`; a
> request/reply "callback" is just a message and a reply. See
> [components & the actor world](./concepts/components-and-the-actor-world.md).

> **From TS/JS.** The same operations are bridged to the `Process` global in the
> [js-runner](#_5-a-ts-js-wasm-component-source-only): `Process.self()`,
> `Process.list()`, `Process.send(to, msg)`, `Process.receive()`,
> `Process.register/whereis/isAlive/kill/setLabel`.

## Streaming (from a component)

Cross-process **byte streams** are Tokio-backpressured and ride the mailbox as
`Received::Stream` — see [byte streams](./concepts/byte-streams.md). A component
opens a stream to another process, writes chunks (the write parks under
back-pressure when the reader is slow), and closes it; the other side accepts and
reads to end-of-stream:

```rust
use rusm::runtime::actor;

// Producer: open a stream to `peer`, write chunks, then close.
if let Some(id) = actor::stream_open(peer) {
    actor::stream_write(id, b"hello!");   // false if the reader is gone
    actor::stream_close(id);              // signals end-of-stream
}

// Consumer: accept the next incoming stream, read to EOF.
let id = actor::stream_accept();          // blocks until a stream arrives
while let Some(chunk) = actor::stream_read(id) {
    // …handle chunk (Vec<u8>)…           // None == end-of-stream
}
```

The same ops are available to **wasip1 core modules** through the raw `rusm::*`
ABI, and the **stream-pipe** benchmark drives the underlying `StreamHandle` at
multiple GB/s.

> **Roadmap.** A native p3-typed `stream<u8>` in the WIT signature (instead of the
> handle-based ops above) is a future ergonomic layer; the handle-based API is the
> real, working one today.

## Capabilities & sandboxing

Every process is default-deny. Named profiles set the baseline; the `Capabilities`
builder overrides per spawn:

```rust
use rusm_wasm::{Capabilities, CapabilityProfile};

CapabilityProfile::Sandboxed.capabilities();          // CPU + bounded heap only
Capabilities::nothing()                               // start from nothing…
    .max_memory(16 << 20)                             // …a 16 MiB ceiling
    .allow_network(true)                              // …outbound sockets
    .preopen("/srv/data", "/data", /* read_only */ true) // …a mounted dir
    .env("LOG", "info");                              // …an env var
```

Grants map onto standard WASI plus a `StoreLimiter` memory cap. A breach traps
*only that process*. See [permissions & sandboxing](./concepts/permissions-and-sandboxing.md).

## Observe a running node

```sh
make node                 # start a node
make attach               # a live REPL: run <scenario>, detail off, stop, quit
make ui                   # or the visual dashboard against the same node
```

The dashboard's **Observer** shows the live process count and per-tick activity;
each scenario panel also unfolds its real engine source so you can see exactly how
it's built.
