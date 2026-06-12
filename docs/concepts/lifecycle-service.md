# Lifecycle — Service component (non-serving, resident)

A long-lived, **stateful** process — a `#[rusm_rs::service]` holding state (a counter, a
cache, a pub/sub hub), reached via `whereis` + `call`/`send`. **This is where "resident
vs per-call" lives — never in the serving tier.** See the
[overview](./component-lifecycle.md) for the shared two-domain model and failure
vocabulary.

## Shape (what you write)

```rust
#[rusm_rs::service]
pub mod counter {
    // state lives in the module; the macro writes the receive→dispatch→reply loop
    pub fn bump(by: u64) -> u64 { /* mutate + return */ }
    pub fn total() -> u64       { /* read */ }
}
```

Declared as a `[[components]]` entry (with `restart = true` to be supervised), spawned
when the node starts, and addressed by name. A sibling calls it through the generated
typed client; the round-trip reads like a local call.

## Platform owns / you write

- **Platform owns:** spawning it at node start, the mailbox, **reply routing** for each
  `call` (matching a reply to its request), the **supervisor + restart policy**, and
  parking the fiber on `receive` between requests.
- **You write:** the handler functions and the state they own (the macro writes the
  `serve()` dispatch loop).

## Lifecycle events

| Event | Platform domain | Application domain | Result |
| --- | --- | --- | --- |
| **Normal run** | blocks the fiber on `receive` between requests; routes each `call`'s reply back to its caller | handles calls/casts, mutates its own state | serves until shutdown |
| **Graceful shutdown** | signals shutdown; lets the loop drain | finishes in-flight work, returns | clean stop |
| **Crash (trap)** | Crashed → the **supervisor restarts it with fresh state** (per intensity); monitors get `Down`, links get an exit signal | the `panic!` / `.unwrap()` | restarted (state reset); a sibling's in-flight `call` **fails** — its reply ref is never answered, so the caller's `receive`-with-timeout returns an error rather than hanging |
| **Memory crash (OOM)** | the `StoreLimiter` cap trips a trap → Crashed → restart | exceeded `max-memory-mb` | restarted; state reset |
| **Restart storm** | restart-intensity window exceeded → the supervisor **escalates** (gives up) rather than crash-loop | (a persistent bug) | the failure surfaces upward |
| **Kill** (explicit or via a link) | abort handle fires; the signal rides the mailbox | — | reclaimed (restarted if supervised) |

## Notes

- **State resets on restart — by design.** "Let it crash": a supervised service comes
  back clean rather than limping on corrupt state. If state must survive a restart, put
  it in durable `kv`, not the process heap.
- **A call to a dead service fails, it doesn't hang.** The reply ref is never answered,
  so a caller using `call`-with-timeout gets a clear error and can react — back-pressure
  for failure, not a silent stall.
- **The home for shared state.** [HTTP](./lifecycle-http.md),
  [SSE](./lifecycle-sse.md), and [WebSocket](./lifecycle-websocket.md) components are
  stateless and disposable; anything they need to share (a cache, a broker, a session
  map) lives in a service like this and is reached by message.

Prev: [Worker component](./lifecycle-worker.md) · Overview: [Component lifecycles](./component-lifecycle.md)
