# ts-app — a TypeScript RUSM app (service + concealed typed client)

A two-component TS app, built by **Bun** and run on RUSM:

- **`calc`** — a *service* component: it just `export`s functions; RUSM runs the
  request → dispatch → reply loop around them.
- **`commander`** — a *worker* (`export default`): it `spawn`s the calc service by
  name and calls it through the **typed client** — `await calc.add(2, 3)` reads
  like a function call, but it's spawn + send + receive, all hidden.

## Run it

```sh
cd examples/ts-app
rusm build      # Bun bundles each components/<name>/index.ts → wasm/<name>.js
rusm run        # loads ./wasm/* and runs them under their capability profiles
```

Expected output:

```
running 2 component(s): calc, commander
press Ctrl-C to stop
2 + 3 = 5
hi RUSM
counted: 1,2,3
work done after 25/50/100
```

The last two lines show the typed client's **streaming** (`for await` a generator
handler) and **callbacks** (a function argument that stays in the caller, invoked
as the service reports progress).

## How it works

`rusm build` detects each `components/<name>/index.ts` and runs
`bun build --format=cjs` → `wasm/<name>.js` (a Rust component would build to
`wasm/<name>.wasm` instead — same manifest, same loader). At runtime a `.js`
artifact runs on the shared rquickjs **js-runner**.

`rusm.toml` declares both components and a custom `orchestrator` capability profile
(it `inherits = "trusted"`, so the commander may `spawn` and use stdio). The `calc`
service is registered by name so the commander can spawn its own instance on demand;
the spawned child never exceeds the spawner's capabilities.

The `Process` actor API and the `spawn<T>()` typed client are typed by `rusm.d.ts`
(copied here from the runner). See `components/*/index.ts`.
