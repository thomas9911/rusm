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
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:4000".to_string(),
            profile: ResourceProfile::default(),
            ticks_per_second: 20,
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
}
