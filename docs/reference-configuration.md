# Reference — configuration (`rusm.toml` & env)

Everything the `rusm` CLI reads at startup. Two inputs, layered:

1. **`rusm.toml`** — the node + app manifest (this page).
2. **Environment** — process env first, then a `./.env` file (`dotenvy`); real env wins.

**Layering for node settings:** built-in defaults → `rusm.toml` → CLI flags
(`--listen`, `--profile`). So a flag overrides the file, and the file overrides the
default. Unknown keys are **rejected** (a typo is an error, not a silent no-op).

## The file at a glance

```toml
# Node — the attach endpoint (`rusm node start`) and the benchmark node (`rusm-bench start`)
listen = "127.0.0.1:4000"     # WebSocket address the node's attach endpoint binds
profile = "balanced"          # light | balanced | max  — the benchmark node's throughput dial
ticks_per_second = 20         # snapshot rate, 10–60 Hz

# A custom capability profile (default-deny; inherits a built-in, overrides grants)
[capabilities.agent]
inherits = "network-client"
spawn = true
max-memory-mb = 256
env = ["OPENAI_API_KEY"]
preopen = [{ host = "./data", guest = "/data", read-only = false }]

# Components to run as supervised processes (`rusm run` / `rusm dev`)
[[components]]
name = "calc"                 # loads ./wasm/calc.{qjsbc,js,wasm}
capability = "sandboxed"
restart = true

# Components to host as network servers (`rusm serve`)
[[serve]]
name = "api"                  # loads ./wasm/api.{qjsbc,js,wasm}
protocol = "http"             # http | sse | ws
listen = "127.0.0.1:8080"
capability = "trusted"

# This listener's HTTP/SSE routes → handler actions (Rust handler components)
[serve.routes]
"GET /" = "api#home"
"GET /users/:id" = "api#show"
```

## Node settings

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `listen` | string | `"127.0.0.1:4000"` | The WebSocket address a node's attach endpoint binds (`rusm node start` / `rusm-bench start`). |
| `profile` | enum | `balanced` | The benchmark node's throughput dial — see below. |
| `ticks_per_second` | int (10–60) | `20` | How often the node samples + broadcasts a snapshot. |
| `store` | string? | none | Path (relative to the app dir) to the node's embedded durable key-value store — one file the node owns, no daemon. Required for components granted `storage` (the `kv` ABI) or a `kv:` bundle `source`. Omitted → no store. |

**`profile`** is the spawn-throughput dial, relative to your CPU count:

| Profile | Spawn workers | Use it when |
| --- | --- | --- |
| `light` | ~¼ of cores | speed isn't the point; leave the machine alone |
| `balanced` | ~⅖ of cores | good throughput with headroom (the default) |
| `max` | ~½ of cores | peak sustained rate, still smooth (the other half reap) |

It can also be changed live from the dashboard. (CLI override: `--profile`.)

## `[log]` — platform lifecycle logging

Opt-in, **off by default**, declared explicitly (no env magic). When set, the runtime
logs each component process as it spawns and exits — coloured, tagged `rusm` (so a
*platform* line is distinct from your app's own logs), as `component#pid` with the
process's **effective capabilities** on the spawn line:

```toml
[log]
level = "debug"      # off | error | warn | info | debug
```

```text
rusm spawn  meta-json#0    net spawn storage stdio env=3 mem=64M
rusm spawn  api#7          net env=2 mem=64M
rusm exit   api#7          normal
```

| `level` | Shows |
| --- | --- |
| `off` (default) | nothing — zero overhead |
| `error` | crashes (a trap / OOM) |
| `warn` | + kills and link cascades |
| `info` | + clean exits (every process *ending*) |
| `debug` | + every spawn (full visibility) |

Levels are cumulative. A **restart** needs no special event — it reads as a crash
`exit` (red) followed by a fresh `spawn` for the same component (a new pid). Only
named components are logged (not internal plumbing), and the spawn line's capability
summary makes a process's real privileges visible at the moment it starts.

## `[[components]]` — run as supervised processes

Each entry loads `./wasm/<name>.{qjsbc,js,wasm}` (TS bytecode → TS bundle → Rust
component, in that preference) and spawns it under its capability profile. Used by
`rusm run` and `rusm dev`. See [the app model](./concepts/app-model).

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | — (required) | Component name → `./wasm/<name>.*`; also registered so a sibling can `spawn` it by name. |
| `capability` | string | `"sandboxed"` | A built-in profile or a `[capabilities.<name>]` id. |
| `restart` | bool | `false` | Restart the component if it exits (supervision). |
| `source` | string? | none | Load the (JS) bundle from a `url:`/`http(s)://` URL or `kv:<bucket>/<key>` instead of the local `./wasm/<name>` artifact — deploy JS live, no node rebuild (re-fetched on each spawn / `rusm dev` reload). See [dynamic bundle sourcing](#dynamic-bundle-sourcing). |

## `[[serve]]` — host as a network server

Each entry hosts a component on its own TCP port. Used by `rusm serve`. Serving is
**always ephemeral**: HTTP and SSE run **a fresh sandboxed instance per request**
(via `http_server`), WS runs **one sandboxed process per connection** (via
`ws_server`). A serving instance never holds state across requests — a trap fails
only that one request/connection. See [the serving model](./concepts/serving-model).

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | — (required) | Component name → `./wasm/<name>.*`. |
| `protocol` | enum | — (required) | `http` · `sse` · `ws`. |
| `listen` | string | — (required) | TCP address to bind, e.g. `"127.0.0.1:8080"`. |
| `capability` | string | `"sandboxed"` | Capability profile. |
| `source` | string? | none | Load the (JS) handler from a URL or `kv:` instead of `./wasm/<name>` — see [dynamic bundle sourcing](#dynamic-bundle-sourcing). |

> **Migration.** Resident serving has been removed — the `mode`, `instances`,
> `shard-by`, and `max-inflight` fields no longer exist. Serving is uniformly
> process-per-request (HTTP/SSE) / process-per-connection (WS). A **stateful**
> handler now lives as a long-lived `[[components]]` service (reached over the actor
> API — `whereis` / `call`) that keeps its state in [`kv`](#dynamic-bundle-sourcing)
> or in process memory; serving instances stay stateless and ephemeral.

## `[serve.routes]` — per-listener HTTP/SSE route table

Each `[[serve]]` HTTP/SSE listener carries its **own** `[serve.routes]` subtable
mapping each HTTP route to a handler **action** in that listener's serving component.
Routing is **declarative config**: you never write a router in handler code. Because
routes belong to a specific listener/port, multiple HTTP listeners (say a public API
on `:8080` and an admin port on `:9090`) each route independently. In TOML,
`[serve.routes]` attaches to the most recent `[[serve]]` entry, so it must sit
immediately after that entry's fields. Required for Rust HTTP/SSE serving components
(the `#[rusm_rs::handlers]` shape); TypeScript HTTP/SSE components handle their own
dispatch via `export default` and need no `[serve.routes]`. WebSocket protocols ignore
the table.

```toml
[[serve]]
name = "api"
protocol = "http"
listen = "127.0.0.1:8080"
capability = "sandboxed"

[serve.routes]                           # this listener's own routes
"GET /" = "api#home"
"GET /users/:id" = "api#show"            # :id captures a path parameter
"POST /plans/:plan/events" = "api#events"
"GET /assets/*" = "api#assets"           # trailing * matches the rest of the path
```

**Key — `"METHOD /path"`:** an uppercase HTTP method, a space, then the path.
Path segments may be:

- **literal** — `users` matches only `users`;
- **a parameter** — `:name` matches one segment and binds it as `name` (read from
  `Params` in the handler);
- **a wildcard** — a trailing `*` matches the remainder of the path (zero or more
  segments).

**Value — `"component#action"`:** the serving component's `name` (its `[[serve]]`
entry), then `#`, then the exported action to invoke. The separator is **`#`**, not
`:`, because `:` is reserved for RUSM scheme syntax (`kv:`, `url:`) elsewhere in the
manifest.

**Matching is most-specific-wins:** a literal segment beats a `:param`, which beats a
`*` wildcard, so overlapping routes resolve deterministically regardless of
declaration order. Resolution semantics:

| Outcome | Result |
| --- | --- |
| A route matches both path and method | dispatch to its `component#action` |
| A path matches but the method does not | **HTTP 405** (Method Not Allowed) |
| No route matches the path | **HTTP 404** (Not Found) |

## `[capabilities.<name>]` — custom capability profiles

Like Cargo's `[profile.<name>]`: a profile **inherits** a built-in base and overrides
only the grants it sets. Default-deny — anything not granted is denied. A
node-registered component runs under **its own** declared profile, whoever spawns it
(the `spawn` capability gates who may spawn; a guest can't fabricate grants the
operator never declared). See [permissions & sandboxing](./concepts/permissions-and-sandboxing).

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `inherits` | string | `sandboxed` | Built-in base: `sandboxed` (deny-all) · `network-client` (+ outbound net) · `trusted` (+ stdio, spawn, process-control, storage, 1 GiB heap). |
| `network` | bool? | from base | Allow outbound network. |
| `spawn` | bool? | from base | May spawn other components by name. |
| `process-control` | bool? | from base | May `kill`/`list`/`info` over foreign pids. |
| `stdio` | bool? | from base | Inherit the host's stdio. |
| `storage` | bool? | from base | May use durable key-value storage (the `kv-*` ABI); needs a node `store`. Granted by `trusted`. |
| `max-memory-mb` | int? | from base | Per-process heap ceiling (MiB). |
| `env` | string[] | `[]` | Env-var **keys** to grant; values resolved from the process env / `.env`. |
| `preopen` | table[] | `[]` | Host dirs mounted in the sandbox: `{ host, guest, read-only }`. |

The three built-in profiles (usable directly as `capability = "..."`): **`sandboxed`**
(CPU + bounded heap only), **`network-client`** (+ outbound network), **`trusted`**
(+ stdio, spawn, process-control, storage, large heap).

## A complete manifest

Every table together — a Rust HTTP API with a routed handler, a long-lived stateful
service, and a custom capability profile:

```toml
# Node
listen = "127.0.0.1:4000"
profile = "balanced"
store = "data/app.redb"            # durable KV — backs `storage` grants and `kv:` sources

# Host the API on a real port (ephemeral instance per request)
[[serve]]
name = "api"                       # → ./wasm/api.wasm
protocol = "http"
listen = "127.0.0.1:8080"
capability = "api-caps"

# This listener's routes → actions in the `api` handler component
[serve.routes]
"GET /" = "api#home"
"GET /users/:id" = "api#show"
"POST /users" = "api#create"
"GET /events" = "api#events"       # an SSE action (3-arg handler) if `api` serves sse
"GET /assets/*" = "api#assets"

# A long-lived, stateful service — reached over the actor API (whereis / call),
# *not* over a port. State that used to live in a "resident" server lives here.
[[components]]
name = "sessions"                  # → ./wasm/sessions.wasm
capability = "trusted"
restart = true

# A custom capability profile for the API handler
[capabilities.api-caps]
inherits = "network-client"
storage = true                     # may read/write the node `store`
max-memory-mb = 128
env = ["API_BASE_URL"]
```

## Dynamic bundle sourcing

A `[[components]]` or `[[serve]]` entry can set **`source`** to load its JS bundle
from somewhere other than the local `./wasm/<name>` artifact — so you deploy new JS
by updating the source, with **no node rebuild**. A `[[components]]` process fetches
its bundle **once** at spawn (and again on each `rusm dev` reload); a `[[serve]]`
endpoint fetches at bind time, then each ephemeral serving instance runs from that
cached bundle.

| `source` | Resolves to |
| --- | --- |
| `https://…` (or `url:<u>`) | an HTTP(S) GET (e.g. a presigned blob or an artifact API); a non-2xx fails loudly |
| `kv:<bucket>/<key>` | an entry in the node's durable `store` (requires `store` to be set) |
| _(omitted)_ | the local `./wasm/<name>` artifact — the default, unchanged |

```toml
store = "data/app.redb"          # kv: sources read from here

[[serve]]
name = "api"
protocol = "http"
listen = "127.0.0.1:8080"
source = "https://cdn.example/api.js"   # deploy by replacing this bundle

[[components]]
name = "worker"
source = "kv:bundles/worker"            # publish to kv, then re-spawn
```

A remote source is always a **JS** bundle (UTF-8). When `source` is omitted the
loader behaves exactly as before, resolving `./wasm/<name>.{qjsbc,js,wasm}`.

## Environment variables

RUSM resolves env **the Rust way**: the process environment first, then a `./.env`
file (`dotenvy`) as a fallback — the real environment always wins. A guest sees a
variable only if its capability profile **grants the key** (via `env = [...]`).

> There is no special config-store; guests read granted variables through the
> standard `wasi:cli/environment`. (Internally the host passes `RUSM_JS_BUNDLE` /
> `RUSM_SERVE_ROLE` to the js-runner — these are not user configuration.)

See also: **[the `rusm` CLI](./reference-cli)** for the commands that consume this file.
