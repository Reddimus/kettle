//! Populate the compatibility data in an otherwise assembled Linux package.

use kettle_update::worker_package::{CAPSULE_BYTES, CAPSULE_METADATA, WorkerCapsule};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let root = std::path::PathBuf::from(arguments.next().ok_or("missing package root")?);
    let target = arguments
        .next()
        .ok_or("missing package target")?
        .into_string()
        .map_err(|_| "target is not UTF-8")?;
    if arguments.next().is_some()
        || !matches!(
            target.as_str(),
            "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu"
        )
    {
        return Err("expected package root and Linux target".into());
    }
    let bytes = std::fs::read(root.join("kettle-media-worker"))?;
    let metadata = WorkerCapsule::for_package(target, &bytes);
    std::fs::create_dir_all(root.join("shell-integration"))?;
    std::fs::write(root.join(CAPSULE_BYTES), &bytes)?;
    std::fs::write(
        root.join(CAPSULE_METADATA),
        serde_json::to_vec_pretty(&metadata)?,
    )?;
    Ok(())
}
