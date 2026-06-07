use std::collections::HashMap;

use serde::Deserialize;

use crate::profile::ResourceProfile;
use crate::runner::RunnerConfig;

/// Node startup configuration, loaded from `rusm.toml`.
///
/// Layering: these are *defaults* — the CLI applies any flags on top. Missing
/// fields fall back to the values below; unknown fields are an error (catch typos
/// early rather than silently ignore them).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NodeConfig {
    /// WebSocket listen address.
    pub listen: String,
    /// Starting resource profile (`light` / `balanced` / `max`).
    pub profile: ResourceProfile,
    /// Snapshot/sampling rate in Hz.
    pub ticks_per_second: u32,
    /// Components to run as an app, declared as `[[components]]` tables. Each is
    /// loaded from `./wasm/<name>.wasm` and spawned under its capability profile.
    pub components: Vec<ComponentSpec>,
    /// Custom capability profiles, declared as `[capabilities.<name>]` tables. A
    /// component's `capability = "<name>"` resolves to one of these first, then to
    /// the built-in profiles (`sandboxed` / `network-client` / `trusted`).
    pub capabilities: HashMap<String, CapabilitySpec>,
}

/// A custom capability profile (`[capabilities.<name>]`) — mirrors Cargo's
/// `[profile.<name>]`: it `inherits` a built-in base (default `sandboxed`,
/// default-deny) and overrides specific grants. Only set fields override the base,
/// so a profile close to a preset stays terse. Referenced by a component's
/// `capability = "<name>"`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct CapabilitySpec {
    /// Built-in profile to start from (`sandboxed` / `network-client` / `trusted`);
    /// omitted (or unrecognised) → `sandboxed`, the most restrictive base.
    pub inherits: Option<String>,
    /// Allow outbound network access.
    pub network: Option<bool>,
    /// Allow spawning other components by name (capability-gated `spawn`).
    pub spawn: Option<bool>,
    /// Allow controlling other processes (kill/list/info over foreign pids).
    pub process_control: Option<bool>,
    /// Inherit the host's stdio.
    pub stdio: Option<bool>,
    /// Per-process memory ceiling in MiB.
    pub max_memory_mb: Option<usize>,
    /// Environment-variable keys to grant; values are resolved from the process
    /// environment (process env, then `.env`) at load — keys with no value are skipped.
    #[serde(default)]
    pub env: Vec<String>,
    /// Host directories to preopen inside the sandbox.
    #[serde(default)]
    pub preopen: Vec<PreopenSpec>,
}

/// One `preopen` entry of a [`CapabilitySpec`]: a host directory mounted at `guest`
/// inside the sandbox, read-only unless `read-only = false`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct PreopenSpec {
    pub host: String,
    pub guest: String,
    #[serde(default)]
    pub read_only: bool,
}

impl CapabilitySpec {
    /// Resolves this spec to concrete [`Capabilities`]: start from the inherited
    /// built-in base (default `sandboxed`), then apply each set override. Env keys
    /// are resolved from the process environment (process env, then `.env`).
    pub fn to_capabilities(&self) -> rusm_wasm::Capabilities {
        let mut caps = self
            .inherits
            .as_deref()
            .and_then(rusm_wasm::CapabilityProfile::from_id)
            .unwrap_or(rusm_wasm::CapabilityProfile::Sandboxed)
            .capabilities();
        if let Some(v) = self.network {
            caps = caps.allow_network(v);
        }
        if let Some(v) = self.spawn {
            caps = caps.allow_spawn(v);
        }
        if let Some(v) = self.process_control {
            caps = caps.allow_process_control(v);
        }
        if let Some(v) = self.stdio {
            caps = caps.inherit_stdio(v);
        }
        if let Some(mb) = self.max_memory_mb {
            caps = caps.max_memory(mb << 20);
        }
        for key in &self.env {
            if let Ok(value) = std::env::var(key) {
                caps = caps.env(key, value);
            }
        }
        for p in &self.preopen {
            caps = caps.preopen(&p.host, &p.guest, p.read_only);
        }
        caps
    }
}

/// One `[[components]]` entry: a component to load from `./wasm/<name>.wasm` and
/// run as a supervised process under a capability profile.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentSpec {
    /// Component name; resolves to the artifact `./wasm/<name>.wasm`.
    pub name: String,
    /// Capability profile id (`sandboxed` / `network-client` / `trusted`).
    #[serde(default = "default_capability")]
    pub capability: String,
    /// Restart the component if it exits (supervision). Off by default.
    #[serde(default)]
    pub restart: bool,
}

fn default_capability() -> String {
    "sandboxed".to_string()
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:4000".to_string(),
            profile: ResourceProfile::default(),
            ticks_per_second: 20,
            components: Vec::new(),
            capabilities: HashMap::new(),
        }
    }
}

impl NodeConfig {
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// The runner config implied by this file (just the sampling rate; the
    /// `profile` is applied to the running node separately, so it shows up in
    /// frames and can be changed live).
    pub fn runner_config(&self) -> RunnerConfig {
        RunnerConfig {
            ticks_per_second: self.ticks_per_second.max(1),
            ..RunnerConfig::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_all_defaults() {
        assert_eq!(NodeConfig::from_toml("").unwrap(), NodeConfig::default());
    }

    #[test]
    fn parses_a_full_file() {
        let cfg = NodeConfig::from_toml(
            r#"
            listen = "0.0.0.0:9000"
            profile = "max"
            ticks_per_second = 60
            "#,
        )
        .unwrap();
        assert_eq!(cfg.listen, "0.0.0.0:9000");
        assert_eq!(cfg.profile, ResourceProfile::Max);
        assert_eq!(cfg.ticks_per_second, 60);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let cfg = NodeConfig::from_toml("profile = \"light\"").unwrap();
        assert_eq!(cfg.profile, ResourceProfile::Light);
        assert_eq!(cfg.listen, NodeConfig::default().listen); // default kept
    }

    #[test]
    fn unknown_field_is_an_error() {
        assert!(NodeConfig::from_toml("nope = 1").is_err());
    }

    #[test]
    fn invalid_profile_is_an_error() {
        assert!(NodeConfig::from_toml("profile = \"turbo\"").is_err());
    }

    #[test]
    fn runner_config_carries_the_tick_rate() {
        let cfg = NodeConfig::from_toml("ticks_per_second = 30").unwrap();
        assert_eq!(cfg.runner_config().ticks_per_second, 30);
    }

    #[test]
    fn parses_component_manifest_with_defaults() {
        let cfg = NodeConfig::from_toml(
            r#"
            [[components]]
            name = "source"
            capability = "network-client"

            [[components]]
            name = "sink"
            restart = true
            "#,
        )
        .unwrap();
        assert_eq!(cfg.components.len(), 2);
        assert_eq!(cfg.components[0].name, "source");
        assert_eq!(cfg.components[0].capability, "network-client");
        assert!(!cfg.components[0].restart);
        // capability defaults to sandboxed; restart parsed.
        assert_eq!(cfg.components[1].capability, "sandboxed");
        assert!(cfg.components[1].restart);
    }

    #[test]
    fn no_components_by_default() {
        assert!(NodeConfig::from_toml("").unwrap().components.is_empty());
    }

    #[test]
    fn unknown_component_field_is_an_error() {
        let toml = "[[components]]\nname = \"x\"\nnope = 1\n";
        assert!(NodeConfig::from_toml(toml).is_err());
    }

    #[test]
    fn parses_custom_capability_profiles() {
        let cfg = NodeConfig::from_toml(
            r#"
            [capabilities.agent]
            inherits = "network-client"
            spawn = true
            max-memory-mb = 256
            preopen = [{ host = "./data", guest = "/data", read-only = false }]

            [[components]]
            name = "pages-agent"
            capability = "agent"
            "#,
        )
        .unwrap();
        let spec = &cfg.capabilities["agent"];
        assert_eq!(spec.inherits.as_deref(), Some("network-client"));
        assert_eq!(spec.spawn, Some(true));
        assert_eq!(spec.max_memory_mb, Some(256));
        assert_eq!(spec.preopen.len(), 1);
        assert!(!spec.preopen[0].read_only);
    }

    #[test]
    fn a_custom_profile_inherits_then_overrides() {
        // Starts from network-client (network on, spawn off), then turns spawn on
        // and tightens memory — only the set fields override the inherited base.
        let cfg = NodeConfig::from_toml(
            "[capabilities.worker]\ninherits = \"network-client\"\nspawn = true\nmax-memory-mb = 32\n",
        )
        .unwrap();
        let caps = cfg.capabilities["worker"].to_capabilities();
        assert!(caps.can_spawn(), "override turned spawn on");
        assert_eq!(caps.memory_limit(), 32 << 20, "override tightened memory");
        // An omitted base → the most restrictive default (sandboxed): no spawn.
        let bare = CapabilitySpec {
            inherits: None,
            network: None,
            spawn: None,
            process_control: None,
            stdio: None,
            max_memory_mb: None,
            env: Vec::new(),
            preopen: Vec::new(),
        };
        assert!(
            !bare.to_capabilities().can_spawn(),
            "default base is sandboxed"
        );
    }

    #[test]
    fn unknown_capability_field_is_an_error() {
        assert!(NodeConfig::from_toml("[capabilities.x]\nnope = true\n").is_err());
    }
}
