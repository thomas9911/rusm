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
mode = "resident"             # per-request (default) | resident
instances = 4                 # resident pool size
shard-by = "header:x-tenant"  # resident routing affinity (optional)
max-inflight = 256            # per-instance overload cap → 503 (optional)
```

## Node settings

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `listen` | string | `"127.0.0.1:4000"` | The WebSocket address a node's attach endpoint binds (`rusm node start` / `rusm-bench start`). |
| `profile` | enum | `balanced` | The benchmark node's throughput dial — see below. |
| `ticks_per_second` | int (10–60) | `20` | How often the node samples + broadcasts a snapshot. |

**`profile`** is the spawn-throughput dial, relative to your CPU count:

| Profile | Spawn workers | Use it when |
| --- | --- | --- |
| `light` | ~¼ of cores | speed isn't the point; leave the machine alone |
| `balanced` | ~⅖ of cores | good throughput with headroom (the default) |
| `max` | ~½ of cores | peak sustained rate, still smooth (the other half reap) |

It can also be changed live from the dashboard. (CLI override: `--profile`.)

## `[[components]]` — run as supervised processes

Each entry loads `./wasm/<name>.{qjsbc,js,wasm}` (TS bytecode → TS bundle → Rust
component, in that preference) and spawns it under its capability profile. Used by
`rusm run` and `rusm dev`. See [the app model](./concepts/app-model).

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | — (required) | Component name → `./wasm/<name>.*`; also registered so a sibling can `spawn` it by name. |
| `capability` | string | `"sandboxed"` | A built-in profile or a `[capabilities.<name>]` id. |
| `restart` | bool | `false` | Restart the component if it exits (supervision). |

## `[[serve]]` — host as a network server

Each entry hosts a component on its own TCP port. Used by `rusm serve`. HTTP/SSE ride
`http_server`, WS rides `ws_server`. See [the serving model](./concepts/serving-model).

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | — (required) | Component name → `./wasm/<name>.*`. |
| `protocol` | enum | — (required) | `http` · `sse` · `ws`. |
| `listen` | string | — (required) | TCP address to bind, e.g. `"127.0.0.1:8080"`. |
| `capability` | string | `"sandboxed"` | Capability profile. |
| `mode` | enum | `per-request` | `per-request` (fresh sandboxed instance per request/connection — max isolation) or `resident` (a warm, supervised pool that holds state). |
| `instances` | int (≥1) | `1` | Resident pool size (only meaningful for `resident`). |
| `shard-by` | string? | none | Resident affinity: `"header:<name>"` pins same-value requests to one instance; omitted → round-robin. |
| `max-inflight` | int? | none (unbounded) | Resident per-instance cap on concurrent in-flight requests/connections; excess sheds to `503`. |

## `[capabilities.<name>]` — custom capability profiles

Like Cargo's `[profile.<name>]`: a profile **inherits** a built-in base and overrides
only the grants it sets. Default-deny — anything not granted is denied, and a spawned
child never exceeds its spawner. See [permissions & sandboxing](./concepts/permissions-and-sandboxing).

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `inherits` | string | `sandboxed` | Built-in base: `sandboxed` (deny-all) · `network-client` (+ outbound net) · `trusted` (+ stdio, spawn, process-control, 1 GiB heap). |
| `network` | bool? | from base | Allow outbound network. |
| `spawn` | bool? | from base | May spawn other components by name. |
| `process-control` | bool? | from base | May `kill`/`list`/`info` over foreign pids. |
| `stdio` | bool? | from base | Inherit the host's stdio. |
| `max-memory-mb` | int? | from base | Per-process heap ceiling (MiB). |
| `env` | string[] | `[]` | Env-var **keys** to grant; values resolved from the process env / `.env`. |
| `preopen` | table[] | `[]` | Host dirs mounted in the sandbox: `{ host, guest, read-only }`. |

The three built-in profiles (usable directly as `capability = "..."`): **`sandboxed`**
(CPU + bounded heap only), **`network-client`** (+ outbound network), **`trusted`**
(+ stdio, spawn, process-control, large heap).

## Environment variables

RUSM resolves env **the Rust way**: the process environment first, then a `./.env`
file (`dotenvy`) as a fallback — the real environment always wins. A guest sees a
variable only if its capability profile **grants the key** (via `env = [...]`).

> There is no special config-store; guests read granted variables through the
> standard `wasi:cli/environment`. (Internally the host passes `RUSM_JS_BUNDLE` /
> `RUSM_SERVE_ROLE` to the js-runner — these are not user configuration.)

See also: **[the `rusm` CLI](./reference-cli)** for the commands that consume this file.
