# rusm-kv

> Durable, embedded key-value storage for RUSM — a Wasm-free [redb](https://crates.io/crates/redb)-backed store the node owns, surfaced to guests behind the `storage` capability.

`rusm-kv` is the durable storage primitive of [RUSM](https://github.com/archan937/rusm):
one embedded redb file, namespaced into **buckets**, with no external daemon. The node owns
the file; a guest just names buckets and keys, reached through the actor ABI's `kv-*`
operations (gated by the default-deny `storage` capability).

```rust
use rusm_kv::Store;

let store = Store::open("data/app.redb")?;
let bucket = store.bucket("specs");
bucket.set("page:1", b"...")?;
let value = bucket.get("page:1")?;
```

Bytes in, bytes out — the application chooses the serialization. Each operation is an ACID
redb commit, so it is the durable source of truth across restarts.

Part of [RUSM](https://github.com/archan937/rusm). See the
[repo README](https://github.com/archan937/rusm#readme).
