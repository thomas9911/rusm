# Reference — configuration (`rusm.toml` & env)

Everything the `rusm` CLI reads at startup. Two inputs, layered:

1. **`rusm.toml`** — the node + app manifest (this page).
2. **Environment** — process env first, then a `./.env` file (`dotenvy`); real env wins.

**Layering for node settings:** built-in defaults → `rusm.toml` → CLI flags
(`--listen`, `--profile`). So a flag overrides the file, and the file overrides the
default. Unknown keys are **rejected** (a typo is an error, not a silent no-op).

## The file at a glance

Tables read top-down in the order an app is built: the node, the listener and its
routes, the capability profiles, then the components those routes and profiles name.

```toml
# Node — the attach endpoint (`rusm node start`) and the benchmark node (`rusm-bench start`)
listen = "127.0.0.1:4000"     # WebSocket address the node's attach endpoint binds
profile = "balanced"          # light | balanced | max  — the benchmark node's throughput dial
ticks_per_second = 20         # snapshot rate, 10–60 Hz

# A network listener (`rusm serve`) — just a port + its routes
[[serve]]
protocol = "http"             # http | sse | ws
listen = "127.0.0.1:8080"

# This listener's HTTP/SSE routes → actions on a [components.<name>] handler
[serve.routes]
"GET /" = "api#home"
"GET /users/:id" = "api#show"

# A custom capability profile (default-deny; inherits a built-in, overrides grants)
[capabilities.agent]
inherits = "network-client"
allow-spawn = true
max-memory-mb = 256
env = ["OPENAI_API_KEY"]
preopen = [{ host = "./data", guest = "/data", read-only = false }]

# Components, keyed by name — registered for spawn-by-name; `resident` ones
# are boot-spawned + supervised (`rusm run` / `rusm dev`)
[components.api]               # the routed handler — loads ./wasm/api.{qjsbc,js,wasm}
capability = "agent"

[components.calc]
capability = "sandboxed"
resident = true               # long-lived service: boot-spawned + supervised
```

## Node settings

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `listen` | string | `"127.0.0.1:4000"` | The WebSocket address a node's attach endpoint binds (`rusm node start` / `rusm-bench start`). |
| `profile` | enum | `balanced` | The benchmark node's throughput dial — see below. |
| `ticks_per_second` | int (10–60) | `20` | How often the node samples + broadcasts a snapshot. |
| `store` | string? | none | Path (relative to the app dir) to the node's embedded durable key-value store — one file the node owns, no daemon. Required for components granted `allow-storage` (the `kv` ABI) or a `kv:` bundle `source`. Omitted → no store. |

**`profile`** is the spawn-throughput dial, relative to your CPU count:

| Profile | Spawn workers | Use it when |
| --- | --- | --- |
| `light` | ~¼ of cores | speed isn't the point; leave the machine alone |
| `balanced` | ~⅖ of cores | good throughput with headroom (the default) |
| `max` | ~½ of cores | peak sustained rate, still smooth (the other half reap) |

It can also be changed live from the dashboard. (CLI override: `--profile`.)

## `[log]` — platform & guest logging

Opt-in, **off by default**, declared explicitly (no env magic). One level gates **all
three** of: the runtime's own lifecycle lines (each component process as it spawns and
exits); your **guests' logs** (`console.*` in TS / the `log` crate in Rust); and a
**serving access log** (every HTTP request, SSE stream, and WS upgrade). All route to the
platform logger, which stamps the time, the calling `component#pid` (or `rusm` for the
runtime's own lines), and a severity colour — so everything reads as one aligned stream.
Guests wire **nothing** — no name, pid, or logger object; the host knows which process is
calling.

```toml
[log]
level = "debug"      # off | error | warn | info | debug
```

```text
rusm       spawn  meta-json#0   net spawn storage env=3 mem=64M   ← platform lifecycle line
meta-json#0 info  meta-json ready (sink + broker)                 ← a guest's console.log / log::info!
rusm       http   GET / → 200                                     ← a served HTTP request (access log)
rusm       sse    GET /stream/app → 200                           ← an SSE stream
rusm       ws     GET /socket → 101                               ← a WS upgrade
commander#7 warn  retrying meta-json (attempt 2)                  ← a guest's console.warn / log::warn!
rusm       exit   api#7         normal
```

| `level` | Shows (platform lifecycle · guest logs · access log) |
| --- | --- |
| `off` (default) | nothing — zero overhead |
| `error` | crashes (a trap / OOM) · `console.error` / `log::error!` |
| `warn` | + kills and link cascades · `console.warn` / `log::warn!` |
| `info` | + clean exits · `console.log`/`console.info` / `log::info!` · **every served request** (`http`/`sse`/`ws`, status coloured by class) |
| `debug` | + every spawn (full visibility) · `console.debug` / `log::debug!` |

Levels are cumulative. A **restart** needs no special event — it reads as a crash
`exit` (red) followed by a fresh `spawn` for the same component (a new pid). Only
named components are logged (not internal plumbing), and the spawn line's capability
summary makes a process's real privileges visible at the moment it starts. A guest's log
line is **not** stdout — it needs no `allow-stdio` grant; logging is a platform primitive
gated only by this level.

## `[[serve]]` — a network listener

Each entry is a **pure listener** on its own TCP port — just a `protocol` and a
`listen` address. Used by `rusm serve`. Serving is **always ephemeral**: HTTP and SSE
run **a fresh sandboxed instance per request**, WS **one sandboxed process per
connection** — a trap fails only that one request/connection, never the listener. See
[the serving model](./concepts/serving-model).

A listener carries no handler logic of its own. The **handler components live in
`[components.<name>]`** (each with its own capability), and the listener reaches them
one of two ways:

- **routed** (the usual HTTP/SSE shape) — a [`[serve.routes]`](#serveroutes-per-listener-httpsse-route-table)
  table names a handler action per route; the listener needs no `name`.
- **single-handler** — a WebSocket listener, or a routes-less `wasi:http` HTTP
  component (e.g. a TS `export default { fetch }`), names its one handler via `name`.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `protocol` | enum | — (required) | `http` · `sse` · `ws`. |
| `listen` | string | — (required) | TCP address to bind, e.g. `"127.0.0.1:8080"`. |
| `name` | string? | none | The single handler **component** for a listener with **no** `[serve.routes]` (a WebSocket listener, or a routes-less `wasi:http` HTTP component). Resolves to `./wasm/<name>.*`; its capability comes from a matching `[components.<name>]` entry, else `sandboxed`. **Omitted** for a routed HTTP/SSE listener — its routes name the handlers. |
| `source` | string? | none | Load the named handler's (JS) bundle from a URL or `kv:` instead of `./wasm/<name>` — see [dynamic bundle sourcing](#dynamic-bundle-sourcing). |

> **Migration.** A `[[serve]]` entry is now a pure listener. Its old fields are gone:
> `capability` (the handler's profile lives on its `[components.<name>]` entry),
> required `name` (routed listeners name handlers via `[serve.routes]`), and the
> resident-serving knobs (`mode` / `instances` / `shard-by` / `max-inflight`). Serving
> is uniformly process-per-request (HTTP/SSE) / process-per-connection (WS); a
> **stateful** handler now lives as a long-lived `[components.<name>]` service
> (`resident = true`, reached over the actor API — `whereis` / `call`) that keeps its
> state in [`kv`](#dynamic-bundle-sourcing) or process memory, so serving instances
> stay stateless and ephemeral.

## `[serve.routes]` — per-listener HTTP/SSE route table

Each HTTP/SSE `[[serve]]` listener carries its **own** `[serve.routes]` subtable
mapping each route to a handler **action**. Routing is **declarative config** — you
never write a router in handler code. Because routes belong to a specific
listener/port, multiple HTTP listeners (say a public API on `:8080` and an admin port
on `:9090`) route independently. In TOML, `[serve.routes]` attaches to the most recent
`[[serve]]` entry, so it must sit immediately after that entry's fields. Required for
Rust HTTP/SSE components (the `#[rusm_rs::handlers]` shape); TypeScript HTTP/SSE
components dispatch via `export default` and need none; WebSocket listeners ignore it.

```toml
[[serve]]
protocol = "http"
listen = "127.0.0.1:8080"

[serve.routes]                           # this listener's own routes
"GET /" = "api#home"
"GET /users/:id" = "api#show"            # :id captures a path parameter
"POST /plans/:plan/events" = "api#events"
"GET /assets/*" = "api#assets"           # trailing * matches the rest of the path

[components.api]                         # the handler the routes name (its own caps)
capability = "sandboxed"
```

**Key — `"METHOD /path"`:** an uppercase HTTP method, a space, then the path.
Path segments may be:

- **literal** — `users` matches only `users`;
- **a parameter** — `:name` matches one segment and binds it as `name` (read from
  `Params` in the handler);
- **a wildcard** — a trailing `*` matches the remainder of the path (zero or more
  segments).

**Value — `"component#action"`:** the handler **component's name** (a
`[components.<name>]` entry), then `#`, then the exported action to invoke. The
separator is **`#`**, not `:`, because `:` is reserved for RUSM scheme syntax (`kv:`,
`url:`) elsewhere in the manifest.

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
(the `allow-spawn` capability gates who may spawn; a guest can't fabricate grants the
operator never declared). See [permissions & sandboxing](./concepts/permissions-and-sandboxing).

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `inherits` | string | `sandboxed` | Built-in base: `sandboxed` (deny-all) · `network-client` (+ outbound net) · `trusted` (+ stdio, spawn, process-control, storage, 1 GiB heap). |
| `allow-network` | bool? | from base | Allow outbound network (e.g. `fetch`). |
| `allow-spawn` | bool? | from base | May spawn other components by name. |
| `allow-process-control` | bool? | from base | May `monitor`/`kill`/`list`/`info` over foreign pids. |
| `allow-stdio` | bool? | from base | Inherit the host's raw stdout/stdin (a `wasi:cli` command, a raw `print!`). **Not** needed for logging — `console.*` / `log::*` route to the platform logger, gated by `[log]` (above). |
| `allow-storage` | bool? | from base | May use durable key-value storage (the `kv-*` ABI); needs a node `store`. Granted by `trusted`. |
| `max-memory-mb` | int? | from base | Per-process heap ceiling (MiB). |
| `env` | string[] | `[]` | Env-var **keys** to grant; values resolved from the process env / `.env`. |
| `preopen` | table[] | `[]` | Host dirs mounted in the sandbox: `{ host, guest, read-only }`. |

Every `allow-*` grant is a boolean override on the inherited base; the non-grant keys
(`inherits`, `max-memory-mb`, `env`, `preopen`) shape the rest of the profile. The
three built-in bases are usable directly as `capability = "..."`: **`sandboxed`** (CPU
+ bounded heap only), **`network-client`** (+ outbound network), **`trusted`** (+
stdio, spawn, process-control, storage, large heap).

## `[components.<name>]` — registered, optionally resident components

Keyed by the component **name** (the table key, like `[capabilities.<name>]` — there is
no `name` field). Each entry loads `./wasm/<name>.{qjsbc,js,wasm}` (TS bytecode → TS
bundle → Rust component, in that preference) and **registers** it under its capability
profile so a route or a sibling can `spawn` it by name. Used by `rusm run`, `rusm dev`,
and as the handlers a `[[serve]]` listener names. See [the app model](./concepts/app-model).

Every entry is **spawnable by name**. The `resident` flag decides whether the node also
boots an instance:

- **`resident = true`** — a long-lived service: boot-spawned at startup **and
  supervised** (auto-restarted on crash, bounded by the runtime's restart-intensity so
  a crash-loop is capped). Use it for stateful services reached over the actor API.
- **default (no `resident`)** — registered for **spawn-by-name only**: a route or a
  sibling spawns it on demand (a per-request HTTP handler, an on-demand worker). It is
  **not** boot-spawned — no idle parked instance.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| _(table key)_ | string | — (required) | The component **name** (`[components.<name>]`) → `./wasm/<name>.*`; registered so a route or sibling can `spawn` it by name. |
| `capability` | string | `"sandboxed"` | A built-in profile or a `[capabilities.<name>]` id. |
| `resident` | bool | `false` | `true` = boot-spawned at startup and supervised (auto-restarted on crash). Default = registered for spawn-by-name only, not boot-spawned. |
| `source` | string? | none | Load the (JS) bundle from a `url:`/`http(s)://` URL or `kv:<bucket>/<key>` instead of the local `./wasm/<name>` artifact — deploy JS live, no node rebuild (re-fetched on each spawn / `rusm dev` reload). See [dynamic bundle sourcing](#dynamic-bundle-sourcing). |

## A complete manifest

Every table together — a Rust HTTP API with a routed handler, a long-lived stateful
service, and a custom capability profile, in canonical order:

```toml
# Node
listen = "127.0.0.1:4000"
profile = "balanced"
store = "data/app.redb"            # durable KV — backs `allow-storage` grants and `kv:` sources

# Host the API on a real port — a pure listener (ephemeral instance per request)
[[serve]]
protocol = "http"
listen = "127.0.0.1:8080"

# This listener's routes → actions on the `api` handler component (below)
[serve.routes]
"GET /" = "api#home"
"GET /users/:id" = "api#show"
"POST /users" = "api#create"
"GET /events" = "api#events"       # an SSE action (3-arg handler) if `api` serves sse
"GET /assets/*" = "api#assets"

# A custom capability profile for the API handler
[capabilities.api-caps]
inherits = "network-client"
allow-storage = true               # may read/write the node `store`
max-memory-mb = 128
env = ["API_BASE_URL"]

# The HTTP handler the routes name — spawned per request, so no `resident`.
[components.api]                   # → ./wasm/api.wasm
capability = "api-caps"

# A long-lived, stateful service — boot-spawned + supervised, reached over the
# actor API (whereis / call), *not* over a port. State that used to live in a
# "resident" server lives here.
[components.sessions]              # → ./wasm/sessions.wasm
capability = "trusted"
resident = true
```

## Dynamic bundle sourcing

A `[components.<name>]` or `[[serve]]` entry can set **`source`** to load its JS bundle
from somewhere other than the local `./wasm/<name>` artifact — so you deploy new JS
by updating the source, with **no node rebuild**. A `[components.<name>]` process fetches
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

# A routes-less HTTP listener names its one handler — loaded from a remote bundle
[[serve]]
protocol = "http"
listen = "127.0.0.1:8080"
name = "api"
source = "https://cdn.example/api.js"   # deploy by replacing this bundle

[components.worker]
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
