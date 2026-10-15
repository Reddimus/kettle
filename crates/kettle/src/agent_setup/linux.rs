use std::ffi::OsStr;
use std::io::Read as _;
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use kettle_ctl::agent_launch::{HeldExecutable, LaunchRegistration};
use kettle_ctl::show::InlineTarget;

pub(super) struct PreparedLaunch {
    program: PathBuf,
    native: HeldExecutable,
    launcher: Option<HeldExecutable>,
}

impl PreparedLaunch {
    pub(super) fn command(&self, name: &str) -> Option<Command> {
        if !self.native.unchanged() || self.launcher.as_ref().is_some_and(|file| !file.unchanged())
        {
            return None;
        }
        let program = if self.launcher.is_some() {
            self.program.clone()
        } else {
            PathBuf::from(format!("/proc/self/fd/{}", self.native.as_fd().as_raw_fd()))
        };
        let mut command = Command::new(program);
        command.arg0(name);
        Some(command)
    }

    fn register(&self, target: InlineTarget, version: &str) -> bool {
        let Some(registration) =
            LaunchRegistration::new(target, version, &self.native, self.launcher.as_ref())
        else {
            return false;
        };
        let Ok(params) = serde_json::to_value(registration) else {
            return false;
        };
        let Ok(mut client) = kettle_ctl::Client::discover_display(None) else {
            log::debug!("agent cards: launch server unavailable");
            return false;
        };
        client
            .call_with_timeout("agent_launch", params, std::time::Duration::from_secs(2))
            .ok()
            .and_then(|reply| reply.get("enabled").and_then(serde_json::Value::as_bool))
            .unwrap_or(false)
    }
}

pub(super) fn prepare(name: &str, target: InlineTarget, version: &str) -> Option<PreparedLaunch> {
    if !kettle_ctl::agent_launch::card_version(target, version) {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    let cwd = std::env::current_dir().ok()?;
    let prepared = resolve(name, target, version, &path, &cwd)
        .inspect(|_| log::debug!("agent cards: executable resolved"))?;
    let native_path = PathBuf::from(format!(
        "/proc/self/fd/{}",
        prepared.native.as_fd().as_raw_fd()
    ));
    let native_version = super::version_of(&native_path);
    let version_matches = native_version
        .as_deref()
        .is_some_and(|native| native.trim() == version.trim());
    log::debug!("agent cards: held executable version matches={version_matches}");
    if !version_matches {
        return None;
    }
    let registered = prepared.register(target, version);
    log::debug!("agent cards: launch registered={registered}");
    if !registered {
        return None;
    }
    Some(prepared)
}

fn resolve(
    name: &str,
    target: InlineTarget,
    version: &str,
    path: &OsStr,
    cwd: &Path,
) -> Option<PreparedLaunch> {
    if !kettle_ctl::agent_launch::card_version(target, version) {
        return None;
    }
    let program = path_program(name, path, cwd)?;
    if let Ok(native) = HeldExecutable::open(&program) {
        return Some(PreparedLaunch {
            program,
            native,
            launcher: None,
        });
    }
    if target != InlineTarget::CodexHook || program.file_name()? != "codex.js" {
        return None;
    }
    let source = small_file(&program, 64 * 1024)?;
    if source.split(|byte| *byte == b'\n').next()? != b"#!/usr/bin/env node" {
        return None;
    }
    let root = program
        .parent()
        .filter(|directory| directory.file_name() == Some(OsStr::new("bin")))?
        .parent()?;
    let version = version.trim().strip_prefix("codex-cli ")?;
    if !package(root, "@openai/codex", version) {
        return None;
    }
    let (triple, leaf) = triple()?;
    // npm installs the native package through an alias, `@openai/codex-linux-
    // arm64` for `@openai/codex@0.162.1-linux-arm64`, so its own package.json
    // names `@openai/codex` at the platform version.
    let platform_version = format!("{version}-{}", leaf.trim_start_matches("codex-"));
    let candidates = [
        root.join("node_modules/@openai").join(leaf),
        root.parent()?.join(leaf),
    ];
    let native = candidates
        .into_iter()
        .filter(|candidate| package(candidate, "@openai/codex", &platform_version))
        .find_map(|candidate| {
            HeldExecutable::open(&candidate.join("vendor").join(triple).join("bin/codex")).ok()
        })
        .or_else(|| {
            HeldExecutable::open(&root.join("vendor").join(triple).join("bin/codex")).ok()
        })?;
    let launcher = HeldExecutable::open(&path_program("node", path, cwd)?).ok()?;
    Some(PreparedLaunch {
        program,
        native,
        launcher: Some(launcher),
    })
}

fn path_program(name: &str, path: &OsStr, cwd: &Path) -> Option<PathBuf> {
    if path.len() > 32 * 1024 {
        return None;
    }
    for (index, directory) in std::env::split_paths(path).enumerate() {
        if index >= 128 {
            return None;
        }
        let candidate = if directory.is_absolute() {
            directory.join(name)
        } else {
            cwd.join(&directory).join(name)
        };
        let Ok(metadata) = std::fs::metadata(&candidate) else {
            continue;
        };
        if metadata.is_file() && metadata.mode() & 0o111 != 0 {
            // A relative PATH entry wins for ordinary execution too. Never
            // substitute a different absolute candidate just to gain cards.
            return directory
                .is_absolute()
                .then(|| std::fs::canonicalize(candidate).ok())
                .flatten();
        }
    }
    None
}

fn small_file(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file()
        || metadata.len() > limit as u64
        || !kettle_ctl::agent_launch::owned_and_writable_by_owner_alone(&metadata)
    {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= limit).then_some(bytes)
}

fn package(path: &Path, name: &str, version: &str) -> bool {
    small_file(&path.join("package.json"), 8192)
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|value| {
            value.get("name").and_then(serde_json::Value::as_str) == Some(name)
                && value.get("version").and_then(serde_json::Value::as_str) == Some(version)
        })
}

fn triple() -> Option<(&'static str, &'static str)> {
    if cfg!(target_arch = "x86_64") {
        Some(("x86_64-unknown-linux-musl", "codex-linux-x64"))
    } else if cfg!(target_arch = "aarch64") {
        Some(("aarch64-unknown-linux-musl", "codex-linux-arm64"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn write(path: &Path, bytes: &[u8], mode: u32) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    fn elf() -> [u8; 64] {
        let mut header = [0; 64];
        header[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        header[16] = 3;
        header[18] = if cfg!(target_arch = "x86_64") {
            62
        } else {
            183
        };
        header
    }

    #[test]
    fn native_launches_hold_the_exact_path_candidate() {
        let root = kettle_test_support::private_tempdir("kettle-agent-native");
        let bin = root.path().join("bin");
        let program = bin.join("codex");
        write(&program, &elf(), 0o755);
        let launch = resolve(
            "codex",
            InlineTarget::CodexHook,
            "codex-cli 0.162.0",
            bin.as_os_str(),
            root.path(),
        )
        .expect("native launch");
        assert_eq!(launch.native.path(), program.canonicalize().unwrap());
        assert!(launch.launcher.is_none());
        assert_eq!(
            launch.command("codex").unwrap().get_program(),
            format!("/proc/self/fd/{}", launch.native.as_fd().as_raw_fd()).as_str()
        );
    }

    #[test]
    fn npm_launches_bind_node_and_the_native_vendor_child() {
        let root = kettle_test_support::private_tempdir("kettle-agent-npm");
        let bin = root.path().join("bin");
        let package = root.path().join("node_modules/@openai/codex");
        let script = package.join("bin/codex.js");
        write(&script, b"#!/usr/bin/env node\n", 0o755);
        write(
            &package.join("package.json"),
            br#"{"name":"@openai/codex","version":"0.162.0"}"#,
            0o644,
        );
        let native = package
            .join("vendor")
            .join(triple().unwrap().0)
            .join("bin/codex");
        write(&native, &elf(), 0o755);
        write(&bin.join("node"), &elf(), 0o755);
        std::os::unix::fs::symlink(&script, bin.join("codex")).unwrap();
        let launch = resolve(
            "codex",
            InlineTarget::CodexHook,
            "codex-cli 0.162.0",
            bin.as_os_str(),
            root.path(),
        )
        .expect("npm launch");
        assert_eq!(launch.native.path(), native.canonicalize().unwrap());
        assert_eq!(
            launch.launcher.as_ref().unwrap().path(),
            bin.join("node").canonicalize().unwrap()
        );
        assert_eq!(
            launch.command("codex").unwrap().get_program(),
            script.canonicalize().unwrap()
        );
    }

    /// The layout `npm install -g @openai/codex` produces: a bin symlink to
    /// the wrapper, and the native package nested under an alias whose
    /// package.json names `@openai/codex` at the platform version.
    #[test]
    fn a_global_npm_install_binds_its_aliased_native_package() {
        let root = kettle_test_support::private_tempdir("kettle-agent-npm-global");
        let bin = root.path().join("bin");
        let package = root.path().join("lib/node_modules/@openai/codex");
        let script = package.join("bin/codex.js");
        write(&script, b"#!/usr/bin/env node\n", 0o755);
        write(
            &package.join("package.json"),
            br#"{"name":"@openai/codex","version":"0.162.1"}"#,
            0o644,
        );
        let (triple, leaf) = triple().unwrap();
        let optional = package.join("node_modules/@openai").join(leaf);
        let platform = leaf.trim_start_matches("codex-");
        write(
            &optional.join("package.json"),
            format!(r#"{{"name":"@openai/codex","version":"0.162.1-{platform}"}}"#).as_bytes(),
            0o644,
        );
        let native = optional.join("vendor").join(triple).join("bin/codex");
        write(&native, &elf(), 0o755);
        write(&bin.join("node"), &elf(), 0o755);
        std::os::unix::fs::symlink(
            "../lib/node_modules/@openai/codex/bin/codex.js",
            bin.join("codex"),
        )
        .unwrap();
        let launch = resolve(
            "codex",
            InlineTarget::CodexHook,
            "codex-cli 0.162.1",
            bin.as_os_str(),
            root.path(),
        )
        .expect("global npm launch");
        assert_eq!(launch.native.path(), native.canonicalize().unwrap());
        assert_eq!(
            launch.command("codex").unwrap().get_program(),
            script.canonicalize().unwrap()
        );
    }

    #[test]
    fn relative_commands_are_not_replaced_by_later_absolute_candidates() {
        let root = kettle_test_support::private_tempdir("kettle-agent-relative");
        let absolute = root.path().join("absolute");
        write(&root.path().join("relative/codex"), &elf(), 0o755);
        write(&absolute.join("codex"), &elf(), 0o755);
        let path = std::env::join_paths([Path::new("relative"), &absolute]).unwrap();
        assert!(path_program("codex", &path, root.path()).is_none());
    }

    #[test]
    fn native_fd_execution_drops_the_launch_descriptor() {
        let native = HeldExecutable::open(Path::new("/bin/sh")).unwrap();
        let fd = native.as_fd().as_raw_fd();
        let launch = PreparedLaunch {
            program: native.path().to_owned(),
            native,
            launcher: None,
        };
        let mut command = launch.command("sh").unwrap();
        command.args(["-c", &format!("test ! -e /proc/self/fd/{fd}")]);
        let mut child = command.spawn().unwrap();
        println!("owned native descriptor fixture child pid={}", child.id());
        assert!(child.wait().unwrap().success());
    }
}
