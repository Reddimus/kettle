//! Embed the Git revision and source identity for version reports and workers.
//! The Git suffix is empty outside a checkout, `(sha12)` for a clean checkout,
//! or `(sha12+dirty)` for tracked or untracked working changes.

#[path = "build_support/source_id.rs"]
mod source_id;

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let repo_root = PathBuf::from(&manifest).join("../..");

    // On Windows targets, embed packaging/windows/kettle.ico into the
    // .exe as a resource so the taskbar, Explorer, and Alt-Tab all
    // display our icon. No-op on Linux/macOS builds — the `target_os`
    // cfg-gates the entire winresource path so cross-platform builds
    // never even consider it. `set_icon` accepts a path relative to
    // the crate root, hence `../../packaging/...`.
    #[cfg(target_os = "windows")]
    {
        let ico = repo_root.join("packaging/windows/kettle.ico");
        if ico.exists() {
            let mut res = winresource::WindowsResource::new();
            res.set_icon(ico.to_str().unwrap_or_default());
            if let Err(e) = res.compile() {
                // Don't fail the build on a resource-compile glitch —
                // the .exe will just lack an icon and a contributor
                // working without the MSVC resource compiler can
                // still iterate. Print to stderr so it shows up in
                // `cargo build --verbose` but stays out of the way.
                eprintln!("warning: winresource compile failed: {e}");
            }
            println!("cargo:rerun-if-changed=../../packaging/windows/kettle.ico");
        }
    }

    // Keep this binary's Git status current, including untracked files.
    // Source-only libraries use the registered inputs instead of forced reruns.
    println!("cargo:rerun-if-changed=NONEXISTENT_FORCE_RERUN_FOR_KETTLE_GIT_SHA");

    let repo_arg = repo_root.to_str().unwrap_or(".");
    let sha = Command::new("git")
        .args(["-C", repo_arg, "rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    // `git status` includes both tracked and untracked changes.
    let dirty = Command::new("git")
        .args(["-C", repo_arg, "status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    match &sha {
        Some(sha) if dirty => println!("cargo:rustc-env=KETTLE_GIT_SHA= ({sha}+dirty)"),
        Some(sha) => println!("cargo:rustc-env=KETTLE_GIT_SHA= ({sha})"),
        None => println!("cargo:rustc-env=KETTLE_GIT_SHA="),
    }

    // `KETTLE_SOURCE_ID` names the source this binary was built from, and
    // `KETTLE_SOURCE_HASH` is its hex hash alone. A worker built beside this
    // binary must share it with the GUI that starts it (an update may have
    // replaced an executable since), because the frames between them are
    // defined by the source. See `build_support/source_id.rs`.
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let hash = format!("{:016x}", source_id::tracked_source_hash(&repo_root));
    println!("cargo:rustc-env=KETTLE_SOURCE_ID={version} ({hash})");
    println!("cargo:rustc-env=KETTLE_SOURCE_HASH={hash}");
}
