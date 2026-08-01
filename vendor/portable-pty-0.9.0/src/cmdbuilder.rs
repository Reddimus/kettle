#[cfg(unix)]
use anyhow::Context;
#[cfg(feature = "serde_support")]
use serde_derive::*;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(unix)]
use std::path::Component;
use std::path::Path;

/// Used to deal with Windows having case-insensitive environment variables.
#[derive(Clone, Debug, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde_support", derive(Serialize, Deserialize))]
struct EnvEntry {
    /// Whether or not this environment variable came from the base environment,
    /// as opposed to having been explicitly set by the caller.
    is_from_base_env: bool,

    /// For case-insensitive platforms, the environment variable key in its preferred casing.
    preferred_key: OsString,

    /// The environment variable value.
    value: OsString,
}

impl EnvEntry {
    fn map_key(k: OsString) -> OsString {
        if cfg!(windows) {
            // Best-effort lowercase transformation of an os string
            match k.to_str() {
                Some(s) => s.to_lowercase().into(),
                None => k,
            }
        } else {
            k
        }
    }
}

/// The current user's login shell and home directory, as recorded in the
/// passwd database.
///
/// Either field may be absent: a passwd entry is not obliged to supply one, and
/// the library is free to hand back a NULL pointer for it.
#[cfg(unix)]
#[derive(Default)]
struct PasswdEntry {
    shell: Option<OsString>,
    home: Option<OsString>,
}

/// Read the current user's passwd entry through the REENTRANT interface.
///
/// Two defects motivated this. `getpwuid` returns a pointer into a buffer
/// shared by the whole process, so two threads opening panes at the same moment
/// race — the second call can overwrite the entry while the first is still
/// reading through it, and nothing about the resulting shell or home path is
/// then trustworthy. And both fields were dereferenced with `CStr::from_ptr`
/// with no NULL check, which is a segfault rather than a missing value.
///
/// `getpwuid_r` writes into a caller-owned buffer, which removes the race; the
/// fields are checked before they are read. Values come back as `OsString`
/// because a path from the passwd database is not obliged to be UTF-8, and
/// deciding what to do about that belongs to the caller.
#[cfg(unix)]
fn current_passwd_entry() -> PasswdEntry {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;

    // `_SC_GETPW_R_SIZE_MAX` is advisory and may be reported as -1 ("no
    // definite limit"), so treat any non-positive answer as unknown and grow on
    // ERANGE instead. The ceiling stops a misbehaving libc from making this
    // loop allocate without bound.
    const MAX_CAPACITY: usize = 64 * 1024;
    let mut capacity = match unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) } {
        reported if reported > 0 => (reported as usize).min(MAX_CAPACITY),
        _ => 1024,
    };
    loop {
        let mut buf = vec![0 as libc::c_char; capacity];
        let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `passwd` and `buf` are live and owned here, and their sizes
        // are passed accurately. `getpwuid_r` writes only within them.
        let rc = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                &mut passwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut found,
            )
        };
        if rc == libc::ERANGE && capacity < MAX_CAPACITY {
            capacity = (capacity * 2).min(MAX_CAPACITY);
            continue;
        }
        // A missing user is reported as success with a null result, so both
        // have to be checked before anything in `passwd` may be read.
        if rc != 0 || found.is_null() {
            return PasswdEntry::default();
        }
        // SAFETY: `getpwuid_r` reported success, so the string fields either
        // point into `buf` — which outlives these reads — or are null, which is
        // checked.
        let field = |ptr: *const libc::c_char| -> Option<OsString> {
            if ptr.is_null() {
                return None;
            }
            Some(OsStr::from_bytes(unsafe { CStr::from_ptr(ptr) }.to_bytes()).to_os_string())
        };
        return PasswdEntry {
            shell: field(passwd.pw_shell),
            home: field(passwd.pw_dir),
        };
    }
}

#[cfg(unix)]
fn get_shell() -> String {
    use nix::unistd::{access, AccessFlags};

    // POSIX gives an empty `pw_shell` the meaning "the implementation's default
    // shell", which is what the fallback below already is.
    if let Some(shell) = current_passwd_entry().shell.filter(|shell| !shell.is_empty()) {
        match shell.into_string() {
            Err(_) => {
                log::warn!(
                    "passwd database shell could not be \
                     represented as utf-8, \
                     falling back to /bin/sh"
                );
            }
            Ok(shell) => {
                if let Err(err) = access(Path::new(&shell), AccessFlags::X_OK) {
                    log::warn!(
                        "passwd database shell={shell:?} which is \
                         not executable ({err:#}), falling back to /bin/sh"
                    );
                } else {
                    return shell;
                }
            }
        }
    }
    "/bin/sh".into()
}

fn get_base_env() -> BTreeMap<OsString, EnvEntry> {
    let mut env: BTreeMap<OsString, EnvEntry> = std::env::vars_os()
        .map(|(key, value)| {
            (
                EnvEntry::map_key(key.clone()),
                EnvEntry {
                    is_from_base_env: true,
                    preferred_key: key,
                    value,
                },
            )
        })
        .collect();

    #[cfg(unix)]
    {
        let key = EnvEntry::map_key("SHELL".into());
        // Only set the value of SHELL if it isn't already set
        if !env.contains_key(&key) {
            env.insert(
                EnvEntry::map_key("SHELL".into()),
                EnvEntry {
                    is_from_base_env: true,
                    preferred_key: "SHELL".into(),
                    value: get_shell().into(),
                },
            );
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        use winapi::um::processenv::ExpandEnvironmentStringsW;
        use winreg::enums::{RegType, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        use winreg::types::FromRegValue;
        use winreg::{RegKey, RegValue};

        fn reg_value_to_string(value: &RegValue) -> anyhow::Result<OsString> {
            match value.vtype {
                RegType::REG_EXPAND_SZ => {
                    // `value.bytes` is a `Vec<u8>`, so its allocation is only
                    // guaranteed to be 1-byte aligned. Reinterpreting that
                    // pointer as `*const u16` and handing it to
                    // `slice::from_raw_parts` is undefined behaviour — the
                    // function requires the pointer be aligned for its element
                    // type. It happened to work because allocators usually
                    // return generously aligned blocks, which is exactly the
                    // kind of accident that holds until it does not.
                    //
                    // Decoding pairs explicitly is both defined and no slower
                    // in any way that matters for an environment block.
                    let mut wide: Vec<u16> = value
                        .bytes
                        .chunks_exact(2)
                        .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
                        .collect();
                    // `ExpandEnvironmentStringsW` reads its input until a NUL.
                    // The old code passed whatever the registry held, so a
                    // value that was not NUL-terminated — or whose length was
                    // odd, since the trailing byte was silently dropped — was
                    // read past its end. Terminate it ourselves.
                    while wide.last() == Some(&0) {
                        wide.pop();
                    }
                    let unexpanded = OsString::from_wide(&wide);
                    wide.push(0);

                    // The returned size counts the terminating NUL. Zero means
                    // failure, which the old code turned into a one-element
                    // buffer and then an empty string — silently replacing the
                    // variable's value with nothing. Preferring the unexpanded
                    // text keeps a usable value in that case.
                    let needed =
                        unsafe { ExpandEnvironmentStringsW(wide.as_ptr(), std::ptr::null_mut(), 0) };
                    if needed == 0 {
                        return Ok(unexpanded);
                    }
                    let mut buf = vec![0u16; needed as usize];
                    let written = unsafe {
                        ExpandEnvironmentStringsW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32)
                    };
                    // A second call that fails, or that wants more room than
                    // the first call asked for (the environment can change in
                    // between), leaves `buf` holding nothing trustworthy.
                    if written == 0 || written as usize > buf.len() {
                        return Ok(unexpanded);
                    }
                    buf.truncate(written as usize);
                    while buf.last() == Some(&0) {
                        buf.pop();
                    }
                    Ok(OsString::from_wide(&buf))
                }
                _ => Ok(OsString::from_reg_value(value)?),
            }
        }

        if let Ok(sys_env) = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey("System\\CurrentControlSet\\Control\\Session Manager\\Environment")
        {
            for (name, value) in sys_env.enum_values().flatten() {
                if name.eq_ignore_ascii_case("username") {
                    continue;
                }
                if let Ok(value) = reg_value_to_string(&value) {
                    log::trace!("adding SYS env: {:?} {:?}", name, value);
                    env.insert(
                        EnvEntry::map_key(name.clone().into()),
                        EnvEntry {
                            is_from_base_env: true,
                            preferred_key: name.into(),
                            value,
                        },
                    );
                }
            }
        }

        if let Ok(sys_env) = RegKey::predef(HKEY_CURRENT_USER).open_subkey("Environment") {
            for (name, value) in sys_env.enum_values().flatten() {
                if let Ok(value) = reg_value_to_string(&value) {
                    // Merge the system and user paths together
                    let value = if name.eq_ignore_ascii_case("path") {
                        match env.get(&EnvEntry::map_key(name.clone().into())) {
                            Some(entry) => {
                                let mut result = OsString::new();
                                result.push(&entry.value);
                                result.push(";");
                                result.push(&value);
                                result
                            }
                            None => value,
                        }
                    } else {
                        value
                    };

                    log::trace!("adding USER env: {:?} {:?}", name, value);
                    env.insert(
                        EnvEntry::map_key(name.clone().into()),
                        EnvEntry {
                            is_from_base_env: true,
                            preferred_key: name.into(),
                            value,
                        },
                    );
                }
            }
        }
    }

    env
}

/// `CommandBuilder` is used to prepare a command to be spawned into a pty.
/// The interface is intentionally similar to that of `std::process::Command`.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde_support", derive(Serialize, Deserialize))]
pub struct CommandBuilder {
    args: Vec<OsString>,
    envs: BTreeMap<OsString, EnvEntry>,
    cwd: Option<OsString>,
    #[cfg(unix)]
    pub(crate) umask: Option<libc::mode_t>,
    controlling_tty: bool,
}

impl CommandBuilder {
    /// Create a new builder instance with argv\[0\] set to the specified
    /// program.
    pub fn new<S: AsRef<OsStr>>(program: S) -> Self {
        Self {
            args: vec![program.as_ref().to_owned()],
            envs: get_base_env(),
            cwd: None,
            #[cfg(unix)]
            umask: None,
            controlling_tty: true,
        }
    }

    /// Create a new builder instance from a pre-built argument vector
    pub fn from_argv(args: Vec<OsString>) -> Self {
        Self {
            args,
            envs: get_base_env(),
            cwd: None,
            #[cfg(unix)]
            umask: None,
            controlling_tty: true,
        }
    }

    /// Set whether we should set the pty as the controlling terminal.
    /// The default is true, which is usually what you want, but you
    /// may need to set this to false if you are crossing container
    /// boundaries (eg: flatpak) to workaround issues like:
    /// <https://github.com/flatpak/flatpak/issues/3697>
    /// <https://github.com/flatpak/flatpak/issues/3285>
    pub fn set_controlling_tty(&mut self, controlling_tty: bool) {
        self.controlling_tty = controlling_tty;
    }

    pub fn get_controlling_tty(&self) -> bool {
        self.controlling_tty
    }

    /// Create a new builder instance that will run some idea of a default
    /// program.  Such a builder will panic if `arg` is called on it.
    pub fn new_default_prog() -> Self {
        Self {
            args: vec![],
            envs: get_base_env(),
            cwd: None,
            #[cfg(unix)]
            umask: None,
            controlling_tty: true,
        }
    }

    /// Returns true if this builder was created via `new_default_prog`
    pub fn is_default_prog(&self) -> bool {
        self.args.is_empty()
    }

    /// Append an argument to the current command line.
    /// Will panic if called on a builder created via `new_default_prog`.
    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) {
        if self.is_default_prog() {
            panic!("attempted to add args to a default_prog builder");
        }
        self.args.push(arg.as_ref().to_owned());
    }

    /// Append a sequence of arguments to the current command line
    pub fn args<I, S>(&mut self, args: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for arg in args {
            self.arg(arg);
        }
    }

    pub fn get_argv(&self) -> &Vec<OsString> {
        &self.args
    }

    pub fn get_argv_mut(&mut self) -> &mut Vec<OsString> {
        &mut self.args
    }

    /// Override the value of an environmental variable
    pub fn env<K, V>(&mut self, key: K, value: V)
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let key: OsString = key.as_ref().into();
        let value: OsString = value.as_ref().into();
        self.envs.insert(
            EnvEntry::map_key(key.clone()),
            EnvEntry {
                is_from_base_env: false,
                preferred_key: key,
                value,
            },
        );
    }

    pub fn env_remove<K>(&mut self, key: K)
    where
        K: AsRef<OsStr>,
    {
        let key = key.as_ref().into();
        self.envs.remove(&EnvEntry::map_key(key));
    }

    pub fn env_clear(&mut self) {
        self.envs.clear();
    }

    pub fn get_env<K>(&self, key: K) -> Option<&OsStr>
    where
        K: AsRef<OsStr>,
    {
        let key = key.as_ref().into();
        self.envs.get(&EnvEntry::map_key(key)).map(
            |EnvEntry {
                 is_from_base_env: _,
                 preferred_key: _,
                 value,
             }| value.as_os_str(),
        )
    }

    pub fn cwd<D>(&mut self, dir: D)
    where
        D: AsRef<OsStr>,
    {
        self.cwd = Some(dir.as_ref().to_owned());
    }

    pub fn clear_cwd(&mut self) {
        self.cwd.take();
    }

    pub fn get_cwd(&self) -> Option<&OsString> {
        self.cwd.as_ref()
    }

    /// Iterate over the configured environment. Only includes environment
    /// variables set by the caller via `env`, not variables set in the base
    /// environment.
    pub fn iter_extra_env_as_str(&self) -> impl Iterator<Item = (&str, &str)> {
        self.envs.values().filter_map(
            |EnvEntry {
                 is_from_base_env,
                 preferred_key,
                 value,
             }| {
                if *is_from_base_env {
                    None
                } else {
                    let key = preferred_key.to_str()?;
                    let value = value.to_str()?;
                    Some((key, value))
                }
            },
        )
    }

    pub fn iter_full_env_as_str(&self) -> impl Iterator<Item = (&str, &str)> {
        self.envs.values().filter_map(
            |EnvEntry {
                 preferred_key,
                 value,
                 ..
             }| {
                let key = preferred_key.to_str()?;
                let value = value.to_str()?;
                Some((key, value))
            },
        )
    }

    /// Return the configured command and arguments as a single string,
    /// quoted per the unix shell conventions.
    pub fn as_unix_command_line(&self) -> anyhow::Result<String> {
        let mut strs = vec![];
        for arg in &self.args {
            let s = arg
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("argument cannot be represented as utf8"))?;
            strs.push(s);
        }
        Ok(shell_words::join(strs))
    }
}

#[cfg(unix)]
impl CommandBuilder {
    pub fn umask(&mut self, mask: Option<libc::mode_t>) {
        self.umask = mask;
    }

    fn resolve_path(&self) -> Option<&OsStr> {
        self.get_env("PATH")
    }

    fn search_path(&self, exe: &OsStr, cwd: &OsStr) -> anyhow::Result<OsString> {
        use nix::unistd::{access, AccessFlags};

        let exe_path: &Path = exe.as_ref();
        if exe_path.is_relative() {
            let cwd: &Path = cwd.as_ref();
            let mut errors = vec![];

            // If the requested executable is explicitly relative to cwd,
            // then check only there.
            if is_cwd_relative_path(exe_path) {
                let abs_path = cwd.join(exe_path);

                if abs_path.is_dir() {
                    anyhow::bail!(
                        "Unable to spawn {} because it is a directory",
                        abs_path.display()
                    );
                } else if access(&abs_path, AccessFlags::X_OK).is_ok() {
                    return Ok(abs_path.into_os_string());
                } else if access(&abs_path, AccessFlags::F_OK).is_ok() {
                    anyhow::bail!(
                        "Unable to spawn {} because it is not executable",
                        abs_path.display()
                    );
                }

                anyhow::bail!(
                    "Unable to spawn {} because it does not exist",
                    abs_path.display()
                );
            }

            if let Some(path) = self.resolve_path() {
                for path in std::env::split_paths(&path) {
                    let candidate = cwd.join(&path).join(exe);

                    if candidate.is_dir() {
                        errors.push(format!("{} exists but is a directory", candidate.display()));
                    } else if access(&candidate, AccessFlags::X_OK).is_ok() {
                        return Ok(candidate.into_os_string());
                    } else if access(&candidate, AccessFlags::F_OK).is_ok() {
                        errors.push(format!(
                            "{} exists but is not executable",
                            candidate.display()
                        ));
                    }
                }
                errors.push(format!("No viable candidates found in PATH {path:?}"));
            } else {
                errors.push("Unable to resolve the PATH".to_string());
            }
            anyhow::bail!(
                "Unable to spawn {} because:\n{}",
                exe_path.display(),
                errors.join(".\n")
            );
        } else if exe_path.is_dir() {
            anyhow::bail!(
                "Unable to spawn {} because it is a directory",
                exe_path.display()
            );
        } else {
            if let Err(err) = access(exe_path, AccessFlags::X_OK) {
                if access(exe_path, AccessFlags::F_OK).is_ok() {
                    anyhow::bail!(
                        "Unable to spawn {} because it is not executable ({err:#})",
                        exe_path.display()
                    );
                } else {
                    anyhow::bail!(
                        "Unable to spawn {} because it doesn't exist on the filesystem ({err:#})",
                        exe_path.display()
                    );
                }
            }

            Ok(exe.to_owned())
        }
    }

    /// Convert the CommandBuilder to a `std::process::Command` instance.
    pub(crate) fn as_command(&self) -> anyhow::Result<std::process::Command> {
        use std::os::unix::process::CommandExt;

        let home = self.get_home_dir()?;
        let dir: &OsStr = self
            .cwd
            .as_deref()
            .filter(|dir| std::path::Path::new(dir).is_dir())
            .unwrap_or(home.as_ref());
        let shell = self.get_shell();

        let mut cmd = if self.is_default_prog() {
            let mut cmd = std::process::Command::new(&shell);

            // Run the shell as a login shell by prefixing the shell's
            // basename with `-` and setting that as argv0
            let basename = shell.rsplit('/').next().unwrap_or(&shell);
            cmd.arg0(format!("-{}", basename));
            cmd
        } else {
            let resolved = self.search_path(&self.args[0], dir)?;
            let mut cmd = std::process::Command::new(&resolved);
            cmd.arg0(&self.args[0]);
            cmd.args(&self.args[1..]);
            cmd
        };

        cmd.current_dir(dir);

        cmd.env_clear();
        cmd.env("SHELL", shell);
        cmd.envs(self.envs.values().map(
            |EnvEntry {
                 is_from_base_env: _,
                 preferred_key,
                 value,
             }| (preferred_key.as_os_str(), value.as_os_str()),
        ));

        Ok(cmd)
    }

    /// Determine which shell to run.
    /// We take the contents of the $SHELL env var first, then
    /// fall back to looking it up from the password database.
    pub fn get_shell(&self) -> String {
        use nix::unistd::{access, AccessFlags};

        if let Some(shell) = self.get_env("SHELL").and_then(OsStr::to_str) {
            match access(shell, AccessFlags::X_OK) {
                Ok(()) => return shell.into(),
                Err(err) => log::warn!(
                    "$SHELL -> {shell:?} which is \
                     not executable ({err:#}), falling back to password db lookup"
                ),
            }
        }

        get_shell()
    }

    fn get_home_dir(&self) -> anyhow::Result<String> {
        if let Some(home_dir) = self.get_env("HOME").and_then(OsStr::to_str) {
            return Ok(home_dir.into());
        }

        // Same reentrancy and NULL-check reasoning as `current_passwd_entry`
        // documents; an absent entry keeps the previous `/` fallback.
        match current_passwd_entry().home {
            None => Ok("/".into()),
            Some(home) => home
                .into_string()
                .map_err(|_| anyhow::anyhow!("home dir is not valid utf-8"))
                .context("failed to resolve home dir"),
        }
    }
}

/// Build `<dir>/<exe><ext>` for one PATHEXT entry, or `None` when the entry is
/// unusable.
///
/// Three separate defects lived in the two lines this replaces, and every one
/// of them is reachable from an ordinary Windows environment:
///
///   * `ext.to_str().expect("PATHEXT entries must be utf8")` PANICKED on an
///     entry that was not UTF-8. `PATHEXT` is environment data; a terminal must
///     not abort because of what it contains.
///   * `&ext[1..]` panicked on an EMPTY entry, and `PATHEXT` ending in `;`
///     produces exactly that — a trailing separator is common, because
///     installers append entries without checking whether one is already there.
///     The same slice assumed the leading `.` occupied one byte, so an entry
///     beginning with a multi-byte character panicked on a char boundary.
///   * `with_extension` REPLACES an existing extension instead of appending, so
///     resolving `foo.bar` searched for `foo.EXE`. Windows appends — `cmd` and
///     `CreateProcess` look for `foo.bar.EXE` — so this could resolve a request
///     for one program to a DIFFERENT program that happened to share its stem.
///
/// Appending on the `OsStr` sidesteps the encoding question altogether: the
/// bytes are never required to be UTF-8 and never indexed.
#[cfg(windows)]
fn with_pathext(dir: &Path, exe: &OsStr, ext: &OsStr) -> Option<std::path::PathBuf> {
    let bytes = ext.as_encoded_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut name = exe.to_os_string();
    // PATHEXT entries carry their own leading `.`; supply one if this entry
    // omits it rather than silently producing `dirfoobar`.
    if bytes[0] != b'.' {
        name.push(".");
    }
    name.push(ext);
    Some(dir.join(name))
}

#[cfg(windows)]
impl CommandBuilder {
    fn search_path(&self, exe: &OsStr) -> OsString {
        if let Some(path) = self.get_env("PATH") {
            let extensions = self.get_env("PATHEXT").unwrap_or(OsStr::new(".EXE"));
            for path in std::env::split_paths(&path) {
                // Check for exactly the user's string in this path dir
                let candidate = path.join(exe);
                if candidate.exists() {
                    return candidate.into_os_string();
                }

                // Otherwise try each PATHEXT extension in turn.
                for ext in std::env::split_paths(&extensions) {
                    let Some(candidate) = with_pathext(&path, exe, ext.as_os_str()) else {
                        continue;
                    };
                    if candidate.exists() {
                        return candidate.into_os_string();
                    }
                }
            }
        }

        exe.to_owned()
    }

    pub(crate) fn current_directory(&self) -> Option<Vec<u16>> {
        let home: Option<&OsStr> = self
            .get_env("USERPROFILE")
            .filter(|path| Path::new(path).is_dir());
        let cwd: Option<&OsStr> = self.cwd.as_deref().filter(|path| Path::new(path).is_dir());
        let dir: Option<&OsStr> = cwd.or(home);

        dir.map(|dir| {
            let mut wide = vec![];

            if Path::new(dir).is_relative() {
                if let Ok(ccwd) = std::env::current_dir() {
                    wide.extend(ccwd.join(dir).as_os_str().encode_wide());
                } else {
                    wide.extend(dir.encode_wide());
                }
            } else {
                wide.extend(dir.encode_wide());
            }

            wide.push(0);
            wide
        })
    }

    /// Constructs an environment block for this spawn attempt.
    /// Uses the current process environment as the base and then
    /// adds/replaces the environment that was specified via the
    /// `env` methods.
    pub(crate) fn environment_block(&self) -> Vec<u16> {
        // encode the environment as wide characters
        let mut block = vec![];

        for EnvEntry {
            is_from_base_env: _,
            preferred_key,
            value,
        } in self.envs.values()
        {
            block.extend(preferred_key.encode_wide());
            block.push(b'=' as u16);
            block.extend(value.encode_wide());
            block.push(0);
        }
        // and a final terminator for CreateProcessW
        block.push(0);

        block
    }

    pub fn get_shell(&self) -> String {
        let exe: OsString = self
            .get_env("ComSpec")
            .unwrap_or(OsStr::new("cmd.exe"))
            .into();
        exe.into_string()
            .unwrap_or_else(|_| "%CompSpec%".to_string())
    }

    pub(crate) fn cmdline(&self) -> anyhow::Result<(Vec<u16>, Vec<u16>)> {
        let mut cmdline = Vec::<u16>::new();

        let exe: OsString = if self.is_default_prog() {
            self.get_env("ComSpec")
                .unwrap_or(OsStr::new("cmd.exe"))
                .into()
        } else {
            self.search_path(&self.args[0])
        };

        Self::append_quoted(&exe, &mut cmdline);

        // Ensure that we nul terminate the module name, otherwise we'll
        // ask CreateProcessW to start something random!
        let mut exe: Vec<u16> = exe.encode_wide().collect();
        exe.push(0);

        for arg in self.args.iter().skip(1) {
            cmdline.push(' ' as u16);
            anyhow::ensure!(
                !arg.encode_wide().any(|c| c == 0),
                "invalid encoding for command line argument {:?}",
                arg
            );
            Self::append_quoted(arg, &mut cmdline);
        }
        // Ensure that the command line is nul terminated too!
        cmdline.push(0);
        Ok((exe, cmdline))
    }

    // Borrowed from https://github.com/hniksic/rust-subprocess/blob/873dfed165173e52907beb87118b2c0c05d8b8a1/src/popen.rs#L1117
    // which in turn was translated from ArgvQuote at http://tinyurl.com/zmgtnls
    fn append_quoted(arg: &OsStr, cmdline: &mut Vec<u16>) {
        if !arg.is_empty()
            && !arg.encode_wide().any(|c| {
                c == ' ' as u16
                    || c == '\t' as u16
                    || c == '\n' as u16
                    || c == '\x0b' as u16
                    || c == '\"' as u16
            })
        {
            cmdline.extend(arg.encode_wide());
            return;
        }
        cmdline.push('"' as u16);

        let arg: Vec<_> = arg.encode_wide().collect();
        let mut i = 0;
        while i < arg.len() {
            let mut num_backslashes = 0;
            while i < arg.len() && arg[i] == '\\' as u16 {
                i += 1;
                num_backslashes += 1;
            }

            if i == arg.len() {
                for _ in 0..num_backslashes * 2 {
                    cmdline.push('\\' as u16);
                }
                break;
            } else if arg[i] == b'"' as u16 {
                for _ in 0..num_backslashes * 2 + 1 {
                    cmdline.push('\\' as u16);
                }
                cmdline.push(arg[i]);
            } else {
                for _ in 0..num_backslashes {
                    cmdline.push('\\' as u16);
                }
                cmdline.push(arg[i]);
            }
            i += 1;
        }
        cmdline.push('"' as u16);
    }
}

#[cfg(unix)]
/// Returns true if the path begins with `./` or `../`
fn is_cwd_relative_path<P: AsRef<Path>>(p: P) -> bool {
    matches!(
        p.as_ref().components().next(),
        Some(Component::CurDir | Component::ParentDir)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `get_shell` must yield something actually runnable, whether it came from
    /// the passwd database or the `/bin/sh` fallback — a pane opened with an
    /// unrunnable shell is an empty pane.
    #[cfg(unix)]
    #[test]
    fn resolved_shell_is_executable() {
        use nix::unistd::{access, AccessFlags};

        let shell = get_shell();
        assert!(!shell.is_empty(), "a shell path is always produced");
        assert!(
            access(Path::new(&shell), AccessFlags::X_OK).is_ok(),
            // Explicit argument: this crate is edition 2018, where a
            // single-argument `assert!` message takes the legacy
            // `panic!(expr)` path and never captures `{shell:?}` implicitly.
            "resolved shell {:?} must be executable",
            shell
        );
    }

    /// The passwd lookup runs whenever a pane is spawned, and panes can be
    /// spawned concurrently. The previous `getpwuid` handed back a pointer into
    /// a process-wide buffer, so overlapping calls raced.
    ///
    /// A passing run is a stress check, not a proof — a data race need not
    /// manifest — but a torn or reused buffer shows up here as threads
    /// disagreeing, and this cannot pass by accident under the reentrant call.
    #[cfg(unix)]
    #[test]
    fn passwd_lookup_agrees_across_threads() {
        let expected = current_passwd_entry();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    (0..64)
                        .map(|_| {
                            let entry = current_passwd_entry();
                            (entry.shell, entry.home)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for thread in threads {
            for (shell, home) in thread.join().expect("passwd lookup must not panic") {
                assert_eq!(shell, expected.shell, "concurrent lookups must agree");
                assert_eq!(home, expected.home, "concurrent lookups must agree");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_cwd_relative() {
        assert!(is_cwd_relative_path("."));
        assert!(is_cwd_relative_path("./foo"));
        assert!(is_cwd_relative_path("../foo"));
        assert!(!is_cwd_relative_path("foo"));
        assert!(!is_cwd_relative_path("/foo"));
    }

    #[test]
    fn test_env() {
        let mut cmd = CommandBuilder::new("dummy");
        let package_authors = cmd.get_env("CARGO_PKG_AUTHORS");
        println!("package_authors: {:?}", package_authors);
        assert!(package_authors == Some(OsStr::new("Wez Furlong")));

        cmd.env("foo key", "foo value");
        cmd.env("bar key", "bar value");

        let iterated_envs = cmd.iter_extra_env_as_str().collect::<Vec<_>>();
        println!("iterated_envs: {:?}", iterated_envs);
        assert!(iterated_envs == vec![("bar key", "bar value"), ("foo key", "foo value")]);

        {
            let mut cmd = cmd.clone();
            cmd.env_remove("foo key");

            let iterated_envs = cmd.iter_extra_env_as_str().collect::<Vec<_>>();
            println!("iterated_envs: {:?}", iterated_envs);
            assert!(iterated_envs == vec![("bar key", "bar value")]);
        }

        {
            let mut cmd = cmd.clone();
            cmd.env_remove("bar key");

            let iterated_envs = cmd.iter_extra_env_as_str().collect::<Vec<_>>();
            println!("iterated_envs: {:?}", iterated_envs);
            assert!(iterated_envs == vec![("foo key", "foo value")]);
        }

        {
            let mut cmd = cmd.clone();
            cmd.env_clear();

            let iterated_envs = cmd.iter_extra_env_as_str().collect::<Vec<_>>();
            println!("iterated_envs: {:?}", iterated_envs);
            assert!(iterated_envs.is_empty());
        }
    }

    /// A scratch directory that removes itself, so these tests need no
    /// dev-dependency (adding one would churn the locked vendor workspace).
    #[cfg(windows)]
    struct ScratchDir(std::path::PathBuf);

    #[cfg(windows)]
    impl ScratchDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static SEQUENCE: AtomicU32 = AtomicU32::new(0);
            let unique = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "portable-pty-{tag}-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("create scratch directory");
            Self(path)
        }
    }

    #[cfg(windows)]
    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `PATHEXT` ending in `;` is ordinary — installers append entries without
    /// checking for a trailing separator — and `split_paths` yields an empty
    /// final entry for it. Slicing that entry panicked, aborting the terminal
    /// while it was trying to resolve a program to spawn.
    #[cfg(windows)]
    #[test]
    fn pathext_with_a_trailing_separator_does_not_panic() {
        let scratch = ScratchDir::new("trailing-sep");
        let mut cmd = CommandBuilder::new("no-such-program");
        cmd.env("PATH", &scratch.0);
        cmd.env("PATHEXT", ".COM;.EXE;");

        // Unresolvable, so the search runs to exhaustion and every extension
        // entry — including the empty one — is visited.
        assert_eq!(
            cmd.search_path(OsStr::new("no-such-program")),
            OsString::from("no-such-program")
        );
    }

    /// The same slice took byte index 1 on faith, so an entry whose first
    /// character is multi-byte panicked on a char boundary.
    #[cfg(windows)]
    #[test]
    fn pathext_entries_need_not_be_ascii() {
        let scratch = ScratchDir::new("nonascii-ext");
        let mut cmd = CommandBuilder::new("no-such-program");
        cmd.env("PATH", &scratch.0);
        cmd.env("PATHEXT", "·EXE;.EXE");

        assert_eq!(
            cmd.search_path(OsStr::new("no-such-program")),
            OsString::from("no-such-program")
        );
    }

    /// `to_str().expect(...)` required UTF-8, which a Windows environment
    /// variable is never obliged to be. Kept separate from the char-boundary
    /// case above: whichever entry comes first is the only panic a single test
    /// can observe, so one test cannot prove both.
    #[cfg(windows)]
    #[test]
    fn pathext_entries_need_not_be_utf8() {
        use std::os::windows::ffi::OsStringExt;

        let scratch = ScratchDir::new("nonutf8-ext");
        // A lone surrogate: representable in a Windows environment variable,
        // not representable in UTF-8, so `to_str()` yields `None`.
        let mut extensions = OsString::from_wide(&[0xd800]);
        extensions.push(";.EXE");

        let mut cmd = CommandBuilder::new("no-such-program");
        cmd.env("PATH", &scratch.0);
        cmd.env("PATHEXT", &extensions);

        assert_eq!(
            cmd.search_path(OsStr::new("no-such-program")),
            OsString::from("no-such-program")
        );
    }

    /// `with_extension` REPLACED the extension, so resolving `foo.bar` searched
    /// for `foo.EXE` — a different program that merely shares the stem. Windows
    /// appends, and so must this.
    #[cfg(windows)]
    #[test]
    fn pathext_is_appended_rather_than_substituted() {
        let scratch = ScratchDir::new("append");
        // The name the old code would have found instead of the right one.
        std::fs::write(scratch.0.join("tool.exe"), b"wrong").unwrap();
        let wanted = scratch.0.join("tool.bar.exe");
        std::fs::write(&wanted, b"right").unwrap();

        let mut cmd = CommandBuilder::new("tool.bar");
        cmd.env("PATH", &scratch.0);
        cmd.env("PATHEXT", ".EXE");

        // Assert on WHICH program was selected rather than on the spelling of
        // the path: the extension carries PATHEXT's casing, and the filesystem
        // is case-insensitive, so `tool.bar.EXE` and `tool.bar.exe` name the
        // same file. What matters is that it is not `tool.exe`.
        let resolved = cmd.search_path(OsStr::new("tool.bar"));
        assert_eq!(
            std::fs::read(&resolved).expect("resolved path must exist"),
            b"right",
            "PATHEXT must extend the requested name, never replace part of it \
             (resolved to {resolved:?})"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_env_case_insensitive_override() {
        let mut cmd = CommandBuilder::new("dummy");
        cmd.env("Cargo_Pkg_Authors", "Not Wez");
        assert!(cmd.get_env("cargo_pkg_authors") == Some(OsStr::new("Not Wez")));

        cmd.env_remove("cARGO_pKG_aUTHORS");
        assert!(cmd.get_env("CARGO_PKG_AUTHORS").is_none());
    }
}
