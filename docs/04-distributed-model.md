# Distributed model & live attach (Phase 9)

RUSM chases the two BEAM superpowers that matter most: nodes that connect into a
cluster, and the ability to hook into a running node live. Both ship in the
Wasm-free [`rusm-cluster`](https://github.com/archan937/rusm) crate, layered over
`rusm-otp` — the actor model, with the wire in between.

## Nodes connecting to nodes

A `ClusterNode` wraps a normal `rusm_otp::Runtime` with a **QUIC** endpoint
(**TLS 1.3**, rustls + ring). Independent nodes — separate OS processes, typically
on separate machines — connect by address:

```rust
let id = Identity::generate()?;                                  // shared cluster cert
let london = ClusterNode::bind("london", Runtime::new(), addr, &id)?;
let tokyo  = ClusterNode::bind("tokyo",  Runtime::new(), addr2, &id)?;
tokyo.connect(london.local_addr()?).await?;                      // like Node.connect/1
```

Each link carries two kinds of stream:

- a single long-lived **control stream** (the bidirectional stream opened during
  the handshake) — node-name exchange, then global-registry gossip and a
  request/reply control-plane RPC;
- one **uni-stream per message**, so cross-node messages never head-of-line-block
  each other (the reason to reach for QUIC over TCP).

### Addressing & location transparency

- **Cross-node `send`** — `node.send("london", "greeter", bytes)` routes a message
  to the process registered as `greeter` on `london`. A `RemoteNode` handle from
  `connect` does the same without naming the node.
- **Global registry** — `register_global(name, pid)` registers a name locally and
  **gossips** it to every peer; `send_global(name, bytes)` resolves the owning node
  and routes there, so the sender never needs to know where a service lives.
  `whereis_global(name)` returns the owning node.
- **Remote spawn** — a node `register_spawnable(name, factory)`s work it knows how
  to build; a peer calls `spawn_remote(node, factory, args)` and gets back the pid
  spawned *there*. (The cluster can't ship a closure across the wire, so a node only
  spawns work it has been taught — explicit, and friendly to capability control.)

## Hooking into a running node (live attach)

The same control-plane RPC backs **live attach**: `node.remote_pids("london")`
lists the processes alive on a peer — point at a running node and see what it's
doing. This is the cluster-level primitive; the dashboard's remote observer and the
`rusm attach <node>` REPL build on it.

> This is a **new RUSM capability**, not something Rust provides. The BEAM bakes
> remote shell + distribution + observer into the VM; Rust has no runtime VM, no
> built-in process/node model, and no live introspection. Closest prior art:
> [`tokio-console`](https://github.com/tokio-rs/console) (live task view) — but
> that's read-mostly diagnostics, not a process-model-aware control plane.

## Security

Every link is QUIC, i.e. TLS 1.3 — encrypted and authenticated. Today a cluster
shares one self-signed `Identity` (a **pre-shared cluster certificate**): a node
only completes a handshake with a peer presenting the same cert, and the client
pins that cert as its sole trust root — a wrong certificate is rejected at the
handshake. Per-node certificates signed by a cluster CA are a later refinement; the
transport seam does not change.

## Performance

Measured on loopback (the [`cluster_fanout`](https://github.com/archan937/rusm)
benchmark), everyday machine load:

- **unloaded cross-node round-trip latency**: ~39µs p50 / ~112µs p99 (two
  cross-node hops + two process hops);
- **saturation throughput**: ~280k round-trips/sec ≈ **~550k cross-node
  messages/sec**.

Latency and throughput are measured separately — under saturation, latency is
dominated by queue depth, not the wire, so one number for both would mislead.

## Testing it

The transport is fully TDD-able in-process: tests boot several `ClusterNode`s on
loopback and exercise cross-node send, the global registry (gossip on connect and
on late registration), remote spawn, live-attach listing, and wrong-certificate
rejection — no external network. See the `rusm-cluster` test module.
