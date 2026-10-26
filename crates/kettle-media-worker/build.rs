//! Embed `KETTLE_SOURCE_HASH`, the source hash this worker shares with the
//! `kettle` GUI that starts it, from the same build helper kettle's own build
//! script uses, so both name the same source.

#[path = "../kettle/build_support/source_id.rs"]
mod source_id;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let repo_root = std::path::PathBuf::from(manifest).join("../..");
    println!(
        "cargo:rustc-env=KETTLE_SOURCE_HASH={:016x}",
        source_id::tracked_source_hash(&repo_root)
    );
}
