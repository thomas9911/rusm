//! Per-process **capabilities** (default-deny) and named **profiles** — RUSM's
//! answer to "what may this component touch?".
//!
//! A process gets nothing unless granted. A [`CapabilityProfile`] bundles sensible
//! defaults; a [`Capabilities`] builder overrides them per spawn — mirroring the
//! resource-profile pattern in the bench harness. Grants map onto **standard
//! WASI** (`wasi:cli/environment`, `wasi:filesystem`, `wasi:sockets`) plus a
//! `StoreLimiter` memory cap — no wasmCloud-style config-store. Env *values* are
//! resolved at the app layer (process env → `.env`); this only carries the grants.

use std::path::PathBuf;

use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtx, WasiCtxBuilder};

/// Intended per-profile memory ceilings (logical caps enforced by a
/// `StoreLimiter`). The pooling allocator's per-instance reservation is the hard
/// upper bound; these take full effect once it is raised for real components.
const SANDBOX_MAX_MEMORY: usize = 64 << 20; // 64 MiB
const TRUSTED_MAX_MEMORY: usize = 1 << 30; // 1 GiB

/// A named bundle of default grants — the starting point for [`Capabilities`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityProfile {
    /// CPU + a bounded heap only: no fs, no network, no env, no stdio.
    Sandboxed,
    /// Sandboxed plus outbound network — for components that call out (HTTP, …).
    NetworkClient,
    /// Generous: inherits stdio, allows network, a large heap. For trusted code.
    Trusted,
}

impl CapabilityProfile {
    pub fn id(self) -> &'static str {
        match self {
            CapabilityProfile::Sandboxed => "sandboxed",
            CapabilityProfile::NetworkClient => "network-client",
            CapabilityProfile::Trusted => "trusted",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "sandboxed" => Some(Self::Sandboxed),
            "network-client" => Some(Self::NetworkClient),
            "trusted" => Some(Self::Trusted),
            _ => None,
        }
    }

    /// The grants this profile starts from.
    pub fn capabilities(self) -> Capabilities {
        match self {
            CapabilityProfile::Sandboxed => Capabilities::nothing(),
            CapabilityProfile::NetworkClient => Capabilities {
                allow_network: true,
                ..Capabilities::nothing()
            },
            CapabilityProfile::Trusted => Capabilities {
                max_memory: TRUSTED_MAX_MEMORY,
                allow_network: true,
                inherit_stdio: true,
                allow_process_control: true,
                ..Capabilities::nothing()
            },
        }
    }
}

/// A host directory granted to a process, mounted at `guest` inside the sandbox.
#[derive(Debug, Clone)]
struct Preopen {
    host: PathBuf,
    guest: String,
    dir: DirPerms,
    file: FilePerms,
}

/// The exact set of things a process may do. Default-deny: [`nothing`](Capabilities::nothing)
/// grants only a bounded heap; builder methods add capabilities explicitly.
#[derive(Debug, Clone)]
pub struct Capabilities {
    max_memory: usize,
    env: Vec<(String, String)>,
    preopens: Vec<Preopen>,
    allow_network: bool,
    inherit_stdio: bool,
    allow_process_control: bool,
}

impl Capabilities {
    /// The default-deny base: a bounded heap, nothing else.
    pub fn nothing() -> Self {
        Self {
            max_memory: SANDBOX_MAX_MEMORY,
            env: Vec::new(),
            preopens: Vec::new(),
            allow_network: false,
            inherit_stdio: false,
            allow_process_control: false,
        }
    }

    /// Grants one environment variable (seen by the guest via `wasi:cli/environment`).
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Grants a host directory, mounted at `guest_path`. Read-only unless `read_only`
    /// is false (then read+write).
    pub fn preopen(
        mut self,
        host_path: impl Into<PathBuf>,
        guest_path: impl Into<String>,
        read_only: bool,
    ) -> Self {
        let (dir, file) = if read_only {
            (DirPerms::READ, FilePerms::READ)
        } else {
            (DirPerms::all(), FilePerms::all())
        };
        self.preopens.push(Preopen {
            host: host_path.into(),
            guest: guest_path.into(),
            dir,
            file,
        });
        self
    }

    /// Allows outbound network access.
    pub fn allow_network(mut self, allow: bool) -> Self {
        self.allow_network = allow;
        self
    }

    /// Inherits the host's stdio.
    pub fn inherit_stdio(mut self, inherit: bool) -> Self {
        self.inherit_stdio = inherit;
        self
    }

    /// Allows this process to **control other processes** via the actor ABI —
    /// `kill`/`list-processes`/`info`/`is-alive` over pids other than its own.
    /// Default-deny: a sandboxed process can manage *itself* and message/name-
    /// coordinate, but can't enumerate, inspect, or kill its neighbours.
    pub fn allow_process_control(mut self, allow: bool) -> Self {
        self.allow_process_control = allow;
        self
    }

    /// Sets the per-process memory ceiling in bytes (enforced by a `StoreLimiter`).
    pub fn max_memory(mut self, bytes: usize) -> Self {
        self.max_memory = bytes;
        self
    }

    /// Whether this process may control others via the actor ABI.
    pub(crate) fn process_control(&self) -> bool {
        self.allow_process_control
    }

    /// The memory ceiling, for the runtime's `StoreLimiter`.
    pub(crate) fn memory_limit(&self) -> usize {
        self.max_memory
    }

    /// Builds the WASI builder these grants describe — the single source of truth
    /// shared by the component (`build_wasi`) and core-module (`build_wasi_p1`) paths.
    fn configure(&self) -> anyhow::Result<WasiCtxBuilder> {
        let mut builder = WasiCtxBuilder::new();
        if self.inherit_stdio {
            builder.inherit_stdio();
        }
        for (key, value) in &self.env {
            builder.env(key, value);
        }
        for p in &self.preopens {
            builder.preopened_dir(&p.host, &p.guest, p.dir, p.file)?;
        }
        if self.allow_network {
            builder.inherit_network();
            builder.allow_tcp(true);
        }
        Ok(builder)
    }

    /// Builds the WASI **component** (p2/p3) context these capabilities describe.
    pub(crate) fn build_wasi(&self) -> anyhow::Result<WasiCtx> {
        Ok(self.configure()?.build())
    }

    /// Builds the WASI **preview1** (core-module) context — the same grants, wired
    /// for the `wasi_snapshot_preview1` import surface a core module links against.
    pub(crate) fn build_wasi_p1(&self) -> anyhow::Result<WasiP1Ctx> {
        Ok(self.configure()?.build_p1())
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::nothing()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_round_trip_and_grant_differently() {
        for p in [
            CapabilityProfile::Sandboxed,
            CapabilityProfile::NetworkClient,
            CapabilityProfile::Trusted,
        ] {
            assert_eq!(CapabilityProfile::from_id(p.id()), Some(p));
        }
        assert_eq!(CapabilityProfile::from_id("nope"), None);

        let sandbox = CapabilityProfile::Sandboxed.capabilities();
        assert!(!sandbox.allow_network && !sandbox.inherit_stdio && sandbox.env.is_empty());
        assert!(
            !sandbox.process_control(),
            "sandboxed: no control of others"
        );
        let client = CapabilityProfile::NetworkClient.capabilities();
        assert!(client.allow_network && !client.inherit_stdio && !client.process_control());
        let trusted = CapabilityProfile::Trusted.capabilities();
        assert!(trusted.allow_network && trusted.inherit_stdio);
        assert!(trusted.process_control(), "trusted: may control others");
        assert!(trusted.memory_limit() > sandbox.memory_limit());
        // The builder grants it explicitly too.
        assert!(Capabilities::nothing()
            .allow_process_control(true)
            .process_control());
    }

    #[test]
    fn builder_adds_grants() {
        let caps = Capabilities::nothing()
            .env("K", "V")
            .allow_network(true)
            .inherit_stdio(true)
            .max_memory(123);
        assert_eq!(caps.memory_limit(), 123);
        assert!(caps.allow_network && caps.inherit_stdio);
        assert_eq!(caps.env, vec![("K".to_string(), "V".to_string())]);
    }

    #[test]
    fn build_wasi_handles_defaults_and_preopens() {
        assert!(Capabilities::nothing().build_wasi().is_ok());
        // Both read-only and read-write preopens build (covers both perm branches).
        let tmp = std::env::temp_dir();
        assert!(Capabilities::nothing()
            .preopen(&tmp, "/ro", true)
            .build_wasi()
            .is_ok());
        assert!(Capabilities::nothing()
            .preopen(&tmp, "/rw", false)
            .env("A", "B")
            .allow_network(true)
            .inherit_stdio(true)
            .build_wasi()
            .is_ok());
    }

    #[test]
    fn build_wasi_p1_shares_the_same_grants() {
        // The preview1 context is built from the same configuration as the
        // component one — both paths must accept the full grant set.
        assert!(Capabilities::nothing().build_wasi_p1().is_ok());
        let tmp = std::env::temp_dir();
        assert!(Capabilities::nothing()
            .preopen(&tmp, "/ro", true)
            .env("A", "B")
            .allow_network(true)
            .inherit_stdio(true)
            .build_wasi_p1()
            .is_ok());
    }
}
