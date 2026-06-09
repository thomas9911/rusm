# Reference — the `rusm` CLI

One binary, `rusm`, drives the whole lifecycle of a RUSM app. The arc:

```sh
rusm new myapp     # scaffold
cd myapp
rusm build         # components/* → ./wasm/*
rusm serve         # host them on real ports     (or)   rusm run   # run as processes
rusm dev           # build + run + watch & reload (iterate)
rusm attach        # live REPL into a running node
```

Config comes from `rusm.toml` (see **[configuration](./reference-configuration)**);
the commands that start a node also accept the flags in the last section.

## `rusm new <name>`

Scaffold a new app in `./<name>` — a zero-dependency TypeScript HTTP component, a
`rusm.toml` with a `[[serve]]` entry, `.gitignore`, and a README. From nothing to a
live server in three commands.

```sh
rusm new hello && cd hello
rusm build
rusm serve              # → http://127.0.0.1:8080
```

## `rusm build`

Compile every `components/<name>/` into `./wasm/`, with **one toolchain each** — no
jco, no cargo-component:

- a **Rust** component (`Cargo.toml`) → `cargo build --target wasm32-wasip2` → `wasm/<name>.wasm`;
- a **TypeScript** component (`index.ts`) → `bun build --minify` → `wasm/<name>.js`,
  then **precompiled to QuickJS bytecode** → `wasm/<name>.qjsbc` (the runner skips
  parsing). See [guests: Rust & TypeScript](./concepts/guests-rust-and-typescript).

Emits a clear error if Bun / the `wasm32-wasip2` target is missing.

## `rusm run`

Load every `[[components]]` entry from `./wasm/` and spawn it as a supervised process
under its capability profile; waits for Ctrl-C. Loads `./.env` (process env wins).

```sh
rusm run
# running 2 component(s): calc, commander
```

## `rusm serve`

Host every `[[serve]]` entry on its TCP `listen` address — HTTP/SSE via `http_server`,
WS via `ws_server`, per-request or resident per the entry's `mode`. Prints each bound
endpoint; waits for Ctrl-C. This is the **server** side of a fair benchmark — the node
only serves; drive load out-of-process with `rusm-loadtest`.

```sh
rusm serve
# serving 1 endpoint(s):
#   api              http://127.0.0.1:8080
```

## `rusm dev`

`build` → `run` → **watch `./components`** and rebuild + hot-reload on any edit (a
dependency-free mtime scan). The fast inner loop.

```sh
rusm dev
# running 2 component(s); watching ./components — edit to reload, Ctrl-C to stop
# change detected — rebuilding…
```

## `rusm node start`

Start the **benchmark/observer node** — the WebSocket server the dashboard and
`rusm attach` connect to. This is the node behind the live dashboard, not an app
server.

```sh
rusm node start --profile max
# rusm node listening on ws://127.0.0.1:4000 (profile: max, 20 Hz)
```

## `rusm attach [target]`

Open a live REPL into a running node (defaults to `127.0.0.1:4000`; accepts
`host`, `host:port`, or a full `ws://` URL — local or remote). Run scenarios, stop
them, toggle the observer, switch the profile, and watch frames stream in. See
[live attach](./concepts/live-attach).

```sh
rusm attach                 # local node
rusm attach 10.0.0.7:4000   # a remote node
# attached — type `help` for commands
> run spawn-storm
> stop
```

## Flags

Applied by the node-starting commands (layered over `rusm.toml`):

| Flag | Commands | Meaning |
| --- | --- | --- |
| `--config <file>` | `node start`, `run`, `serve`, `dev` | Use a specific manifest instead of `./rusm.toml`. |
| `--listen <addr>` | `node start` | Override the node's WebSocket address. |
| `--profile light\|balanced\|max` | `node start` | Override the startup throughput profile. |

> `rusm new` takes the app name; `rusm attach` takes the target as a positional
> argument; `rusm build` takes no flags.
