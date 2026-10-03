//! Where this build's media worker is installed, and whether it may run.
//!
//! `kettle-media` decides what an answer means; this module does the work it
//! cannot. It reads the running executable's directory once, at startup,
//! checks the worker file beside it, and on macOS checks the worker's code
//! signature against Kettle's own requirement. It never looks in `PATH`, the
//! working directory, the environment or the configuration, and it starts no
//! process except `codesign`.
//!
//! These checks keep a stray, half-installed or foreign file from running as
//! the worker. They cannot stop a program running as the same user, which can
//! rewrite a user-owned install directly; installation authenticity comes from
//! the signed release and its package hashes.

use std::path::{Path, PathBuf};

use kettle_media::client::{FileIdentity, UnavailableCause, WorkerPlatform};

/// The worker's file name, beside the `kettle` executable.
const WORKER_NAME: &str = "kettle-media-worker";

pub(crate) struct InstalledWorker {
    path: Result<PathBuf, UnavailableCause>,
    /// The code requirement the worker's signature must meet.
    #[cfg(target_os = "macos")]
    requirement: String,
}

impl InstalledWorker {
    /// Record the worker's path from the running executable now. A later
    /// rename or deletion of the executable does not move it.
    pub(crate) fn capture() -> Self {
        Self::beside(std::env::current_exe())
    }

    fn beside(executable: std::io::Result<PathBuf>) -> Self {
        Self {
            path: worker_beside(executable),
            #[cfg(target_os = "macos")]
            requirement: signature::official_requirement(),
        }
    }
}

/// The worker beside `executable`, with links in the executable's path
/// resolved, so a Homebrew link finds the install it points into.
fn worker_beside(executable: std::io::Result<PathBuf>) -> Result<PathBuf, UnavailableCause> {
    if cfg!(not(any(target_os = "macos", target_os = "linux"))) {
        return Err(UnavailableCause::UnsupportedPlatform);
    }
    // On Linux a deleted executable reads as "<path> (deleted)", which does
    // not resolve.
    let executable = executable
        .and_then(std::fs::canonicalize)
        .map_err(|_| UnavailableCause::NoInstallLocation)?;
    let directory = executable
        .parent()
        .filter(|directory| directory.is_absolute())
        .ok_or(UnavailableCause::NoInstallLocation)?;
    Ok(directory.join(WORKER_NAME))
}

impl WorkerPlatform for InstalledWorker {
    fn worker_path(&self) -> Result<&Path, UnavailableCause> {
        self.path.as_deref().map_err(|cause| *cause)
    }

    fn inspect(&self, path: &Path) -> Result<FileIdentity, UnavailableCause> {
        inspect(path)
    }

    fn verify(&self, path: &Path) -> Result<(), UnavailableCause> {
        #[cfg(target_os = "macos")]
        return signature::verify(path, &self.requirement);
        // The package install checked the worker's bytes against the signed
        // release; there is no signature to check at run time.
        #[cfg(not(target_os = "macos"))]
        {
            let _ = path;
            Ok(())
        }
    }
}

/// A regular executable file, not a link, owned by this user or root, with
/// neither it nor its directory writable by anyone else, and no set-id bits.
#[cfg(unix)]
fn inspect(path: &Path) -> Result<FileIdentity, UnavailableCause> {
    use std::os::unix::fs::MetadataExt as _;

    let leaf = match std::fs::symlink_metadata(path) {
        Ok(leaf) => leaf,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(UnavailableCause::WorkerMissing);
        }
        Err(_) => return Err(UnavailableCause::UnsafeWorkerFile),
    };
    let directory = path
        .parent()
        .and_then(|directory| std::fs::metadata(directory).ok())
        .ok_or(UnavailableCause::UnsafeWorkerFile)?;
    // SAFETY: geteuid has no preconditions.
    let user = unsafe { libc::geteuid() };
    let owned = |uid: u32| uid == 0 || uid == user;
    let safe = leaf.file_type().is_file()
        && leaf.mode() & 0o111 != 0
        && leaf.mode() & 0o6022 == 0
        && owned(leaf.uid())
        && directory.is_dir()
        && directory.mode() & 0o022 == 0
        && owned(directory.uid());
    if !safe {
        return Err(UnavailableCause::UnsafeWorkerFile);
    }
    Ok(FileIdentity {
        dev: leaf.dev(),
        ino: leaf.ino(),
        size: leaf.size(),
        mtime_seconds: leaf.mtime(),
        mtime_nanos: leaf.mtime_nsec(),
        ctime_seconds: leaf.ctime(),
        ctime_nanos: leaf.ctime_nsec(),
    })
}

#[cfg(not(unix))]
fn inspect(_: &Path) -> Result<FileIdentity, UnavailableCause> {
    Err(UnavailableCause::UnsupportedPlatform)
}

/// The worker's code signature, checked with the `codesign` macOS ships, as
/// the updater checks the app bundle.
#[cfg(target_os = "macos")]
mod signature {
    use std::io::Read as _;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use kettle_media::client::UnavailableCause;

    const CODESIGN: &str = "/usr/bin/codesign";
    /// How long one `codesign` run may take. A run that outlasts it is
    /// `check_failed`, so a stuck tool cannot hold the answer at `checking`.
    const CODESIGN_DEADLINE: Duration = Duration::from_secs(30);
    /// More than `codesign --display` ever reports; the rest is not read.
    const MAX_REPORT_BYTES: u64 = 64 * 1024;
    /// The worker's signing identifier. The app's is `org.kettle.terminal`, so
    /// the app's own requirement cannot be reused for the worker.
    pub(super) const WORKER_IDENTIFIER: &str = "org.kettle.terminal.media-worker";
    /// The CodeDirectory flag for the hardened runtime.
    const CS_RUNTIME: u32 = 0x1_0000;

    /// Apple's chain to a Developer ID Application certificate issued to
    /// Kettle's team, under the worker's own identifier.
    pub(super) fn official_requirement() -> String {
        format!(
            "anchor apple generic and identifier \"{WORKER_IDENTIFIER}\" \
             and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
             and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
             and certificate leaf[subject.OU] = \"{}\"",
            kettle_update::APPLE_TEAM_IDENTIFIER
        )
    }

    /// A strict signature check of every architecture against `requirement`,
    /// then the hardened runtime, which a requirement cannot express.
    pub(super) fn verify(path: &Path, requirement: &str) -> Result<(), UnavailableCause> {
        let mut verify = Command::new(CODESIGN);
        verify
            .args(["--verify", "--strict", "-R"])
            .arg(format!("={requirement}"))
            .arg("--")
            .arg(path);
        let (verified, _) = run_bounded(verify, CODESIGN_DEADLINE)?;
        if !verified {
            return Err(UnavailableCause::Unverified);
        }
        // `codesign --display` reports on stderr.
        let mut display = Command::new(CODESIGN);
        display.args(["--display", "--verbose=1", "--"]).arg(path);
        let (described, report) = run_bounded(display, CODESIGN_DEADLINE)?;
        if !described || !hardened_runtime(&String::from_utf8_lossy(&report)) {
            return Err(UnavailableCause::Unverified);
        }
        Ok(())
    }

    /// Run `command` with no input or standard output, and report whether it
    /// succeeded and the start of what it wrote to stderr. One that runs past
    /// `deadline` is killed and reaped.
    pub(super) fn run_bounded(
        mut command: Command,
        deadline: Duration,
    ) -> Result<(bool, Vec<u8>), UnavailableCause> {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| UnavailableCause::CheckFailed)?;
        let until = Instant::now() + deadline;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(5)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(UnavailableCause::CheckFailed);
                }
            }
        };
        // The report is small enough to sit in the pipe until the tool exits.
        let mut report = Vec::new();
        if let Some(stderr) = child.stderr.take() {
            let _ = stderr.take(MAX_REPORT_BYTES).read_to_end(&mut report);
        }
        Ok((status.success(), report))
    }

    /// Whether `codesign --display` reports a CodeDirectory with the hardened
    /// runtime flag, as in `CodeDirectory v=20500 size=… flags=0x10000(runtime)`.
    pub(super) fn hardened_runtime(described: &str) -> bool {
        described
            .lines()
            .filter(|line| line.starts_with("CodeDirectory "))
            .flat_map(str::split_whitespace)
            .filter_map(|field| field.strip_prefix("flags=0x"))
            .filter_map(|flags| {
                let hex = flags.split('(').next().unwrap_or_default();
                u32::from_str_radix(hex, 16).ok()
            })
            .any(|flags| flags & CS_RUNTIME != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worker_is_beside_the_executable() {
        let install = tempfile::tempdir().unwrap();
        let executable = install.path().join("kettle");
        std::fs::write(&executable, "").unwrap();
        let worker = InstalledWorker::beside(Ok(executable.clone()));
        let expected = install.path().canonicalize().unwrap().join(WORKER_NAME);
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert_eq!(worker.worker_path(), Ok(expected.as_path()));
            // Renaming the executable afterwards does not move the worker.
            std::fs::rename(&executable, install.path().join("kettle.old")).unwrap();
            assert_eq!(worker.worker_path(), Ok(expected.as_path()));
        } else {
            assert_eq!(
                worker.worker_path(),
                Err(UnavailableCause::UnsupportedPlatform)
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_executable_finds_the_install_it_points_into() {
        let install = tempfile::tempdir().unwrap();
        let links = tempfile::tempdir().unwrap();
        let executable = install.path().join("kettle");
        std::fs::write(&executable, "").unwrap();
        let link = links.path().join("kettle");
        std::os::unix::fs::symlink(&executable, &link).unwrap();
        let worker = InstalledWorker::beside(Ok(link));
        let expected = install.path().canonicalize().unwrap().join(WORKER_NAME);
        assert_eq!(worker.worker_path(), Ok(expected.as_path()));
    }

    #[cfg(unix)]
    #[test]
    fn an_unresolvable_executable_has_no_install_location() {
        let install = tempfile::tempdir().unwrap();
        for executable in [
            Err(std::io::Error::other("no executable path")),
            Ok(install.path().join("kettle (deleted)")),
        ] {
            assert_eq!(
                InstalledWorker::beside(executable).worker_path(),
                Err(UnavailableCause::NoInstallLocation)
            );
        }
    }

    /// Run `test` in a child of this test binary whose `PATH` and working
    /// directory both hold a decoy worker, and resolve the real one there.
    #[cfg(unix)]
    fn resolve_with_decoy(test: &str, on_path: bool) {
        const CHILD_ENV: &str = "KETTLE_MEDIA_WORKER_DECOY_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let decoys = tempfile::tempdir().unwrap();
            let decoy = decoys.path().join(WORKER_NAME);
            std::fs::write(&decoy, "#!/bin/sh\nexit 0\n").unwrap();
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&decoy, std::fs::Permissions::from_mode(0o755)).unwrap();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args(["--exact", test, "--nocapture"])
                .env(CHILD_ENV, "1");
            if on_path {
                child.env("PATH", decoys.path());
            } else {
                child.current_dir(decoys.path());
            }
            let status = child.status().unwrap();
            assert!(status.success(), "decoy child failed: {status}");
            return;
        }
        let worker = InstalledWorker::capture();
        let path = worker.worker_path().unwrap();
        let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
        assert_eq!(path.parent(), executable.parent());
        // No worker sits beside the test binary, whatever PATH and the
        // working directory hold.
        assert_eq!(worker.inspect(path), Err(UnavailableCause::WorkerMissing));
    }

    #[cfg(unix)]
    #[test]
    fn worker_on_path_is_ignored() {
        resolve_with_decoy("media_platform::tests::worker_on_path_is_ignored", true);
    }

    #[cfg(unix)]
    #[test]
    fn worker_in_cwd_is_ignored() {
        resolve_with_decoy("media_platform::tests::worker_in_cwd_is_ignored", false);
    }

    #[cfg(unix)]
    mod files {
        use super::*;
        use std::os::unix::fs::PermissionsExt as _;

        fn install() -> (tempfile::TempDir, PathBuf) {
            let directory = tempfile::tempdir().unwrap();
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            let worker = directory.path().join(WORKER_NAME);
            std::fs::write(&worker, "worker").unwrap();
            std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();
            (directory, worker)
        }

        fn chmod(path: &Path, mode: u32) {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }

        #[test]
        fn worker_missing_is_typed() {
            let (_install, worker) = install();
            std::fs::remove_file(&worker).unwrap();
            assert_eq!(inspect(&worker), Err(UnavailableCause::WorkerMissing));
        }

        #[test]
        fn an_installed_worker_reports_its_identity() {
            use std::os::unix::fs::MetadataExt as _;
            let (_install, worker) = install();
            let identity = inspect(&worker).unwrap();
            let metadata = std::fs::metadata(&worker).unwrap();
            assert_eq!((identity.ino, identity.size), (metadata.ino(), 6));
        }

        #[test]
        fn a_rewritten_worker_has_a_new_identity() {
            let (_install, worker) = install();
            let before = inspect(&worker).unwrap();
            // Same size, inode and modification time: only the status time
            // shows the rewrite.
            let modified = std::fs::metadata(&worker).unwrap().modified().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
            std::fs::write(&worker, "WORKER").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&worker)
                .unwrap()
                .set_modified(modified)
                .unwrap();
            let after = inspect(&worker).unwrap();
            assert_eq!((after.ino, after.size), (before.ino, before.size));
            assert_eq!(
                (after.mtime_seconds, after.mtime_nanos),
                (before.mtime_seconds, before.mtime_nanos)
            );
            assert_ne!(after, before);
        }

        #[test]
        fn unsafe_worker_files_are_refused() {
            /// Make the install at the first path, with its worker at the
            /// second, unsafe.
            type Breakage = fn(&Path, &Path);
            let cases: [(&str, Breakage); 7] = [
                ("not executable", |_, worker| chmod(worker, 0o644)),
                ("group-writable", |_, worker| chmod(worker, 0o775)),
                ("world-writable", |_, worker| chmod(worker, 0o757)),
                ("set-user-id", |_, worker| chmod(worker, 0o4755)),
                ("directory writable by others", |directory, _| {
                    chmod(directory, 0o775)
                }),
                ("a directory", |_, worker| {
                    std::fs::remove_file(worker).unwrap();
                    std::fs::create_dir(worker).unwrap();
                    chmod(worker, 0o755);
                }),
                ("a link to a worker", |directory, worker| {
                    let real = directory.join("real-worker");
                    std::fs::rename(worker, &real).unwrap();
                    std::os::unix::fs::symlink(&real, worker).unwrap();
                }),
            ];
            for (name, make_unsafe) in cases {
                let (install, worker) = install();
                assert!(inspect(&worker).is_ok(), "{name}: control");
                make_unsafe(install.path(), &worker);
                assert_eq!(
                    inspect(&worker),
                    Err(UnavailableCause::UnsafeWorkerFile),
                    "{name}"
                );
                chmod(install.path(), 0o755);
            }
        }
    }

    #[cfg(target_os = "macos")]
    mod macos {
        use super::super::signature::{WORKER_IDENTIFIER, hardened_runtime, run_bounded, verify};
        use super::*;
        use std::process::Command;

        fn run(program: &str, args: &[&str], path: &Path) -> String {
            let output = Command::new(program).args(args).arg(path).output().unwrap();
            assert!(
                output.status.success(),
                "{program} {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stderr).into_owned()
        }

        /// A small native program to sign: a copy of `/usr/bin/true`.
        fn program(directory: &Path) -> PathBuf {
            let path = directory.join(WORKER_NAME);
            std::fs::copy("/usr/bin/true", &path).unwrap();
            path
        }

        /// Sign `path` ad hoc as the worker, as a local build would be.
        fn sign_ad_hoc(path: &Path, runtime: bool) {
            let mut args = vec!["--force", "--sign", "-", "--identifier", WORKER_IDENTIFIER];
            if runtime {
                args.extend(["--options", "runtime"]);
            }
            args.push("--");
            run("/usr/bin/codesign", &args, path);
        }

        /// A requirement only this exact signed file meets, standing in for
        /// the Developer ID signature CI cannot make. `codesign` checks every
        /// architecture in a universal file, so it names each one's cdhash.
        fn pinned_requirement(path: &Path) -> String {
            let cdhashes: Vec<String> = ["x86_64", "arm64", "arm64e"]
                .into_iter()
                .filter_map(|arch| {
                    let output = Command::new("/usr/bin/codesign")
                        .args(["--display", "-vvv", "--arch", arch, "--"])
                        .arg(path)
                        .output()
                        .unwrap();
                    let described = String::from_utf8_lossy(&output.stderr).into_owned();
                    let cdhash = described
                        .lines()
                        .find_map(|line| line.strip_prefix("CDHash="))?;
                    output
                        .status
                        .success()
                        .then(|| format!("cdhash H\"{cdhash}\""))
                })
                .collect();
            assert!(!cdhashes.is_empty(), "codesign reports no cdhash");
            format!(
                "identifier \"{WORKER_IDENTIFIER}\" and ({})",
                cdhashes.join(" or ")
            )
        }

        fn official(path: &Path) -> Result<(), UnavailableCause> {
            let worker = InstalledWorker::beside(Ok(path.to_path_buf()));
            worker.verify(path)
        }

        #[test]
        fn the_official_requirement_compiles() {
            let output = Command::new("/usr/bin/csreq")
                .arg("-r")
                .arg(format!("={}", signature::official_requirement()))
                .arg("-t")
                .output()
                .unwrap();
            assert!(output.status.success(), "csreq refused the requirement");
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(text.contains(WORKER_IDENTIFIER), "{text}");
            assert!(
                text.contains(kettle_update::APPLE_TEAM_IDENTIFIER),
                "{text}"
            );
            assert!(text.contains("anchor apple generic"), "{text}");
        }

        #[test]
        fn a_pinned_ad_hoc_worker_verifies() {
            // The control for the refusals below: the same checks pass a
            // signature that meets the requirement they are given.
            let directory = tempfile::tempdir().unwrap();
            let worker = program(directory.path());
            sign_ad_hoc(&worker, true);
            assert_eq!(verify(&worker, &pinned_requirement(&worker)), Ok(()));
        }

        #[test]
        fn macos_adhoc_worker_fails_official_requirement() {
            let directory = tempfile::tempdir().unwrap();
            let worker = program(directory.path());
            sign_ad_hoc(&worker, true);
            assert_eq!(official(&worker), Err(UnavailableCause::Unverified));
        }

        #[test]
        fn an_apple_program_fails_official_requirement() {
            let directory = tempfile::tempdir().unwrap();
            let worker = program(directory.path());
            assert_eq!(official(&worker), Err(UnavailableCause::Unverified));
        }

        #[test]
        fn macos_unsigned_worker_is_unverified() {
            let directory = tempfile::tempdir().unwrap();
            let worker = program(directory.path());
            sign_ad_hoc(&worker, true);
            let requirement = pinned_requirement(&worker);
            run("/usr/bin/codesign", &["--remove-signature", "--"], &worker);
            assert_eq!(official(&worker), Err(UnavailableCause::Unverified));
            assert_eq!(
                verify(&worker, &requirement),
                Err(UnavailableCause::Unverified)
            );
        }

        #[test]
        fn macos_tampered_worker_is_unverified() {
            let directory = tempfile::tempdir().unwrap();
            let worker = program(directory.path());
            sign_ad_hoc(&worker, true);
            let requirement = pinned_requirement(&worker);
            assert_eq!(verify(&worker, &requirement), Ok(()));
            // Flip one byte in the first architecture's first page, past its
            // header: always signed code. (The end of an architecture holds
            // the signature itself, with unsigned room to spare, and padding
            // between architectures is not signed either.) The first
            // architecture's offset is in the big-endian universal header.
            let mut bytes = std::fs::read(&worker).unwrap();
            let offset = if bytes[..4] == [0xca, 0xfe, 0xba, 0xbe] {
                usize::try_from(u32::from_be_bytes(bytes[16..20].try_into().unwrap())).unwrap()
            } else {
                0
            };
            bytes[offset + 1024] ^= 0xff;
            std::fs::write(&worker, bytes).unwrap();
            assert_eq!(
                verify(&worker, &requirement),
                Err(UnavailableCause::Unverified)
            );
        }

        #[test]
        fn a_worker_without_the_hardened_runtime_is_unverified() {
            let directory = tempfile::tempdir().unwrap();
            let worker = program(directory.path());
            sign_ad_hoc(&worker, false);
            assert_eq!(
                verify(&worker, &pinned_requirement(&worker)),
                Err(UnavailableCause::Unverified)
            );
        }

        #[test]
        fn a_stuck_check_is_stopped_at_its_deadline() {
            let mut sleep = Command::new("/bin/sleep");
            sleep.arg("30");
            let started = std::time::Instant::now();
            assert_eq!(
                run_bounded(sleep, std::time::Duration::from_millis(100)),
                Err(UnavailableCause::CheckFailed)
            );
            assert!(started.elapsed() < std::time::Duration::from_secs(10));
        }

        #[test]
        fn a_finished_check_reports_its_status_and_stderr() {
            let mut report = Command::new("/bin/sh");
            report.args(["-c", "echo report >&2; exit 3"]);
            assert_eq!(
                run_bounded(report, std::time::Duration::from_secs(10)),
                Ok((false, b"report\n".to_vec()))
            );
            let mut quiet = Command::new("/bin/sh");
            quiet.args(["-c", "exit 0"]);
            assert_eq!(
                run_bounded(quiet, std::time::Duration::from_secs(10)),
                Ok((true, Vec::new()))
            );
        }

        #[test]
        fn hardened_runtime_reads_the_code_directory_flags() {
            for (described, runtime) in [
                (
                    "Executable=/x\nCodeDirectory v=20500 size=1 flags=0x10000(runtime) hashes=1\n",
                    true,
                ),
                (
                    "CodeDirectory v=20500 size=1 flags=0x10002(adhoc,runtime) hashes=1\n",
                    true,
                ),
                (
                    "CodeDirectory v=20400 size=1 flags=0x2(adhoc) hashes=1\n",
                    false,
                ),
                (
                    "CodeDirectory v=20400 size=1 flags=0x0(none) hashes=1\n",
                    false,
                ),
                // Only the CodeDirectory line counts.
                (
                    "Identifier=flags=0x10000\nCodeDirectory v=1 flags=0x0(none)\n",
                    false,
                ),
                ("", false),
            ] {
                assert_eq!(hardened_runtime(described), runtime, "{described}");
            }
        }
    }
}
