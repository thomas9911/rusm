//! Durable key-value storage for a Rust guest — the ergonomic wrapper over the
//! `kv-*` actor ABI (gated by the `storage` capability). Mirrors the host
//! [`rusm_kv`](https://docs.rs/rusm-kv) `Bucket` API: open a bucket by name, then
//! `get`/`set`/`delete`/`exists`/`list`. Every op returns `Result<_, String>` —
//! the host's message on failure (storage denied, no store configured on the node,
//! or a store error).
//!
//! ```ignore
//! let specs = rusm_rs::kv::bucket("specs");
//! specs.set("page:1", b"{...}")?;
//! let spec = specs.get("page:1")?;        // Option<Vec<u8>>
//! let ids = specs.list()?;                // Vec<String>
//! ```

use crate::actor;

/// Open the named bucket. Cheap and infallible — no I/O until an op runs.
pub fn bucket(name: impl Into<String>) -> Bucket {
    Bucket { name: name.into() }
}

/// A handle to one named bucket in the node's durable store.
pub struct Bucket {
    name: String,
}

impl Bucket {
    /// The stored value for `key`, or `None` if absent.
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        actor::kv_get(&self.name, key)
    }

    /// Store `value` under `key`, overwriting any previous value.
    pub fn set(&self, key: &str, value: &[u8]) -> Result<(), String> {
        actor::kv_set(&self.name, key, value)
    }

    /// Remove `key`, returning whether it was present.
    pub fn delete(&self, key: &str) -> Result<bool, String> {
        actor::kv_delete(&self.name, key)
    }

    /// Whether `key` is present.
    pub fn exists(&self, key: &str) -> Result<bool, String> {
        actor::kv_exists(&self.name, key)
    }

    /// Every key in this bucket, in sorted order.
    pub fn list(&self) -> Result<Vec<String>, String> {
        actor::kv_list(&self.name)
    }
}
