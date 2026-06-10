//! `rusm-node` — the benchmark-free node layer shared by the `rusm` CLI and any
//! tooling that hosts or observes a running node.
//!
//! It owns three things, none of them benchmark-aware:
//! - [`config`] — the `rusm.toml` app manifest (components, servers, capabilities).
//! - [`profile`] — [`ResourceProfile`], the machine-usage tier.
//! - the live **attach** layer (`protocol` + `node`) — a node streams plain
//!   [`rusm_otp`] process introspection to attached clients over WebSocket, and
//!   `rusm attach` renders it.

pub mod config;
pub mod profile;

pub use config::{
    CapabilitySpec, ComponentSpec, NodeConfig, PreopenSpec, ServeMode, ServeProtocol, ServeSpec,
};
pub use profile::{ResourceProfile, ResourceProfileMeta};
