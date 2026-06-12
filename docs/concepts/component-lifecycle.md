# Concept — the component lifecycle (every type, success & failure)

Every component in RUSM — a long-lived service, a per-request HTTP handler, a
per-connection WebSocket worker, an SSE stream, a one-shot command — is the **same
runtime object: a process**. One Wasm instance = one Tokio task, one mailbox, one
fiber, one capability set. So the lifecycle is *uniform*; what differs between
component types is only **when** a process is spawned and **how** it ends.

This page walks the whole lifecycle — first the phases that are common to all
components, then each component type's **success flow** and **error flow**, and
finally the isolation guarantees that make a failure local.

## The universal phases

```
 register ──▶ spawn ──▶ run ──▶ (block ⇄ work) ──▶ exit ──▶ reclaim
                │                                     │
          optimized path                    Normal · Crashed · Killed
        (pool + CoW + InstancePre)                    │
                                              links · monitors · supervisor
```

1. **Register** — a component is known by name (the `[[components]]` loader, a
   `[[serve]]` handler, or a guest calling `register`). A name lookup resolves to a
   prepared component (the compiled module + linked imports), so spawning never
   recompiles.
2. **Spawn** — the spawn hot path instantiates on a pooling allocator with
   copy-on-write memory and a per-module `InstancePre` + precomputed export index, so
   a fresh instance costs microseconds (~440k component spawns/sec). The process gets
   its **capabilities** (default-deny, **non-escalating** — a child never exceeds its
   parent) and a `StoreLimiter` memory ceiling. It becomes a real
   [process](./wasm-instance-as-process.md): a mailbox + an abort handle, scheduled as
   one Tokio task.
3. **Run** — the guest's entrypoint executes. **Blocking is async**
   ([fibers](./fibers-and-blocking-to-async.md)): `receive`, `Stream::read`, a service
   `call` — each *suspends the fiber and frees the scheduler thread* until data
   arrives, then resumes. You write straight-line blocking code; the runtime makes it
   cooperative. [Epoch preemption](./epoch-preemption.md) keeps a CPU-bound guest from
   monopolising a thread.
4. **Work** — messages arrive in the single mailbox as a `Received` value (a user
   message, a byte stream, or an exit signal); the process handles them in order.
5. **Exit** — the process ends in exactly one of three ways:
   - **Normal** — `run` returned. A clean, expected finish.
   - **Crashed** — a Wasm **trap**: a guest panic, `unreachable`, a `StoreLimiter`
     memory-cap hit (OOM), or a capability violation that traps. The exit reason
     carries the trap.
   - **Killed** — an external `kill` (or an exit cascade from a link). A `futures`
     abort handle tears the fiber down; the kill rides the same mailbox, so there is
     no second signal channel.
6. **Reclaim** — the instance returns to the pool, the mailbox drops, byte streams
   close. **Links** fire exit signals to linked peers (an exit cascade, unless the
   peer set `trap_exit`); **monitors** receive a `Down` carrying the reason; a
   **supervisor** restarts per its strategy. None of this leaks — a dropped process
   takes its instance, mailbox, and streams with it.

### Supervision & restart (the error backbone)

A supervised child is linked to its supervisor. On a `Crashed`/`Killed` exit the
supervisor applies its strategy — **one-for-one** (restart just that child),
**one-for-all** (restart the whole set), or **rest-for-one** (restart that child and
those started after it) — bounded by **windowed restart intensity**: too many restarts
within the window and the supervisor itself gives up (escalating the failure upward)
rather than crash-looping forever. Guests get the same supervisor (over a `monitor`
ABI; a dead child arrives as a `__down` message — no polling).

---

## Per-request HTTP handler (`#[rusm_rs::handlers]` / `wasi:http`)

A fresh sandboxed instance **per request**; it handles exactly one request and exits.

**Success flow**

1. A request arrives on the listener. The host resolves it against the listener's
   [`[serve.routes]`](../serving-http-ws-sse.md) table → `component#action` + captured
   path params.
2. The gateway **spawns the handler fresh** on the optimized path and dispatches the
   matched action over the actor wire (an ephemeral Wasm-free *responder* process owns
   the reply).
3. The action returns a `Response`; the responder turns it into the HTTP response.
4. `run` returns → the handler exits **Normal** → the instance is reclaimed. The next
   request is a brand-new instance — no shared state, no carry-over.

**Error flow**

| Failure | What the client sees | What happens to the process |
| --- | --- | --- |
| No route matches the path | **404** | no handler is spawned |
| Path matches, wrong method | **405** | no handler is spawned |
| Route names an unregistered component | **500** | no handler is spawned |
| Handler **traps** mid-request (panic / OOM / `unreachable`) | **502** | process **Crashed**; the responder's reply channel drops, so the gateway answers 502 |
| Handler returns an error status (e.g. `Response::new(400, …)`) | **400** | a normal reply — **not** a crash |

A trap drops **only that one request**. The listener keeps accepting; other in-flight
requests are untouched (each is its own instance). Per-request handlers aren't
supervised — there's nothing to restart, because the next request simply gets a fresh
instance.

## SSE streaming handler (`fn(Request, Params, Sse)`)

A per-request process that streams a `text/event-stream` body for the life of one
connection.

**Success flow** — spawn → reply a streaming head → open a **bounded, back-pressured**
byte stream to the responder → write events (`sse.data(…)`, or `sse.run(hb, map)` to
live-tail a topic with heartbeats). When the producer outruns the consumer the write
**parks the fiber** (back-pressure) — it never busy-spins. On end-of-feed or client
disconnect the stream closes, `run` returns, the process exits **Normal**.

**Error flow** — a **client disconnect** makes the next write return `false`; the loop
breaks, the process exits, and (if it had subscribed to a broker) the broker's
`monitor` prunes it on the resulting `Down`. A **trap** mid-stream Crashes the process;
its stream writer drops, so the chunked body simply ends — only that connection is
affected. A finite/endless feed can never peg a core (bounded channel + parking write),
and a disconnect always tears the process down promptly — both are regression-guarded
by tests.

## Per-connection WebSocket component

One sandboxed component process **per connection**.

**Success flow** — the socket upgrades → a process is spawned → message 1 is the
**writer pid** (the Wasm-free process that owns the socket sink) → each inbound frame
arrives as a mailbox message → the handler replies with `conn.send(…)` → the client
closes → the host kills the process → **Normal/Killed** exit, socket and writer
reclaimed.

**Error flow** — a handler **trap** Crashes that connection's process; the host tears
down its writer and closes that socket. Every other connection and the listener itself
are untouched — connections share nothing, so a crash is contained to one client.

## `[[components]]` service (long-lived, optionally `restart = true`)

A resident, stateful process — a `#[rusm_rs::service]` (or TS service) holding state
(a counter, a cache, a pub/sub hub) and reached via `whereis` + `call`/`send`.

**Success flow** — spawned when the node starts → registers its name → its `serve()`
loop blocks on `receive`, handling calls/casts, until node shutdown (a graceful
`shutdown` lets it drain).

**Error flow** — a **trap** Crashes it → if `restart = true` the supervisor restarts it
(fresh state) under [windowed intensity](./links-and-supervision.md); monitors get a
`Down`, linked peers get an exit signal. An in-flight `call` from a sibling to a
crashed service **fails** (the reply ref is never answered → the caller's
`receive`-with-timeout returns an error) rather than hanging forever. If restarts
exceed the window, the supervisor escalates instead of crash-looping. This is where
"resident vs per-call" lives — **not** in the serving tier.

## `[[components]]` per-call worker

A short-lived process a sibling spawns to do one unit of work and exit.

**Success flow** — a sibling `spawn`s it (capability-gated, non-escalating) → it does
its work (maybe streaming results back over a byte stream or messages) → it exits
**Normal**. **Error flow** — a trap Crashes it; a spawner that `monitor`ed it gets a
`Down` and decides what to do (retry, give up, escalate).

## `wasi:cli` command component

A stock command component (`WasmRuntime::spawn_command`) — no actor world, just
`wasi:cli/run`.

**Success flow** — spawned → runs `run` → exits with code **0** → **Normal**.
**Error flow** — a non-zero exit or a trap → **Crashed**, the exit reason carrying the
status. Same isolation and reclaim as any process.

---

## Why a failure stays local

The lifecycle is built so that the blast radius of an error is exactly **one unit of
work**:

- **Isolation by instance** — each request/connection/worker is its own Wasm instance
  with its own linear memory and capabilities. A trap can corrupt only that instance's
  memory, which is thrown away on exit.
- **No head-of-line blocking** — because serving is process-per-unit, a slow or stuck
  handler never holds up the listener or other clients.
- **Crash containment** — a trap becomes a `Crashed` exit, surfaced as a status code
  (502) or a `Down`/exit signal — never a node-wide failure. Supervisors decide whether
  to restart; per-request work just gets a fresh instance next time.
- **No leaks, no spins** — exit reclaims the instance, mailbox, and streams; byte
  streams are bounded and back-pressured, so a producer parks rather than spins, and a
  disconnect tears the process down.

See also: [the process model](./wasm-instance-as-process.md),
[links & supervision](./links-and-supervision.md),
[fibers & blocking→async](./fibers-and-blocking-to-async.md),
[the serving model](./serving-model.md), and
[permissions & sandboxing](./permissions-and-sandboxing.md).
