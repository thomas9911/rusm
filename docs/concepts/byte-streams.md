# Concept — cross-process byte streams

Messages are whole values: you `send` a chunk, the receiver gets that chunk. But
some work is a *flow* — an HTTP body, an SSE feed, a file being piped — where the
producer keeps emitting and the consumer keeps reading, and neither should have to
buffer the whole thing in memory. RUSM models that as a **byte stream** between
processes.

## A bounded channel, the actor way

A stream is a **bounded Tokio channel** of byte chunks (`StreamHandle` in the
Wasm-free `rusm-otp` core). The read end travels in a message —
`Received::Stream` — moving ownership to the recipient exactly like any other
message. So streams are pure actor composition: no shared memory, no new wiring.

## Back-pressure for free

Because the channel is bounded, a slow reader automatically slows the writer: the
writer's `stream_write` simply **parks its fiber** until there's room (see
[fibers & blocking→async](./fibers-and-blocking-to-async.md)). No busy-polling, no
unbounded memory growth — the safety property that lets a component stream an
HTTP/SSE/WS body without falling over.

## From guests

A Wasm guest drives streams through the actor ABI: `stream_open(to)` hands the read
end to another process and keeps the write end; `stream_write`/`stream_close` and
`stream_accept`/`stream_read` move chunks. The two byte copies — *out of* the
producer's sandboxed memory and *into* the consumer's — are the price of true
isolation; everything between is a zero-copy channel hand-off. The **stream-pipe**
benchmark sustains multiple GB/s across producer→consumer pairs.

> Shipped in Phase 7 (core `StreamHandle` in Phase 2; the guest ABI in Phase 7).
