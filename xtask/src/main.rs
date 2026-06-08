//! Project automation for RUSM, run via the `cargo xtask` alias.
//!
//! The only task today is `deploy-docs`: build the VitePress site and publish
//! the static artifact to the `gh-pages` branch, which GitHub Pages serves at
//! https://archan937.github.io/rusm/.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const GH_PAGES_BRANCH: &str = "gh-pages";

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("deploy-docs") => match deploy_docs() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("xtask: deploy-docs failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some(other) => {
            eprintln!("xtask: unknown task `{other}`");
            usage();
            ExitCode::FAILURE
        }
        None => {
            usage();
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!("usage: cargo xtask <task>\n\ntasks:\n  deploy-docs   build the VitePress docs and publish them to the `gh-pages` branch");
}

/// Build `docs/` with VitePress, then force-push the built artifact to
/// `gh-pages`. The push happens from a throwaway git repo created *inside* the
/// build output, so the main working tree is never touched and `gh-pages` stays
/// a single-commit, source-free artifact branch.
fn deploy_docs() -> Result<(), String> {
    let repo_root = repo_root();
    let docs = repo_root.join("docs");
    let dist = docs.join(".vitepress/dist");

    println!("==> Building docs (bun)…");
    run("bun", &["install"], &docs)?;
    run("bun", &["run", "build"], &docs)?;

    if !dist.is_dir() {
        return Err(format!("expected build output at {}", dist.display()));
    }

    // `.nojekyll` stops GitHub Pages from dropping VitePress's `_`-prefixed asset
    // directories.
    std::fs::write(dist.join(".nojekyll"), [])
        .map_err(|e| format!("writing .nojekyll: {e}"))?;

    let origin = capture("git", &["remote", "get-url", "origin"], &repo_root)?;
    let origin = origin.trim();
    println!("==> Publishing to {origin} ({GH_PAGES_BRANCH})…");

    // A fresh, single-commit repo inside dist — force-pushed over gh-pages.
    let git_dir = dist.join(".git");
    let _ = std::fs::remove_dir_all(&git_dir); // clean any leftover from a prior run
    run("git", &["init", "-q"], &dist)?;
    run("git", &["checkout", "-q", "-b", GH_PAGES_BRANCH], &dist)?;
    run("git", &["add", "-A"], &dist)?;
    run("git", &["commit", "-q", "-m", "deploy docs"], &dist)?;
    run(
        "git",
        &["push", "-f", origin, &format!("HEAD:{GH_PAGES_BRANCH}")],
        &dist,
    )?;
    std::fs::remove_dir_all(&git_dir).map_err(|e| format!("cleaning up {}: {e}", git_dir.display()))?;

    println!("\n==> Done. Live (once Pages serves the gh-pages branch):");
    println!("    https://archan937.github.io/rusm/");
    Ok(())
}

fn repo_root() -> PathBuf {
    // This crate lives at <repo>/xtask, so the workspace root is one level up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask crate has a parent directory")
        .to_path_buf()
}

fn run(program: &str, args: &[&str], dir: &Path) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|e| format!("running `{program}`: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{program} {}` exited with {status}", args.join(" ")))
    }
}

fn capture(program: &str, args: &[&str], dir: &Path) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("running `{program}`: {e}"))?;
    if out.status.success() {
        String::from_utf8(out.stdout).map_err(|e| format!("`{program}` output not UTF-8: {e}"))
    } else {
        Err(format!(
            "`{program} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}
