//! `rusm new <name> [--rust] [--protocol http|sse|ws]` — scaffold a new RUSM app.
//!
//! Produces a project whose component source is **pure developer logic** — no
//! `wit-bindgen`/`export!` boilerplate (Rust hides it behind `#[rusm_rs::main]`) and
//! no `Process`/frame plumbing (TS uses web standards and the `rusm` package). Pick a
//! language (`--rust`, default TypeScript) and a protocol (`--protocol`, default
//! `http`); from nothing to a live server in three commands:
//!
//! ```text
//! rusm new hello && cd hello
//! rusm build      # components/<name>/ -> wasm/<name>.{js,wasm}
//! rusm serve      # hosts it on http://127.0.0.1:8080
//! ```

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// The guest language for the scaffolded component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    TypeScript,
    Rust,
}

/// The protocol the component is served over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Http,
    Sse,
    Ws,
}

impl Protocol {
    fn as_str(self) -> &'static str {
        match self {
            Protocol::Http => "http",
            Protocol::Sse => "sse",
            Protocol::Ws => "ws",
        }
    }
}

/// A parsed `rusm new` invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewApp {
    pub name: String,
    pub lang: Lang,
    pub protocol: Protocol,
}

/// Parse the arguments following `rusm new` into a [`NewApp`]: a single positional
/// name plus optional `--rust`/`--lang <ts|rust>` and `--protocol <http|sse|ws>`
/// (`-p`, `--protocol=…` also accepted). Unknown flags, bad values, and a missing or
/// duplicate name are hard errors — a typo never silently scaffolds the wrong thing.
pub fn parse_new_args(rest: &[String]) -> Result<NewApp> {
    let mut name: Option<String> = None;
    let mut lang = Lang::TypeScript;
    let mut protocol = Protocol::Http;

    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--rust" => lang = Lang::Rust,
            "--lang" => lang = parse_lang(next_value(&mut it, "--lang")?)?,
            l if l.starts_with("--lang=") => lang = parse_lang(&l["--lang=".len()..])?,
            "--protocol" | "-p" => protocol = parse_protocol(next_value(&mut it, "--protocol")?)?,
            p if p.starts_with("--protocol=") => {
                protocol = parse_protocol(&p["--protocol=".len()..])?
            }
            flag if flag.starts_with('-') => bail!("unknown option `{flag}`"),
            positional if name.is_none() => name = Some(positional.to_string()),
            extra => bail!(
                "unexpected argument `{extra}` (the app name is already `{}`)",
                { name.as_deref().unwrap_or_default() }
            ),
        }
    }

    let name = name.context("usage: rusm new <name> [--rust] [--protocol http|sse|ws]")?;
    validate_name(&name)?;
    Ok(NewApp {
        name,
        lang,
        protocol,
    })
}

fn next_value<'a>(it: &mut std::slice::Iter<'a, String>, flag: &str) -> Result<&'a str> {
    it.next()
        .map(String::as_str)
        .with_context(|| format!("`{flag}` needs a value"))
}

fn parse_lang(value: &str) -> Result<Lang> {
    match value {
        "ts" | "typescript" => Ok(Lang::TypeScript),
        "rust" | "rs" => Ok(Lang::Rust),
        other => bail!("unknown language `{other}` — use `ts` or `rust`"),
    }
}

fn parse_protocol(value: &str) -> Result<Protocol> {
    match value {
        "http" => Ok(Protocol::Http),
        "sse" => Ok(Protocol::Sse),
        "ws" => Ok(Protocol::Ws),
        other => bail!("unknown protocol `{other}` — use `http`, `sse`, or `ws`"),
    }
}

/// Scaffolds the app at `<root>/<name>`, returning the files created (relative to the
/// project). Fails if the target directory exists and is non-empty, so an existing
/// project is never clobbered.
pub fn scaffold(root: &Path, app: &NewApp) -> Result<Vec<PathBuf>> {
    let project = root.join(&app.name);
    if project.exists()
        && project
            .read_dir()
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
    {
        bail!("`{}` already exists and is not empty", app.name);
    }

    let mut created = Vec::new();
    for (rel, contents) in files(app) {
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

/// The full set of (relative path, contents) for an app — the single place that maps
/// a (language, protocol) to its files.
fn files(app: &NewApp) -> Vec<(PathBuf, String)> {
    let mut out = vec![
        (PathBuf::from("rusm.toml"), rusm_toml(app)),
        (PathBuf::from(".gitignore"), GITIGNORE.to_string()),
        (PathBuf::from("README.md"), readme(app)),
    ];
    match app.lang {
        Lang::TypeScript => {
            out.push((
                PathBuf::from("components/api/index.ts"),
                ts_component(app.protocol).to_string(),
            ));
            out.push((PathBuf::from("tsconfig.json"), TSCONFIG.to_string()));
            // Only a `rusm`-importing component needs a manifest + install; HTTP/SSE
            // are zero-dependency web-standard handlers.
            if app.protocol == Protocol::Ws {
                out.push((PathBuf::from("package.json"), package_json(&app.name)));
            }
        }
        Lang::Rust => {
            out.push((
                PathBuf::from("components/api/Cargo.toml"),
                CARGO_TOML.to_string(),
            ));
            out.push((
                PathBuf::from("components/api/src/lib.rs"),
                rust_component(app.protocol).to_string(),
            ));
        }
    }
    out
}

/// A resident handler holds state across requests/connections; per-request gives a
/// fresh sandboxed instance each time. TS HTTP/SSE run per-request on the js-runner;
/// TS WS and every Rust handler are resident (the ergonomic `serve` APIs are resident).
fn is_resident(app: &NewApp) -> bool {
    matches!(
        (app.lang, app.protocol),
        (Lang::TypeScript, Protocol::Ws) | (Lang::Rust, _)
    )
}

fn rusm_toml(app: &NewApp) -> String {
    let artifact = match app.lang {
        Lang::TypeScript => "wasm/api.js",
        Lang::Rust => "wasm/api.wasm",
    };
    let mode = if is_resident(app) {
        let note = if app.protocol == Protocol::Ws {
            "one instance serves every connection (shared state)"
        } else {
            "a long-lived instance; state persists across requests"
        };
        format!("\nmode = \"resident\"        # {note}")
    } else {
        String::new()
    };
    format!(
        "# RUSM app config. `rusm serve` hosts each [[serve]] entry; `rusm build` compiles\n\
         # components/<name>/ into wasm/ first. See https://github.com/archan937/rusm.\n\
         \n\
         [[serve]]\n\
         name = \"api\"                # loads {artifact}, built from components/api\n\
         protocol = \"{proto}\"           # http | sse | ws\n\
         listen = \"127.0.0.1:8080\"\n\
         capability = \"sandboxed\"     # default-deny; see [capabilities.<name>] for more{mode}\n",
        proto = app.protocol.as_str(),
    )
}

/// Build output and installed dependencies are not source.
const GITIGNORE: &str = "/wasm/\n/node_modules/\n/target/\n";

/// One tsconfig for any TS component: web-standard `Request`/`Response`/streams come
/// from the DOM lib, and bundler resolution finds the `rusm` package (WS).
const TSCONFIG: &str = "\
{
  \"compilerOptions\": {
    \"target\": \"ES2022\",
    \"module\": \"ESNext\",
    \"moduleResolution\": \"bundler\",
    \"lib\": [\"ES2022\", \"DOM\"],
    \"strict\": true,
    \"skipLibCheck\": true,
    \"noEmit\": true,
    \"types\": []
  },
  \"include\": [\"components/**/*.ts\"]
}
";

fn package_json(name: &str) -> String {
    format!(
        "{{\n  \"name\": \"{name}\",\n  \"private\": true,\n  \"type\": \"module\",\n  \"dependencies\": {{\n    \"rusm\": \"^0.1.0\"\n  }}\n}}\n"
    )
}

/// The Rust component crate — one `cdylib`, the `rusm-rs` guest crate, and
/// `wit-bindgen` (which `#[rusm_rs::main]` drives so the source carries no `wit/`).
const CARGO_TOML: &str = "\
[package]
name = \"api\"
version = \"0.1.0\"
edition = \"2021\"

[lib]
crate-type = [\"cdylib\"]

[dependencies]
rusm-rs = \"0.1\"
wit-bindgen = \"0.46\"

[profile.release]
opt-level = \"z\"
strip = true

[workspace]
";

fn ts_component(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Http => {
            "\
// A RUSM HTTP component: export a default handler. `rusm serve` runs it with one
// sandboxed WASM instance per request — you write the handler, RUSM owns the rest.
export default function handle(request: Request): Response {
  const url = new URL(request.url);
  return new Response(`Hello from RUSM \u{1F44B}  (you asked for ${url.pathname})\\n`, {
    headers: { \"content-type\": \"text/plain\" },
  });
}
"
        }
        Protocol::Sse => {
            "\
// A RUSM SSE component: return a streaming `text/event-stream` Response. Each chunk
// is one Server-Sent Event; close the controller to end the stream.
export default function handle(_request: Request): Response {
  const encoder = new TextEncoder();
  let n = 0;
  const body = new ReadableStream({
    pull(controller) {
      if (n >= 5) return controller.close();
      controller.enqueue(encoder.encode(`data: tick ${n++}\\n\\n`));
    },
  });
  return new Response(body, { headers: { \"content-type\": \"text/event-stream\" } });
}
"
        }
        Protocol::Ws => {
            "\
// A RUSM WebSocket component: one instance serves every connection. Reply with
// `socket.send(...)`; keep shared state (rooms, presence) in the handler's closure.
import { websocket } from \"rusm\";

export default websocket({
  open(socket) {
    socket.send(\"welcome to RUSM\\n\");
  },
  message(socket, data) {
    socket.send(data); // echo the frame back to the sender
  },
});
"
        }
    }
}

fn rust_component(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Http => {
            "\
//! A RUSM HTTP component: implement `Handler` and serve it. The instance is
//! long-lived, so `&mut self` state persists across requests.
use rusm_rs::http::{Handler, Request, Response};

#[derive(Default)]
struct Api {
    hits: u64,
}

impl Handler for Api {
    fn handle(&mut self, _request: Request) -> Response {
        self.hits += 1;
        Response::text(format!(\"Hello from RUSM \u{1F44B}  (hit #{})\\n\", self.hits))
    }
}

#[rusm_rs::main]
fn main() {
    rusm_rs::http::serve(Api::default());
}
"
        }
        Protocol::Sse => {
            "\
//! A RUSM SSE component: yield the event chunks for each request; they stream to the
//! client as a `text/event-stream` body, with the byte stream's natural back-pressure.
#[rusm_rs::main]
fn main() {
    rusm_rs::http::serve_sse(|_request| {
        (0..5).map(|n| format!(\"data: tick {n}\\n\\n\").into_bytes())
    });
}
"
        }
        Protocol::Ws => {
            "\
//! A RUSM WebSocket component: one instance serves every connection. Reply to a
//! connection by sending bytes to its `conn` pid; hold shared state in `self`.
use rusm_rs::ws::{self, Handler};
use rusm_rs::Pid;

#[derive(Default)]
struct Api;

impl Handler for Api {
    fn open(&mut self, conn: Pid) {
        ws::send(conn, b\"welcome to RUSM\\n\");
    }
    fn message(&mut self, conn: Pid, data: Vec<u8>) {
        ws::send(conn, &data); // echo the frame back to the sender
    }
}

#[rusm_rs::main]
fn main() {
    ws::serve(Api::default());
}
"
        }
    }
}

fn readme(app: &NewApp) -> String {
    let name = &app.name;
    let lang = match app.lang {
        Lang::TypeScript => "TypeScript",
        Lang::Rust => "Rust",
    };
    let source = match app.lang {
        Lang::TypeScript => "components/api/index.ts",
        Lang::Rust => "components/api/src/lib.rs",
    };
    let probe = match app.protocol {
        Protocol::Http => "curl http://127.0.0.1:8080/",
        Protocol::Sse => "curl -N http://127.0.0.1:8080/        # streams events; Ctrl-C to stop",
        Protocol::Ws => "websocat ws://127.0.0.1:8080/          # type a line; it echoes back",
    };
    format!(
        "# {name}\n\n\
         A RUSM app — a {lang} **{proto}** component running as an isolated, supervised\n\
         WASM process on an Erlang-style actor runtime.\n\n\
         ## Run it\n\n\
         ```sh\n\
         rusm build      # compile components/ -> wasm/\n\
         rusm serve      # serve on http://127.0.0.1:8080\n\
         ```\n\n\
         Then, in another terminal:\n\n\
         ```sh\n\
         {probe}\n\
         ```\n\n\
         ## Layout\n\n\
         - `{source}` — the handler (edit this).\n\
         - `rusm.toml` — what to serve, on which port, under which capability profile.\n\
         - `wasm/` — built artifacts (git-ignored); produced by `rusm build`.\n\n\
         Add more components under `components/<name>/` and reference them from `rusm.toml`.\n",
        proto = app.protocol.as_str(),
    )
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
    use rusm_bench::{NodeConfig, ServeMode, ServeProtocol};

    fn app(lang: Lang, protocol: Protocol) -> NewApp {
        NewApp {
            name: "demo".into(),
            lang,
            protocol,
        }
    }

    const COMBOS: &[(Lang, Protocol, ServeProtocol, bool)] = &[
        (Lang::TypeScript, Protocol::Http, ServeProtocol::Http, false),
        (Lang::TypeScript, Protocol::Sse, ServeProtocol::Sse, false),
        (Lang::TypeScript, Protocol::Ws, ServeProtocol::Ws, true),
        (Lang::Rust, Protocol::Http, ServeProtocol::Http, true),
        (Lang::Rust, Protocol::Sse, ServeProtocol::Sse, true),
        (Lang::Rust, Protocol::Ws, ServeProtocol::Ws, true),
    ];

    #[test]
    fn every_combo_scaffolds_a_coherent_app() {
        for &(lang, protocol, want_proto, resident) in COMBOS {
            let dir = tempfile::tempdir().unwrap();
            let app = app(lang, protocol);
            let created = scaffold(dir.path(), &app).unwrap();
            let root = dir.path().join("demo");

            // Every advertised file is on disk.
            for rel in &created {
                assert!(
                    root.join(rel).is_file(),
                    "{lang:?}/{protocol:?}: missing {rel:?}"
                );
            }

            // The right component source exists for the language, and no boilerplate
            // leaks into it.
            let (src, forbidden): (PathBuf, &[&str]) = match lang {
                Lang::TypeScript => (
                    "components/api/index.ts".into(),
                    &["declare const Process", "Process.receive", "wit_bindgen"],
                ),
                Lang::Rust => (
                    "components/api/src/lib.rs".into(),
                    &["wit_bindgen::generate", "export!(", "impl Guest"],
                ),
            };
            let source = std::fs::read_to_string(root.join(&src)).unwrap();
            for needle in forbidden {
                assert!(
                    !source.contains(needle),
                    "{lang:?}/{protocol:?}: leaked boilerplate `{needle}`"
                );
            }

            // The generated rusm.toml parses through the real config and declares the
            // right protocol + mode.
            let toml = std::fs::read_to_string(root.join("rusm.toml")).unwrap();
            let cfg = NodeConfig::from_toml(&toml).expect("scaffolded rusm.toml must parse");
            assert_eq!(cfg.serve.len(), 1);
            assert_eq!(cfg.serve[0].name, "api");
            assert_eq!(cfg.serve[0].protocol, want_proto, "{lang:?}/{protocol:?}");
            let want_mode = if resident {
                ServeMode::Resident
            } else {
                ServeMode::PerRequest
            };
            assert_eq!(cfg.serve[0].mode, want_mode, "{lang:?}/{protocol:?}");
        }
    }

    #[test]
    fn rust_components_carry_no_wit_dir_and_use_the_main_macro() {
        let dir = tempfile::tempdir().unwrap();
        scaffold(dir.path(), &app(Lang::Rust, Protocol::Http)).unwrap();
        let root = dir.path().join("demo");
        assert!(
            !root.join("components/api/wit").exists(),
            "no wit/ dir needed"
        );
        let src = std::fs::read_to_string(root.join("components/api/src/lib.rs")).unwrap();
        assert!(src.contains("#[rusm_rs::main]"));
    }

    #[test]
    fn only_the_rusm_importing_ts_component_gets_a_package_json() {
        let dir = tempfile::tempdir().unwrap();
        scaffold(dir.path(), &app(Lang::TypeScript, Protocol::Ws)).unwrap();
        assert!(dir.path().join("demo/package.json").is_file());
        assert!(
            std::fs::read_to_string(dir.path().join("demo/components/api/index.ts"))
                .unwrap()
                .contains("import { websocket } from \"rusm\"")
        );

        let dir2 = tempfile::tempdir().unwrap();
        scaffold(dir2.path(), &app(Lang::TypeScript, Protocol::Http)).unwrap();
        assert!(
            !dir2.path().join("demo/package.json").exists(),
            "a zero-dep web-standard handler needs no package.json"
        );
    }

    #[test]
    fn parses_flags_with_sensible_defaults() {
        let p =
            |args: &[&str]| parse_new_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let d = p(&["hello"]).unwrap();
        assert_eq!(d.lang, Lang::TypeScript);
        assert_eq!(d.protocol, Protocol::Http);

        assert_eq!(p(&["hello", "--rust"]).unwrap().lang, Lang::Rust);
        assert_eq!(p(&["hello", "--lang", "rust"]).unwrap().lang, Lang::Rust);
        assert_eq!(
            p(&["hello", "--protocol", "ws"]).unwrap().protocol,
            Protocol::Ws
        );
        assert_eq!(
            p(&["hello", "--protocol=sse"]).unwrap().protocol,
            Protocol::Sse
        );
        assert_eq!(p(&["hello", "-p", "ws"]).unwrap().protocol, Protocol::Ws);
        // Order-independent.
        let mixed = p(&["--rust", "-p", "sse", "hello"]).unwrap();
        assert_eq!(
            (mixed.lang, mixed.protocol, mixed.name.as_str()),
            (Lang::Rust, Protocol::Sse, "hello")
        );
    }

    #[test]
    fn rejects_bad_input() {
        let p =
            |args: &[&str]| parse_new_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(p(&[]).is_err(), "missing name");
        assert!(p(&["a", "b"]).is_err(), "two names");
        assert!(p(&["hello", "--protocol", "grpc"]).is_err(), "bad protocol");
        assert!(p(&["hello", "--lang", "go"]).is_err(), "bad language");
        assert!(p(&["hello", "--frobnicate"]).is_err(), "unknown flag");
        assert!(
            p(&["hello", "--protocol"]).is_err(),
            "missing protocol value"
        );
        for bad in ["..", ".", "a/b", "", "a\\b"] {
            assert!(p(&[bad]).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn refuses_a_non_empty_existing_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("taken")).unwrap();
        std::fs::write(dir.path().join("taken/keep.txt"), "x").unwrap();
        let occupied = NewApp {
            name: "taken".into(),
            lang: Lang::TypeScript,
            protocol: Protocol::Http,
        };
        let err = scaffold(dir.path(), &occupied).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }
}
