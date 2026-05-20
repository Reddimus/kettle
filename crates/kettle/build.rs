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

    // Cycle 195 note: we intentionally *don't* call
    // `cargo:rerun-if-changed=…` here. Restricting the rerun set to
    // `.git/HEAD` + the symbolic-ref file (the cycle-192 design) made
    // the script rerun on commit / branch switch — but NOT on a
    // source-file edit, which is exactly when the `+dirty` marker
    // needs to refresh. The two `git` invocations below are ~10ms
    // total; running the build script on every cargo build is well
    // under the noise floor of any real build. The cost-benefit
    // pivots once `+dirty` matters more than ~10ms per build, and it
    // does for bug reports against dev-iter builds with uncommitted
    // changes.

    let repo_arg = repo_root.to_str().unwrap_or(".");
    let sha = Command::new("git")
        .args(["-C", repo_arg, "rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    // Cycle 195: tag the SHA with `+dirty` if the working tree has
    // uncommitted changes. Without this, a dev build with edits to
    // `src/main.rs` reports the same SHA as the clean tip — bug
    // reports against custom builds are indistinguishable from
    // bug reports against the matching upstream commit. `git status
    // --porcelain` produces empty output on a clean tree, non-empty
    // on any modification (tracked or untracked). Mirrors the
    // `git describe --dirty` convention.
    let dirty = Command::new("git")
        .args(["-C", repo_arg, "status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    match sha {
        Some(sha) if dirty => println!("cargo:rustc-env=KETTLE_GIT_SHA= ({sha}+dirty)"),
        Some(sha) => println!("cargo:rustc-env=KETTLE_GIT_SHA= ({sha})"),
        None => println!("cargo:rustc-env=KETTLE_GIT_SHA="),
    }
}
