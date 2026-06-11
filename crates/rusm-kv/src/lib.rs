//! **rusm-kv** — the Wasm-free embedded key-value store of RUSM: durable,
//! transactional buckets over [`redb`] (a pure-Rust, ACID, single-file store — no
//! external daemon). It is to durable storage what `rusm-otp` is to processes: a
//! focused primitive with **no Wasmtime dependency**, surfaced to guests by
//! `rusm-wasm` behind a capability and wrapped ergonomically by the guest SDKs.
//!
//! A [`Store`] owns one database file; a [`Bucket`] is a named namespace within it
//! (`get`/`set`/`delete`/`exists`/`list`). Buckets share a single redb table, keyed
//! by a **length-prefixed composite** (`[bucket-len: u32 BE][bucket][key]`) so that
//! a bucket's keys are contiguous (cheap `list`) and two buckets can never collide
//! — `("a","bc")` and `("ab","c")` encode distinctly.
//!
//! `Store` is cheap to [`clone`](Clone) (an `Arc` over the database) and is
//! `Send + Sync`: redb serialises writes and allows concurrent reads, so many
//! processes can share one store, each op in its own transaction.

use std::path::Path;
use std::sync::Arc;

use redb::{Database, ReadableDatabase, TableDefinition};

/// The single backing table; buckets are encoded into the key (see module docs).
const KV: TableDefinition<&[u8], &[u8]> = TableDefinition::new("kv");

/// A fallible KV operation. The error is redb's umbrella type — callers that cross
/// an ABI boundary (e.g. `rusm-wasm`) render it with `to_string`.
pub type Result<T> = std::result::Result<T, redb::Error>;

/// A durable, embedded key-value store backed by one redb database file.
#[derive(Clone)]
pub struct Store {
    db: Arc<Database>,
}

impl Store {
    /// Open (creating if absent) the store at `path`. The backing table is
    /// materialised up front, so reads never race a not-yet-written table.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Database::create(path)?;
        let write = db.begin_write()?;
        write.open_table(KV)?; // create-if-absent
        write.commit()?;
        Ok(Self { db: Arc::new(db) })
    }

    /// A handle to the named bucket. Cheap and infallible — no I/O until an op runs.
    pub fn bucket(&self, name: &str) -> Bucket {
        let mut prefix = (name.len() as u32).to_be_bytes().to_vec();
        prefix.extend_from_slice(name.as_bytes());
        Bucket {
            db: Arc::clone(&self.db),
            prefix,
        }
    }
}

/// One namespace within a [`Store`]. All ops are scoped to this bucket; keys in
/// other buckets are invisible here.
pub struct Bucket {
    db: Arc<Database>,
    /// `[bucket-len: u32 BE][bucket bytes]` — prepended to every key.
    prefix: Vec<u8>,
}

impl Bucket {
    /// The stored value for `key`, or `None` if absent.
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let encoded = self.encode(key);
        let table = self.db.begin_read()?.open_table(KV)?;
        let value = table.get(encoded.as_slice())?;
        Ok(value.map(|v| v.value().to_vec()))
    }

    /// Store `value` under `key`, overwriting any previous value.
    pub fn set(&self, key: &str, value: &[u8]) -> Result<()> {
        let write = self.db.begin_write()?;
        {
            let mut table = write.open_table(KV)?;
            table.insert(self.encode(key).as_slice(), value)?;
        }
        write.commit()?;
        Ok(())
    }

    /// Remove `key`, returning whether it was present.
    pub fn delete(&self, key: &str) -> Result<bool> {
        let write = self.db.begin_write()?;
        let existed = {
            let mut table = write.open_table(KV)?;
            let removed = table.remove(self.encode(key).as_slice())?;
            removed.is_some()
        };
        write.commit()?;
        Ok(existed)
    }

    /// Whether `key` is present (without materialising its value).
    pub fn exists(&self, key: &str) -> Result<bool> {
        let encoded = self.encode(key);
        let table = self.db.begin_read()?.open_table(KV)?;
        Ok(table.get(encoded.as_slice())?.is_some())
    }

    /// Every key in this bucket, in sorted (byte) order. Other buckets' keys are
    /// excluded by the shared prefix; iteration stops as soon as the prefix changes.
    pub fn list(&self) -> Result<Vec<String>> {
        let table = self.db.begin_read()?.open_table(KV)?;
        let mut keys = Vec::new();
        for item in table.range::<&[u8]>(self.prefix.as_slice()..)? {
            let (stored, _) = item?;
            let stored = stored.value();
            if !stored.starts_with(&self.prefix) {
                break; // past this bucket's contiguous range
            }
            keys.push(String::from_utf8_lossy(&stored[self.prefix.len()..]).into_owned());
        }
        Ok(keys)
    }

    /// `[bucket-len][bucket][key]` — the on-disk key for `key` in this bucket.
    fn encode(&self, key: &str) -> Vec<u8> {
        let mut encoded = self.prefix.clone();
        encoded.extend_from_slice(key.as_bytes());
        encoded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store on a fresh temp file, plus its dir guard (kept alive for the test).
    fn temp_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("kv.redb")).unwrap();
        (store, dir)
    }

    #[test]
    fn set_then_get_roundtrips() {
        let (store, _dir) = temp_store();
        let b = store.bucket("specs");
        b.set("k", b"hello").unwrap();
        assert_eq!(b.get("k").unwrap().as_deref(), Some(b"hello".as_slice()));
    }

    #[test]
    fn get_missing_is_none_and_exists_is_false() {
        let (store, _dir) = temp_store();
        let b = store.bucket("specs");
        assert_eq!(b.get("absent").unwrap(), None);
        assert!(!b.exists("absent").unwrap());
        b.set("present", b"1").unwrap();
        assert!(b.exists("present").unwrap());
    }

    #[test]
    fn set_overwrites() {
        let (store, _dir) = temp_store();
        let b = store.bucket("specs");
        b.set("k", b"first").unwrap();
        b.set("k", b"second").unwrap();
        assert_eq!(b.get("k").unwrap().as_deref(), Some(b"second".as_slice()));
    }

    #[test]
    fn delete_removes_and_reports_prior_existence() {
        let (store, _dir) = temp_store();
        let b = store.bucket("specs");
        b.set("k", b"v").unwrap();
        assert!(
            b.delete("k").unwrap(),
            "delete of a present key reports true"
        );
        assert!(!b.exists("k").unwrap());
        assert!(
            !b.delete("k").unwrap(),
            "delete of an absent key reports false"
        );
    }

    #[test]
    fn binary_values_roundtrip() {
        let (store, _dir) = temp_store();
        let b = store.bucket("blobs");
        let value = [0u8, 1, 2, 255, 254, 0, 128];
        b.set("k", &value).unwrap();
        assert_eq!(b.get("k").unwrap().as_deref(), Some(value.as_slice()));
    }

    #[test]
    fn buckets_are_isolated() {
        let (store, _dir) = temp_store();
        store.bucket("a").set("k", b"in-a").unwrap();
        store.bucket("b").set("k", b"in-b").unwrap();
        assert_eq!(
            store.bucket("a").get("k").unwrap().as_deref(),
            Some(b"in-a".as_slice())
        );
        assert_eq!(
            store.bucket("b").get("k").unwrap().as_deref(),
            Some(b"in-b".as_slice())
        );
        assert_eq!(store.bucket("a").list().unwrap(), vec!["k"]);
    }

    #[test]
    fn keys_with_shared_prefixes_do_not_collide() {
        // The length prefix is what prevents ("a","bc") and ("ab","c") — which would
        // both naively concatenate to "abc" — from colliding.
        let (store, _dir) = temp_store();
        store.bucket("a").set("bc", b"A").unwrap();
        store.bucket("ab").set("c", b"B").unwrap();
        assert_eq!(
            store.bucket("a").get("bc").unwrap().as_deref(),
            Some(b"A".as_slice())
        );
        assert_eq!(
            store.bucket("ab").get("c").unwrap().as_deref(),
            Some(b"B".as_slice())
        );
        assert_eq!(store.bucket("a").list().unwrap(), vec!["bc"]);
        assert_eq!(store.bucket("ab").list().unwrap(), vec!["c"]);
    }

    #[test]
    fn list_returns_only_this_buckets_keys_sorted() {
        let (store, _dir) = temp_store();
        let specs = store.bucket("specs");
        specs.set("gamma", b"3").unwrap();
        specs.set("alpha", b"1").unwrap();
        specs.set("beta", b"2").unwrap();
        store.bucket("plans").set("other", b"x").unwrap();
        assert_eq!(specs.list().unwrap(), vec!["alpha", "beta", "gamma"]);
        assert!(specs.delete("beta").unwrap());
        assert_eq!(specs.list().unwrap(), vec!["alpha", "gamma"]);
    }

    #[test]
    fn empty_bucket_lists_nothing() {
        let (store, _dir) = temp_store();
        assert!(store.bucket("untouched").list().unwrap().is_empty());
    }

    #[test]
    fn data_is_durable_across_reopen() {
        // The whole point over in-process state: survive a restart.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kv.redb");
        {
            let store = Store::open(&path).unwrap();
            store.bucket("specs").set("k", b"persisted").unwrap();
        } // store dropped → file closed
        let reopened = Store::open(&path).unwrap();
        assert_eq!(
            reopened.bucket("specs").get("k").unwrap().as_deref(),
            Some(b"persisted".as_slice()),
            "data must survive a close + reopen"
        );
    }

    #[test]
    fn a_shared_store_is_usable_from_clones() {
        // Cloning is an Arc bump; clones see each other's writes (shared db).
        let (store, _dir) = temp_store();
        let clone = store.clone();
        store.bucket("specs").set("k", b"v").unwrap();
        assert_eq!(
            clone.bucket("specs").get("k").unwrap().as_deref(),
            Some(b"v".as_slice())
        );
    }

    #[test]
    fn open_failure_is_reported_not_panicked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kv.redb");
        let _held = Store::open(&path).unwrap();
        // redb holds an exclusive lock; a second in-process open of the same file
        // is refused — `open` must surface that as `Err`, never panic.
        assert!(
            Store::open(&path).is_err(),
            "a second open of a locked store must error"
        );
    }
}
