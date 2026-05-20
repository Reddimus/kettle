//! Embed the current git SHA in `KETTLE_GIT_SHA` so `kettle --version` can
//! include it. Other Rust CLIs that ship with this affordance (cargo,
//! rustc, ripgrep, fd) use the SHA to disambiguate bug-report builds —
//! without it, "kettle 0.1.0" on every nightly cargo install looks
//! identical to the last release. Cycle 192.
//!
//! Outputs `KETTLE_GIT_SHA` as either:
//! - `" (<sha12>)"` — when in a git checkout, ready to concat onto the
//!   version string with one `concat!()` at the call site.
//! - `""` — source-tarball / vendored / no-git case. `concat!` of an
//!   empty string is a no-op, so the call site doesn't need a cfg.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let repo_root = PathBuf::from(&manifest).join("../..");

    // Tell cargo to re-run this script when the git HEAD moves. `.git/HEAD`
    // changes on branch switch; the *content* of `.git/refs/heads/<branch>`
    // changes on every commit, so watch both. If the working copy isn't a
    // git checkout, the rerun-if-changed lines simply don't match anything.
    let head_path = repo_root.join(".git/HEAD");
    println!("cargo:rerun-if-changed={}", head_path.display());
    if let Ok(head) = std::fs::read_to_string(&head_path)
        && let Some(rest) = head.trim().strip_prefix("ref: ")
    {
        let ref_path = repo_root.join(".git").join(rest);
        println!("cargo:rerun-if-changed={}", ref_path.display());
    }

    let sha = Command::new("git")
        .args(["-C", repo_root.to_str().unwrap_or(".")])
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    match sha {
        Some(sha) => println!("cargo:rustc-env=KETTLE_GIT_SHA= ({sha})"),
        None => println!("cargo:rustc-env=KETTLE_GIT_SHA="),
    }
}
