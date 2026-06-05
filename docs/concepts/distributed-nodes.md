# Concept — distributed nodes

A single RUSM node is a host process running many Wasm processes. A **cluster** is
several nodes — on different machines — connected so processes can spawn and
message across them, exactly like distributed Erlang.

## Connecting

Nodes connect over **QUIC** with **TLS** (think `Node.connect/1` + epmd, but
secure by default). Each node has a name; the cluster tracks which names are
reachable. Untrusted certificates are rejected.

## Location transparency

Once connected:

- **Remote spawn** — start a process on another node and get back a pid that
  works like any local pid.
- **Cross-node `send`** — the host routes a message to the right node's mailbox;
  the sender doesn't care where the target lives.
- **Distributed registry** — a `:global`-style name → pid map spanning the
  cluster, so services can be found by name from any node.

## Testing it

An `ex_united`-style harness boots several nodes in one test and connects them,
so cross-node spawn/send and registry lookup are all TDD-able. (`ex_united` is the
hex package the author wrote to do this for Elixir.)

## Hooking in

You can also attach to any running node to inspect it live — see
[live attach](./live-attach.md).

> Implemented in Phase 10.
