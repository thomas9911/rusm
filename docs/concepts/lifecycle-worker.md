# Lifecycle — Worker component (non-serving, per-call)

A short-lived process a sibling `spawn`s to do **one unit of work** and exit. No socket
and no listener — pure actor work, off the serving path. See the
[overview](./component-lifecycle.md) for the shared two-domain model and failure
vocabulary.

## Shape (what you write)

```rust
#[rusm_rs::main]
fn run() {
    let job = rusm_rs::receive_bytes();      // the work item (blocks; the fiber parks)
    let result = do_work(&job);              // your logic
    if let Some(caller) = /* … */ { rusm_rs::send_bytes(caller, &result); }
    // returning ends the process — it exits Normal
}
```

A worker is spawned on demand by another process (`rusm_rs::spawn("worker")`), does its
job — optionally streaming results back over a [byte stream](./byte-streams.md) or
messages — and returns. The dispatch-from-`commander` pattern in the
[app model](./app-model.md) is exactly this.

## Platform owns / you write

- **Platform owns:** the **capability-gated, non-escalating** spawn (a child inherits
  *exactly* its parent's capabilities — never more), delivering the work message,
  scheduling/parking the fiber on blocking calls, and notifying monitors/links on exit.
- **You write:** receive the job, do it, (optionally) reply, return.

## Lifecycle events

| Event | Platform domain | Application domain | Result |
| --- | --- | --- | --- |
| **Normal** | spawn → deliver the job → reclaim on return | does the work, returns | result delivered; the process is gone |
| **Spawn denied** | a parent without the `spawn` capability gets an error (not a new process) | the `spawn(...)` call returns `Err` | no worker — the parent decides what to do |
| **Crash (trap)** | the process is Crashed; a spawner that `monitor`ed it gets a `Down(reason)`; a **linked** spawner gets an exit signal (or an exit cascade) | the `panic!` / `.unwrap()` | surfaced to the spawner: retry / give up / escalate |
| **Memory crash (OOM)** | the `StoreLimiter` cap trips a trap → Crashed → `Down` | exceeded `max-memory-mb` | same — surfaced to the spawner |
| **Kill** (parent died, via a link) | the exit cascade fires this process's abort handle | — | reclaimed; no orphan |

## Notes

- **Supervision is opt-in and yours to shape.** A worker isn't auto-restarted; the
  *spawner* chooses the policy by `monitor`ing it (a dead child arrives as a `__down`
  message) or by putting it under an in-guest
  [`Supervisor`](./links-and-supervision.md) with a one-for-one / rest-for-one
  strategy.
- **No escalation of privilege.** Because the spawn is non-escalating, a worker can
  never reach beyond what its spawner was granted — capability scope only narrows down
  the spawn tree.
- **Concurrency = many workers.** A guest is single-threaded; you get parallelism by
  spawning *more* workers (each its own process/instance), not by threading inside one.

Prev: [WebSocket component](./lifecycle-websocket.md) · Next: [Service component](./lifecycle-service.md)
