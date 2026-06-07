# Concept — the RUSM app model

A RUSM **app** is a project that declares some components and lets the runtime
build, load, supervise, and run them — so you write *source*, not glue.

## Layout

```
my-app/
├── rusm.toml          # node config + [[components]] (name, capability, restart)
├── components/        # SOURCE — a cargo workspace, one crate per component
│   └── <name>/        # Rust (and TS via Bun + rquickjs, embedded — no jco)
└── wasm/              # BUILT, ready-to-run .wasm (also drop 3rd-party .wasm here)
```

The manifest refers to components **by name**; `./wasm/` is the enforced
load directory.

## Commands

- **`rusm build`** — discovers `components/<name>/`, builds each with one
  toolchain (`cargo build --target wasm32-wasip2`, which componentizes — no jco,
  no cargo-component), and emits `./wasm/<name>.wasm`.
- **`rusm run`** — loads each manifest component, spawns it under its capability
  profile, and supervises it per its restart policy. Code can also spawn
  dynamically — both work together.
- **`rusm dev`** — build, then run (filesystem watch/reload is a follow-on).

## Environment — KISS, the Rust way

Env vars resolve **process environment first, then a `.env`** fallback (via
`dotenvy`; the real environment wins), exposed to guests through the standard
`wasi:cli/environment` — gated by the capability profile (default-deny: grant keys
or all). No wasmCloud-style `wasi:config/store`.

> Shipped in Phase 7 (`rusm-cli`).
