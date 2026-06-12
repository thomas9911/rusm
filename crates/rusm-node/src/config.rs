use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::profile::ResourceProfile;

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
    /// Servers to host, declared as `[[serve]]` tables. Each loads a component
    /// from `./wasm/<name>.{wasm,js}` and serves it on a real TCP port over HTTP
    /// (also SSE) or WebSocket — what `rusm serve` runs and a load driver hits.
    #[serde(default)]
    pub serve: Vec<ServeSpec>,
    /// Custom capability profiles, declared as `[capabilities.<name>]` tables. A
    /// component's `capability = "<name>"` resolves to one of these first, then to
    /// the built-in profiles (`sandboxed` / `network-client` / `trusted`).
    pub capabilities: HashMap<String, CapabilitySpec>,
    /// Path to the node's durable key-value store (one embedded file the node owns,
    /// resolved relative to the app directory). Omitted → no store: a component
    /// granted `storage` then gets an error if it uses `kv`. Set it to give resident
    /// state somewhere to survive a restart.
    #[serde(default)]
    pub store: Option<String>,
    /// Declarative HTTP routing, the `[routes]` table: `"METHOD /path/:param" =
    /// "component#action"`. The serving gateway resolves each request to a component +
    /// action (with path params) and dispatches it per-request. Empty → no HTTP routing.
    #[serde(default)]
    pub routes: HashMap<String, String>,
}

impl NodeConfig {
    /// The compiled [`RouteTable`] for the `[routes]` map (errors on a malformed entry).
    pub fn route_table(&self) -> Result<crate::routes::RouteTable, String> {
        crate::routes::RouteTable::from_map(&self.routes)
    }
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
    /// Allow durable key-value storage (the `kv-*` actor ABI), if the node has a
    /// `store` configured. Default-deny.
    pub storage: Option<bool>,
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

// `CapabilitySpec::to_capabilities()` (the conversion to a concrete
// `rusm_wasm::Capabilities`) lives in the CLI, the only consumer that links
// `rusm-wasm` — keeping this manifest crate free of the Wasm backend.

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
    /// Where to load the (JS) bundle from instead of the local `./wasm/<name>`
    /// artifact: a `url:`/`http(s)://` URL or a `kv:<bucket>/<key>` store entry
    /// (see [`BundleSource`]). Omitted → the local artifact. Lets JS deploy live —
    /// change the bundle at the source and re-`spawn`/reload, no node rebuild.
    #[serde(default)]
    pub source: Option<String>,
}

/// Where a component's JS bundle is fetched from when a [`ComponentSpec`]/[`ServeSpec`]
/// sets `source`, beyond the default local `./wasm/<name>` artifact:
/// - `http(s)://…` (or `url:<u>`) — an HTTP(S) URL (e.g. a presigned blob / artifact API),
/// - `kv:<bucket>/<key>` — an entry in the node's durable store ([`crate`]'s `store`).
///
/// Parsing is pure (this type carries no I/O); the app loader resolves it to bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleSource {
    /// Fetch over HTTP(S).
    Url(String),
    /// Read `key` from `bucket` in the node's durable key-value store.
    Kv { bucket: String, key: String },
}

impl BundleSource {
    /// Parse a manifest `source` string. `kv:<bucket>/<key>` → [`Kv`](Self::Kv);
    /// `url:<u>` or a bare `http(s)://…` → [`Url`](Self::Url). Any other shape is a
    /// (human-readable) error, so a typo is caught at load rather than silently
    /// falling back to a local file.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let spec = spec.trim();
        if let Some(rest) = spec.strip_prefix("kv:") {
            let (bucket, key) = rest
                .split_once('/')
                .ok_or_else(|| format!("kv source must be `kv:<bucket>/<key>`, got {spec:?}"))?;
            if bucket.is_empty() || key.is_empty() {
                return Err(format!(
                    "kv source needs a non-empty bucket and key: {spec:?}"
                ));
            }
            return Ok(BundleSource::Kv {
                bucket: bucket.to_string(),
                key: key.to_string(),
            });
        }
        let url = spec.strip_prefix("url:").unwrap_or(spec);
        if url.starts_with("http://") || url.starts_with("https://") {
            return Ok(BundleSource::Url(url.to_string()));
        }
        Err(format!(
            "unrecognised bundle source {spec:?} (expected `http(s)://…` or `kv:<bucket>/<key>`)"
        ))
    }
}

fn default_capability() -> String {
    "sandboxed".to_string()
}

/// The wire protocol a `[[serve]]` entry is hosted over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServeProtocol {
    /// Request/response HTTP/1.1 — a `wasi:http` component, one instance per request.
    Http,
    /// Server-Sent Events: an HTTP component that streams a `text/event-stream`
    /// body. Served identically to [`Http`](Self::Http); the tag documents intent
    /// and lets a load driver pick the streaming scenario.
    Sse,
    /// WebSocket — one sandboxed component process per connection.
    Ws,
}

impl ServeProtocol {
    /// Whether this protocol is hosted by the HTTP server (`http_server`). Both
    /// plain HTTP and SSE are; only WebSocket uses a different server.
    pub fn is_http(self) -> bool {
        matches!(self, Self::Http | Self::Sse)
    }
}

/// How the handler for a `[[serve]]` entry is instanced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServeMode {
    /// A fresh sandboxed instance per request (HTTP/SSE) or per connection (WS):
    /// stateless and isolated — the default, and the throughput-optimal shape.
    #[default]
    PerRequest,
    /// One (or a pooled set of) long-lived instance(s) serve everything and **hold
    /// state**, supervised for restart. Set `instances` to shard across a pool.
    Resident,
}

fn one() -> usize {
    1
}

/// One `[[serve]]` entry: a component to host as a network server on its own port.
/// Loaded from `./wasm/<name>.{wasm,js}` and run under a capability profile —
/// HTTP/SSE via `http_server`, WebSocket via `ws_server`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServeSpec {
    /// Component name; resolves to `./wasm/<name>.wasm` (or `<name>.js`).
    pub name: String,
    /// Wire protocol to host over (`http` / `sse` / `ws`).
    pub protocol: ServeProtocol,
    /// TCP address to bind, e.g. `"127.0.0.1:8080"` (the real serving port).
    pub listen: String,
    /// Capability profile id (`sandboxed` / `network-client` / `trusted` / custom).
    #[serde(default = "default_capability")]
    pub capability: String,
    /// `per-request` (default) or `resident` (stateful, supervised).
    #[serde(default)]
    pub mode: ServeMode,
    /// Resident pool size (≥1). Only meaningful for `mode = "resident"`.
    #[serde(default = "one")]
    pub instances: usize,
    /// Resident routing affinity: `"header:<name>"` pins requests/connections with
    /// the same header value to one instance; omitted → round-robin.
    #[serde(default)]
    pub shard_by: Option<String>,
    /// Resident overload back-pressure: max concurrent in-flight requests (HTTP/SSE)
    /// or connections (WS) **per instance**; excess sheds to 503. Omitted → unbounded.
    #[serde(default)]
    pub max_inflight: Option<usize>,
    /// Load the (JS) bundle from a `url:`/`http(s)://` URL or `kv:<bucket>/<key>`
    /// instead of `./wasm/<name>` (see [`BundleSource`]). Omitted → the local artifact.
    #[serde(default)]
    pub source: Option<String>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:4000".to_string(),
            profile: ResourceProfile::default(),
            ticks_per_second: 20,
            components: Vec::new(),
            serve: Vec::new(),
            capabilities: HashMap::new(),
            store: None,
            routes: HashMap::new(),
        }
    }
}

impl NodeConfig {
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Loads a manifest from `path`. A `required` (explicitly requested) file that
    /// is missing is an error; a missing optional file (the default `rusm.toml`)
    /// yields [`NodeConfig::default`]. Invalid TOML is always an error. The
    /// returned message is human-readable, ready to print.
    pub fn load(path: &Path, required: bool) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                Self::from_toml(&text).map_err(|e| format!("invalid {}: {e}", path.display()))
            }
            Err(_) if !required => Ok(Self::default()),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
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
    fn load_reads_file_defaults_when_optional_and_errors_when_required() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rusm.toml");
        std::fs::write(&path, "listen = \"0.0.0.0:9000\"\n").unwrap();
        // An explicit, valid file is parsed.
        assert_eq!(
            NodeConfig::load(&path, true).unwrap().listen,
            "0.0.0.0:9000"
        );
        // A missing optional file → defaults; a missing required file → error.
        let missing = dir.path().join("absent.toml");
        assert_eq!(
            NodeConfig::load(&missing, false).unwrap(),
            NodeConfig::default()
        );
        assert!(NodeConfig::load(&missing, true).is_err());
        // Invalid TOML always errors.
        std::fs::write(&path, "nope = 1\n").unwrap();
        assert!(NodeConfig::load(&path, true).is_err());
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
        assert_eq!(spec.storage, None, "storage defaults to unset (deny)");
    }

    #[test]
    fn parses_bundle_source_field_on_components_and_serve() {
        let cfg = NodeConfig::from_toml(
            r#"
            [[components]]
            name = "api"
            source = "kv:bundles/api"

            [[serve]]
            name = "web"
            protocol = "http"
            listen = "127.0.0.1:8080"
            source = "https://cdn.example/web.js"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.components[0].source.as_deref(), Some("kv:bundles/api"));
        assert_eq!(
            cfg.serve[0].source.as_deref(),
            Some("https://cdn.example/web.js")
        );
        // Default: no source (use the local ./wasm artifact).
        assert!(NodeConfig::from_toml("[[components]]\nname = \"x\"\n")
            .unwrap()
            .components[0]
            .source
            .is_none());
    }

    #[test]
    fn bundle_source_parses_url_and_kv_and_rejects_garbage() {
        use BundleSource::*;
        assert_eq!(
            BundleSource::parse("https://cdn/x.js"),
            Ok(Url("https://cdn/x.js".into()))
        );
        assert_eq!(
            BundleSource::parse("url:http://h/x.js"),
            Ok(Url("http://h/x.js".into())) // the `url:` prefix is stripped
        );
        assert_eq!(
            BundleSource::parse("kv:bundles/api"),
            Ok(Kv {
                bucket: "bundles".into(),
                key: "api".into()
            })
        );
        // A key may itself contain slashes (split on the first only).
        assert_eq!(
            BundleSource::parse("kv:b/a/b/c"),
            Ok(Kv {
                bucket: "b".into(),
                key: "a/b/c".into()
            })
        );
        for bad in [
            "kv:nokey",
            "kv:/key",
            "kv:bucket/",
            "ftp://x",
            "./wasm/x.js",
            "",
        ] {
            assert!(
                BundleSource::parse(bad).is_err(),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn parses_store_and_storage_capability() {
        let cfg = NodeConfig::from_toml(
            r#"
            store = "data/app.redb"

            [capabilities.stateful]
            inherits = "trusted"
            storage = true
            "#,
        )
        .unwrap();
        assert_eq!(cfg.store.as_deref(), Some("data/app.redb"));
        assert_eq!(cfg.capabilities["stateful"].storage, Some(true));
        // Default: no store configured.
        assert!(NodeConfig::from_toml("").unwrap().store.is_none());
    }

    #[test]
    fn unknown_capability_field_is_an_error() {
        assert!(NodeConfig::from_toml("[capabilities.x]\nnope = true\n").is_err());
    }

    #[test]
    fn parses_serve_manifest_with_defaults() {
        let cfg = NodeConfig::from_toml(
            r#"
            [[serve]]
            name = "api"
            protocol = "http"
            listen = "127.0.0.1:8080"
            capability = "trusted"

            [[serve]]
            name = "echo"
            protocol = "ws"
            listen = "0.0.0.0:8081"

            [[serve]]
            name = "ticker"
            protocol = "sse"
            listen = "127.0.0.1:8082"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.serve.len(), 3);
        assert_eq!(cfg.serve[0].name, "api");
        assert_eq!(cfg.serve[0].protocol, ServeProtocol::Http);
        assert_eq!(cfg.serve[0].listen, "127.0.0.1:8080");
        assert_eq!(cfg.serve[0].capability, "trusted");
        // protocol parsed; capability defaults to sandboxed.
        assert_eq!(cfg.serve[1].protocol, ServeProtocol::Ws);
        assert_eq!(cfg.serve[1].capability, "sandboxed");
        // SSE is an HTTP-hosted server; WS is not.
        assert_eq!(cfg.serve[2].protocol, ServeProtocol::Sse);
        assert!(cfg.serve[0].protocol.is_http() && cfg.serve[2].protocol.is_http());
        assert!(!cfg.serve[1].protocol.is_http());
        // Mode defaults to per-request, instances to 1, no shard affinity.
        assert_eq!(cfg.serve[0].mode, ServeMode::PerRequest);
        assert_eq!(cfg.serve[0].instances, 1);
        assert!(cfg.serve[0].shard_by.is_none());
    }

    #[test]
    fn parses_resident_serve_fields() {
        let cfg = NodeConfig::from_toml(
            r#"
            [[serve]]
            name = "rooms"
            protocol = "ws"
            listen = "127.0.0.1:9000"
            mode = "resident"
            instances = 4
            shard_by = "header:x-session"
            max_inflight = 256
            "#,
        )
        .unwrap();
        assert_eq!(cfg.serve[0].mode, ServeMode::Resident);
        assert_eq!(cfg.serve[0].instances, 4);
        assert_eq!(cfg.serve[0].shard_by.as_deref(), Some("header:x-session"));
        assert_eq!(cfg.serve[0].max_inflight, Some(256));
    }

    #[test]
    fn no_servers_by_default() {
        assert!(NodeConfig::from_toml("").unwrap().serve.is_empty());
    }

    #[test]
    fn unknown_serve_protocol_is_an_error() {
        let toml = "[[serve]]\nname = \"x\"\nprotocol = \"grpc\"\nlisten = \"127.0.0.1:1\"\n";
        assert!(NodeConfig::from_toml(toml).is_err());
    }

    #[test]
    fn serve_requires_a_listen_address() {
        let toml = "[[serve]]\nname = \"x\"\nprotocol = \"http\"\n";
        assert!(NodeConfig::from_toml(toml).is_err());
    }

    #[test]
    fn unknown_serve_field_is_an_error() {
        let toml = "[[serve]]\nname = \"x\"\nprotocol = \"http\"\nlisten = \"a:1\"\nnope = 1\n";
        assert!(NodeConfig::from_toml(toml).is_err());
    }
}
