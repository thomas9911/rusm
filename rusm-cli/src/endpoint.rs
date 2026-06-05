pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 4000;

/// Turns a user-friendly attach target into a WebSocket URL.
///
/// Accepts a full `ws://`/`wss://` URL (used as-is), a `host:port`, or a bare
/// `host` (the default port is appended). This is why `rusm attach` needs no
/// `ws://…` ceremony for the common local case.
pub fn normalize_target(target: &str) -> String {
    let target = target.trim();
    if target.starts_with("ws://") || target.starts_with("wss://") {
        target.to_string()
    } else if target.contains(':') {
        format!("ws://{target}")
    } else {
        format!("ws://{target}:{DEFAULT_PORT}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_host_gets_scheme_and_default_port() {
        assert_eq!(normalize_target("localhost"), "ws://localhost:4000");
        assert_eq!(normalize_target(DEFAULT_HOST), "ws://127.0.0.1:4000");
    }

    #[test]
    fn host_port_gets_scheme_only() {
        assert_eq!(normalize_target("10.0.0.5:5000"), "ws://10.0.0.5:5000");
    }

    #[test]
    fn full_urls_pass_through() {
        assert_eq!(normalize_target("ws://node:1"), "ws://node:1");
        assert_eq!(normalize_target("wss://node"), "wss://node");
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(normalize_target("  localhost  "), "ws://localhost:4000");
    }
}
