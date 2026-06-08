//! `rusm new <name>` — scaffold a new RUSM app.
//!
//! Creates a minimal, **immediately buildable** project: one TypeScript HTTP
//! component and a `[[serve]]` manifest, so a fresh user can go from nothing to a
//! live server in three commands:
//!
//! ```text
//! rusm new hello && cd hello
//! rusm build      # Bun bundles components/api/index.ts -> wasm/api.js
//! rusm serve      # hosts it on http://127.0.0.1:8080
//! ```
//!
//! The starter component depends on nothing (a web-standard `Request`/`Response`
//! handler), so there is no package to install — the scaffold builds with just Bun.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// `rusm.toml`: a single HTTP server on a real port, sandboxed by default.
const RUSM_TOML: &str = "\
# RUSM app config. `rusm serve` hosts each [[serve]] entry; `rusm build` compiles
# components/<name>/ into wasm/<name>.{wasm,js} first. See https://github.com/archan937/rusm.

[[serve]]
name = \"api\"                 # loads wasm/api.js (or api.wasm), built from components/api
protocol = \"http\"            # http | sse | ws
listen = \"127.0.0.1:8080\"
capability = \"sandboxed\"      # default-deny; see [capabilities.<name>] for custom profiles
";

/// The starter HTTP component: a web-standard request→response handler, zero deps.
const API_COMPONENT: &str = "\
// A RUSM HTTP component. Export a default handler; `rusm serve` hosts it with one
// sandboxed WASM instance per request — write the handler, RUSM owns the lifecycle.
export default function handle(request: Request): Response {
  const url = new URL(request.url);
  return new Response(`Hello from RUSM 👋  (you asked for ${url.pathname})\\n`, {
    headers: { \"content-type\": \"text/plain\" },
  });
}
";

/// `.gitignore`: build output and JS deps are not source.
const GITIGNORE: &str = "/wasm/\n/node_modules/\n/target/\n";

/// Renders the project README, filled with the app's name.
fn readme(name: &str) -> String {
    format!(
        "# {name}\n\n\
A RUSM app — isolated, supervised WASM components on an Erlang-style actor runtime.\n\n\
## Run it\n\n\
```sh\n\
rusm build      # Bun bundles components/api/index.ts -> wasm/api.js\n\
rusm serve      # serves it on http://127.0.0.1:8080\n\
```\n\n\
Then in another terminal:\n\n\
```sh\n\
curl http://127.0.0.1:8080/\n\
```\n\n\
## Layout\n\n\
- `components/api/index.ts` — the HTTP handler (edit this).\n\
- `rusm.toml` — what to serve, on which port, under which capability profile.\n\
- `wasm/` — built artifacts (git-ignored); produced by `rusm build`.\n\n\
Add more components under `components/<name>/` and reference them from `rusm.toml`.\n"
    )
}

/// Scaffolds a new app at `<root>/<name>`, returning the files created (relative to
/// `root`). Fails if the target directory already exists and is non-empty, so an
/// existing project is never clobbered.
pub fn scaffold(root: &Path, name: &str) -> Result<Vec<PathBuf>> {
    validate_name(name)?;
    let project = root.join(name);
    if project.exists()
        && project
            .read_dir()
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
    {
        bail!("`{name}` already exists and is not empty");
    }

    let files = [
        (PathBuf::from("rusm.toml"), RUSM_TOML.to_string()),
        (
            PathBuf::from("components/api/index.ts"),
            API_COMPONENT.to_string(),
        ),
        (PathBuf::from(".gitignore"), GITIGNORE.to_string()),
        (PathBuf::from("README.md"), readme(name)),
    ];

    let mut created = Vec::with_capacity(files.len());
    for (rel, contents) in files {
        let path = project.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
        created.push(rel);
    }
    Ok(created)
}

/// A project name must be a single safe path segment (no separators, no `..`), so
/// scaffolding can never escape the target directory.
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        bail!("invalid app name `{name}` — use a simple directory name like `my-app`");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusm_bench::{NodeConfig, ServeProtocol};

    #[test]
    fn scaffolds_a_buildable_app() {
        let dir = tempfile::tempdir().unwrap();
        let created = scaffold(dir.path(), "hello").unwrap();

        // Every advertised file exists.
        let root = dir.path().join("hello");
        for rel in &created {
            assert!(root.join(rel).is_file(), "missing {}", rel.display());
        }
        assert!(root.join("components/api/index.ts").is_file());

        // The generated rusm.toml parses and declares the HTTP server we documented.
        let toml = std::fs::read_to_string(root.join("rusm.toml")).unwrap();
        let cfg = NodeConfig::from_toml(&toml).expect("scaffolded rusm.toml must parse");
        assert_eq!(cfg.serve.len(), 1);
        assert_eq!(cfg.serve[0].name, "api");
        assert_eq!(cfg.serve[0].protocol, ServeProtocol::Http);

        // The README is personalised to the app name.
        let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
        assert!(readme.starts_with("# hello"));
    }

    #[test]
    fn into_an_empty_existing_dir_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("blank")).unwrap();
        assert!(scaffold(dir.path(), "blank").is_ok());
    }

    #[test]
    fn refuses_a_non_empty_existing_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("taken")).unwrap();
        std::fs::write(dir.path().join("taken/keep.txt"), "x").unwrap();
        let err = scaffold(dir.path(), "taken").unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn rejects_path_traversal_names() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["..", ".", "a/b", "", "a\\b"] {
            assert!(
                scaffold(dir.path(), bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }
}
