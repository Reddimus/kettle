//! Kettle's Claude Code plugin: how Claude Code gets `kettle mcp --display`
//! and the hook that prints inline cards.
//!
//! Kettle writes the plugin itself, into a private directory named by its
//! contents, and offers it to Claude Code in each new pane by prepending that
//! directory to `CLAUDE_CODE_PLUGIN_DIRS`. Before every pane it checks the
//! directory again without following links: owned by this user or root, no
//! writable edge, read-only modes, no extra entries, and every file exactly
//! what this Kettle wrote. Anything else and the pane gets no plugin, and
//! Settings says why. Kettle never edits Claude Code's settings or its
//! installed-plugin registry.
//!
//! The check needs Unix ownership and modes, so elsewhere nothing is offered
//! and Settings says the system cannot run it yet.
#![cfg_attr(not(unix), allow(dead_code))]

#[cfg(unix)]
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// The plugin's name, which with its server's name makes the tool name the
/// hook matches: `mcp__plugin_kettle_kettle__kettle_show`.
const PLUGIN_NAME: &str = "kettle";
/// Largest file the check reads; Kettle's own are well under it.
#[cfg(test)]
const MAX_PLUGIN_FILE_BYTES: u64 = crate::owned_dir::MAX_FILE_BYTES;
/// The variable Claude Code (2.1.280 and later) reads extra plugin
/// directories from.
pub(crate) const PLUGIN_DIRS_VARIABLE: &str = "CLAUDE_CODE_PLUGIN_DIRS";

/// The plugin for one Kettle executable: its files, each by its path inside
/// the plugin directory, kept as [`crate::owned_dir`] keeps them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginFiles(crate::owned_dir::OwnedFiles);

impl std::ops::Deref for PluginFiles {
    type Target = crate::owned_dir::OwnedFiles;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for PluginFiles {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Why a pane gets no plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PluginRefusal {
    /// Claude Code's own settings forbid plugins from the environment.
    SideloadDisabled,
    /// Kettle runs from a translocated copy, whose path does not last.
    Translocated,
    /// Kettle's executable path is not one a plugin can name.
    UnnamedExecutable,
    /// The plugin directory's path is not one `CLAUDE_CODE_PLUGIN_DIRS` can
    /// hold: not UTF-8, or holding its `:` separator.
    UnnamedDirectory,
    /// The plugin directory could not be written, or failed its check.
    Unverified,
    /// This system has no way to check the plugin directory.
    #[cfg_attr(unix, allow(dead_code))]
    Unsupported,
}

impl PluginFiles {
    /// The plugin for `executable` at Kettle `version`.
    pub(crate) fn new(executable: &Path, version: &str) -> Result<Self, PluginRefusal> {
        let command = executable
            .to_str()
            .ok_or(PluginRefusal::UnnamedExecutable)?;
        if command.contains("/AppTranslocation/") {
            return Err(PluginRefusal::Translocated);
        }
        let manifest = serde_json::json!({
            "name": PLUGIN_NAME,
            "version": version,
            "description": "Kettle shows the images and diagrams an agent sends it, as a card under the call.",
        });
        let servers = serde_json::json!({
            "mcpServers": {
                PLUGIN_NAME: {
                    "command": command,
                    "args": ["mcp", "--display", "--claude-card-hook"],
                }
            }
        });
        let hooks = serde_json::json!({
            "hooks": {
                "PostToolUse": [{
                    "matcher": "mcp__plugin_kettle_kettle__kettle_show",
                    "hooks": [{
                        "type": "mcp_tool",
                        "server": "plugin:kettle:kettle",
                        "tool": "kettle_card",
                        "input": {"tool_use_id": "${tool_use_id}"},
                        "timeout": 3,
                    }],
                }]
            }
        });
        let text = |value: serde_json::Value| {
            let mut text = serde_json::to_string_pretty(&value).unwrap_or_default();
            text.push('\n');
            text
        };
        // A changed plugin gets a new directory, so one a running Claude
        // Code loaded is never rewritten.
        Ok(Self(crate::owned_dir::OwnedFiles::new(
            PLUGIN_NAME,
            command.to_owned(),
            version.to_owned(),
            vec![
                (".claude-plugin/plugin.json", text(manifest)),
                (".mcp.json", text(servers)),
                ("hooks/hooks.json", text(hooks)),
            ],
        )))
    }
}

/// Where Kettle keeps its agent plugins: inside Kettle's own data directory.
pub(crate) fn plugins_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    #[cfg(target_os = "macos")]
    return Some(home.join("Library/Application Support/kettle/agent-plugins"));
    #[cfg(not(target_os = "macos"))]
    {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".local/share"));
        Some(data.join("kettle/agent-plugins"))
    }
}

/// Write the plugin under `root` unless it is already there intact, and
/// remove what is stale; see [`crate::owned_dir::install`]. Returns the
/// plugin's directory.
pub(crate) fn install(root: &Path, files: &PluginFiles) -> Result<PathBuf, PluginRefusal> {
    if cfg!(not(unix)) {
        return Err(PluginRefusal::Unsupported);
    }
    crate::owned_dir::install(root, files, plugin_identity).map_err(|()| PluginRefusal::Unverified)
}

/// Replace whatever is at `directory` with the plugin.
#[cfg(all(unix, test))]
fn write_plugin(root: &Path, directory: &Path, files: &PluginFiles) {
    crate::owned_dir::write(root, directory, files);
}

/// Whether `directory` holds exactly the plugin; see
/// [`crate::owned_dir::verify`].
pub(crate) fn verify(directory: &Path, files: &PluginFiles) -> Result<(), PluginRefusal> {
    if cfg!(not(unix)) {
        return Err(PluginRefusal::Unsupported);
    }
    crate::owned_dir::verify(directory, files).map_err(|()| PluginRefusal::Unverified)
}

#[cfg(all(unix, test))]
use crate::owned_dir::{INSTALL_LOCK, remove_tree, version_key};

/// The executable a Kettle plugin directory's server runs and the version
/// its manifest names, when its `.mcp.json` reads as Kettle's.
#[cfg(unix)]
fn plugin_identity(directory: &Path) -> Option<(String, Option<String>)> {
    let read = |path: &str| -> Option<serde_json::Value> {
        serde_json::from_slice(&crate::owned_dir::read_small(directory, path)?).ok()
    };
    let servers = read(".mcp.json")?;
    let server = &servers["mcpServers"][PLUGIN_NAME];
    // Every Kettle's server is `kettle mcp …`.
    if server["args"][0].as_str() != Some("mcp") {
        return None;
    }
    let command = server["command"].as_str()?.to_owned();
    let version = read(".claude-plugin/plugin.json")
        .and_then(|manifest| manifest["version"].as_str().map(str::to_owned));
    Some((command, version))
}

#[cfg(not(unix))]
fn plugin_identity(_: &Path) -> Option<(String, Option<String>)> {
    None
}

/// Where Claude Code reads its managed policy, the only settings that can
/// forbid plugins from the environment (`disableSideloadFlags`). With that
/// set, Claude Code refuses to start while `CLAUDE_CODE_PLUGIN_DIRS` names a
/// directory, so a new pane then gets no plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PolicySources {
    /// The managed settings directory: `managed-settings.json` and the
    /// drop-ins in `managed-settings.d`.
    managed: PathBuf,
    /// Claude Code's configuration directory, which caches remote policy;
    /// `None` when Kettle cannot tell where it is, which forbids the plugin.
    config: Option<PathBuf>,
    /// macOS managed preferences, per user and then per device.
    preferences: Vec<PathBuf>,
}

impl PolicySources {
    /// This machine's sources, for a Claude Code configured in `config`.
    pub(crate) fn for_machine(config: Option<PathBuf>) -> Self {
        let managed = PathBuf::from(if cfg!(target_os = "macos") {
            "/Library/Application Support/ClaudeCode"
        } else {
            "/etc/claude-code"
        });
        #[allow(unused_mut)]
        let mut preferences = Vec::new();
        #[cfg(target_os = "macos")]
        {
            const DOMAIN: &str = "com.anthropic.claudecode.plist";
            let root = Path::new("/Library/Managed Preferences");
            if let Some(user) = user_name() {
                preferences.push(root.join(user).join(DOMAIN));
            }
            preferences.push(root.join(DOMAIN));
        }
        Self {
            managed,
            config,
            preferences,
        }
    }

    /// `managed-settings.json` and the drop-ins in `managed-settings.d`, in
    /// the order Claude Code merges them; `None` when the drop-ins cannot
    /// all be listed.
    fn managed_files(&self) -> Option<Vec<PathBuf>> {
        let mut files = vec![self.managed.join("managed-settings.json")];
        let drop_ins = self.managed.join("managed-settings.d");
        match std::fs::read_dir(&drop_ins) {
            Ok(entries) => {
                let mut names = Vec::new();
                for entry in entries {
                    let entry = entry.ok()?;
                    let kind = entry.file_type().ok()?;
                    // Claude Code reads files and links, nothing else.
                    if !(kind.is_file() || kind.is_symlink()) {
                        continue;
                    }
                    if let Some(name) = drop_in_name(&entry.file_name())? {
                        names.push(name);
                    }
                    if names.len() > MAX_POLICY_DROP_INS {
                        return None;
                    }
                }
                // Claude Code sorts as JavaScript does, by UTF-16 units.
                names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
                files.extend(names.into_iter().map(|name| drop_ins.join(name)));
            }
            Err(error) if absent(&error) => {}
            Err(_) => return None,
        }
        Some(files)
    }

    /// The cached remote policy; `None` when Kettle cannot tell where it is.
    fn remote_file(&self) -> Option<PathBuf> {
        self.config
            .as_ref()
            .map(|config| config.join("remote-settings.json"))
    }
}

/// A drop-in's name when Claude Code reads it as policy: visible, and
/// ending in `.json`. `Some(None)` for a name it skips, and `None` for a
/// policy name that is not UTF-8, which Kettle cannot read as Claude Code
/// does.
fn drop_in_name(name: &std::ffi::OsStr) -> Option<Option<String>> {
    let bytes = name.as_encoded_bytes();
    if !bytes.ends_with(b".json") || bytes.starts_with(b".") {
        return Some(None);
    }
    name.to_str().map(|name| Some(name.to_owned()))
}

/// Claude Code's configuration directory for a pane whose environment sets
/// `CLAUDE_CONFIG_DIR` and `HOME` to these, if anything. `None` when Kettle
/// cannot tell: a relative directory, which Claude Code takes from wherever
/// it starts, or no home to default to.
pub(crate) fn claude_config_dir(
    configured: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let absolute = |path: PathBuf| Some(path).filter(|path| path.is_absolute());
    match configured.filter(|value| !value.is_empty()) {
        Some(value) => absolute(PathBuf::from(value)),
        None => absolute(PathBuf::from(home.filter(|home| !home.is_empty())?))
            .map(|home| home.join(".claude")),
    }
}

/// Whether Claude Code's managed policy forbids plugins from the
/// environment. Kettle takes the managed file and its drop-ins as Claude Code
/// merges them, and counts a rule in any of the other sources, which Claude
/// Code may instead pass over for a higher one: Kettle can withhold the
/// plugin where Claude Code would allow it, but not the reverse for what it
/// can read. A source Kettle cannot read whole, or find, counts as forbidding
/// it. The answer is kept until a source changes on disk.
#[cfg(unix)]
pub(crate) fn sideload_disabled(sources: &PolicySources) -> bool {
    let (Some(managed), Some(remote)) = (sources.managed_files(), sources.remote_file()) else {
        return true;
    };
    let stamps: Vec<_> = managed
        .iter()
        .chain([&remote])
        .chain(&sources.preferences)
        .chain(std::iter::once(&sources.managed.join("managed-settings.d")))
        .map(|path| file_stamp(path))
        .collect();
    let cacheable = !stamps.contains(&FileStamp::Unknown);
    let mut cache = POLICY_CACHE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if cacheable
        && let Some((cached_sources, cached_stamps, verdict)) = cache.as_ref()
        && cached_sources == sources
        && *cached_stamps == stamps
    {
        return *verdict;
    }
    let verdict = managed_forbids(&managed)
        || read_json_policy(&remote).forbids()
        || sources
            .preferences
            .iter()
            .any(|path| read_plist_policy(path).forbids());
    *cache = cacheable.then(|| (sources.clone(), stamps, verdict));
    verdict
}

#[cfg(not(unix))]
pub(crate) fn sideload_disabled(_: &PolicySources) -> bool {
    true
}

/// Whether the managed file and its drop-ins, merged in order as Claude Code
/// merges them, forbid plugins: the last to set the rule decides, and one
/// Kettle cannot read whole forbids them.
#[cfg(unix)]
fn managed_forbids(files: &[PathBuf]) -> bool {
    let mut forbidden = false;
    for path in files {
        match read_json_policy(path) {
            PolicyReading::Absent => {}
            PolicyReading::Unreadable => return true,
            PolicyReading::Settings(settings) => {
                if let Some(serde_json::Value::Bool(rule)) = settings.get(SIDELOAD_RULE) {
                    forbidden = *rule;
                }
            }
        }
    }
    forbidden
}

/// The policy rule that forbids plugins from the environment.
const SIDELOAD_RULE: &str = "disableSideloadFlags";

/// What a policy file looked like when it was read, which tells the cached
/// answer is stale.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum FileStamp {
    Absent,
    /// Its identity, size and change times.
    Present(u64, u64, u64, i64, i64, i64, i64),
    /// Its metadata could not be read; no answer is kept beside it.
    Unknown,
}

#[cfg(unix)]
fn file_stamp(path: &Path) -> FileStamp {
    use std::os::unix::fs::MetadataExt as _;
    match std::fs::metadata(path) {
        Ok(metadata) => FileStamp::Present(
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        ),
        Err(error) if absent(&error) => FileStamp::Absent,
        Err(_) => FileStamp::Unknown,
    }
}

#[cfg(unix)]
static POLICY_CACHE: std::sync::Mutex<Option<(PolicySources, Vec<FileStamp>, bool)>> =
    std::sync::Mutex::new(None);

/// What one policy source says.
#[derive(Debug, PartialEq)]
enum PolicyReading {
    Absent,
    Settings(serde_json::Value),
    /// There, but not readable whole.
    Unreadable,
}

impl PolicyReading {
    fn forbids(&self) -> bool {
        match self {
            Self::Absent => false,
            Self::Settings(settings) => {
                settings.get(SIDELOAD_RULE) == Some(&serde_json::Value::Bool(true))
            }
            Self::Unreadable => true,
        }
    }

    /// Settings text as Claude Code would take it: a leading byte-order mark
    /// is dropped, and a file that is not JSON sets nothing.
    fn parse(text: &[u8]) -> Self {
        let text = text.strip_prefix(b"\xef\xbb\xbf").unwrap_or(text);
        serde_json::from_slice(text).map_or(Self::Absent, Self::Settings)
    }
}

fn absent(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// Read a JSON policy file without blocking on a FIFO or reading past the
/// limit.
#[cfg(unix)]
fn read_json_policy(path: &Path) -> PolicyReading {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if absent(&error) => return PolicyReading::Absent,
        Err(_) => return PolicyReading::Unreadable,
    };
    match file.metadata() {
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_POLICY_BYTES => {}
        _ => return PolicyReading::Unreadable,
    }
    let mut text = Vec::new();
    match file.take(MAX_POLICY_BYTES + 1).read_to_end(&mut text) {
        Ok(_) if text.len() as u64 <= MAX_POLICY_BYTES => PolicyReading::parse(&text),
        _ => PolicyReading::Unreadable,
    }
}

/// Read a managed-preferences plist the way Claude Code does, through
/// `plutil`, waiting at most `PLUTIL_TIMEOUT`.
#[cfg(unix)]
fn read_plist_policy(path: &Path) -> PolicyReading {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_POLICY_BYTES => {}
        Ok(_) => return PolicyReading::Unreadable,
        Err(error) if absent(&error) => return PolicyReading::Absent,
        Err(_) => return PolicyReading::Unreadable,
    }
    let Ok(mut child) = std::process::Command::new("/usr/bin/plutil")
        .args(["-convert", "json", "-o", "-", "--"])
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return PolicyReading::Unreadable;
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return PolicyReading::Unreadable;
    };
    // Read on another thread so a large conversion cannot fill the pipe
    // while this one waits.
    let reader = std::thread::spawn(move || {
        let mut text = Vec::new();
        stdout
            .take(MAX_POLICY_BYTES + 1)
            .read_to_end(&mut text)
            .map(|_| text)
    });
    let deadline = std::time::Instant::now() + PLUTIL_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let text = reader.join().ok().and_then(Result::ok);
    match (status, text) {
        (Some(status), Some(text)) if status.success() && text.len() as u64 <= MAX_POLICY_BYTES => {
            PolicyReading::parse(&text)
        }
        _ => PolicyReading::Unreadable,
    }
}

/// This user's name from the user database, as Claude Code finds its
/// per-user managed preferences.
#[cfg(target_os = "macos")]
fn user_name() -> Option<String> {
    let mut buffer = vec![0u8; 16 * 1024];
    // SAFETY: an all-zero `passwd` is a valid value of this C struct.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call and `buffer` outlives the
    // strings `entry` points into, which are read before it is dropped.
    let rc = unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            &mut entry,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut found,
        )
    };
    if rc != 0 || found.is_null() || entry.pw_name.is_null() {
        return None;
    }
    // SAFETY: `pw_name` is a NUL-terminated string inside `buffer`.
    let name = unsafe { std::ffi::CStr::from_ptr(entry.pw_name) };
    name.to_str()
        .ok()
        .filter(|name| !name.is_empty() && !name.contains('/') && *name != "..")
        .map(str::to_owned)
}

/// Largest policy file read, as Claude Code limits it.
const MAX_POLICY_BYTES: u64 = 2 * 1024 * 1024;
/// Most policy drop-ins read; more counts as unreadable.
const MAX_POLICY_DROP_INS: usize = 256;
/// Longest wait for `plutil` to convert a managed-preferences plist.
#[cfg(unix)]
const PLUTIL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// The plugin Kettle offers new panes, and why the last pane got none.
static PLUGIN: RwLock<Option<(PathBuf, PluginFiles)>> = RwLock::new(None);
static REFUSAL: RwLock<Option<PluginRefusal>> = RwLock::new(None);

/// Offer `plugin` to new panes, or none; `refusal` says why there is none.
pub(crate) fn offer(plugin: Option<(PathBuf, PluginFiles)>, refusal: Option<PluginRefusal>) {
    if let Ok(mut offered) = PLUGIN.write() {
        *offered = plugin;
    }
    if let Ok(mut why) = REFUSAL.write() {
        *why = refusal;
    }
}

/// Why panes get no plugin, for Settings; `None` when they do, or when the
/// integration is off.
pub(crate) fn refusal() -> Option<PluginRefusal> {
    REFUSAL.read().ok().and_then(|why| *why)
}

/// The plugin directory for a new pane whose Claude Code is configured in
/// the directory `claude_config` names, checked now. `None` when the
/// integration is off, Claude Code's policy forbids it, or the check fails,
/// which Settings then reports.
pub(crate) fn for_new_pane(claude_config: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    let checked = check_offered(|| PolicySources::for_machine(claude_config()))?;
    if let Ok(mut why) = REFUSAL.write() {
        *why = checked.as_ref().err().copied();
    }
    checked.ok()
}

/// The offered plugin checked now: its directory, or why a pane cannot have
/// it under the policy in `sources`; `None` when nothing is offered.
fn check_offered(
    sources: impl FnOnce() -> PolicySources,
) -> Option<Result<PathBuf, PluginRefusal>> {
    let (directory, files) = PLUGIN.read().ok()?.clone()?;
    Some(if sideload_disabled(&sources()) {
        Err(PluginRefusal::SideloadDisabled)
    } else {
        verify(&directory, &files).map(|()| directory)
    })
}

/// What `kettle agent-setup --status` reports about Claude Code, for a shell
/// whose environment `var` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaudeStatus {
    /// The shell's pane started with Kettle's plugin offered to Claude Code.
    pub in_pane: bool,
    /// Claude Code's managed policy on this machine forbids plugins from the
    /// environment, or Kettle cannot read all of it. A policy managed
    /// remotely is not visible to Kettle.
    pub forbidden: bool,
    /// This Kettle can make its plugin: it does not run from a translocated
    /// copy or a path the plugin cannot name.
    pub offerable: bool,
}

/// [`ClaudeStatus`] for the shell whose environment `var` reads: its
/// `CLAUDE_CODE_PLUGIN_DIRS`, and the policy for the Claude Code its
/// `CLAUDE_CONFIG_DIR` and `HOME` name.
pub fn claude_status(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> ClaudeStatus {
    let in_pane = var(PLUGIN_DIRS_VARIABLE).is_some_and(|value| {
        value
            .to_string_lossy()
            .split(':')
            .any(|entry| is_kettle_plugin(entry.trim()))
    });
    let config = claude_config_dir(var("CLAUDE_CONFIG_DIR").as_deref(), var("HOME").as_deref());
    let offerable = std::env::current_exe()
        .is_ok_and(|kettle| PluginFiles::new(&kettle, env!("CARGO_PKG_VERSION")).is_ok());
    ClaudeStatus {
        in_pane,
        forbidden: sideload_disabled(&PolicySources::for_machine(config)),
        offerable,
    }
}

/// The entry `directory` makes in `CLAUDE_CODE_PLUGIN_DIRS`; `None` when the
/// variable cannot hold it.
pub(crate) fn plugin_dir_entry(directory: &Path) -> Option<&str> {
    directory
        .to_str()
        .filter(|entry| !entry.contains(':') && directory.is_absolute())
}

/// `CLAUDE_CODE_PLUGIN_DIRS` for a new pane: Kettle's plugin `directory`
/// first when it offers one, then the entries the pane would otherwise have
/// had, less any Kettle plugin. Those an outer Kettle or an older version
/// left would bypass this Kettle's checks, and with Claude Code's policy
/// forbidding plugins would stop it starting. `None` when the pane's value
/// can stay as it is.
pub(crate) fn plugin_dirs_value(
    directory: Option<&Path>,
    existing: Option<&str>,
) -> Option<String> {
    let entry = directory.and_then(plugin_dir_entry);
    let others: Vec<&str> = existing
        .unwrap_or_default()
        .split(':')
        .filter(|other| !other.trim().is_empty())
        .collect();
    let kept: Vec<&str> = others
        .iter()
        .copied()
        .filter(|other| !is_kettle_plugin(other.trim()))
        .collect();
    if entry.is_none() && kept.len() == others.len() {
        return None;
    }
    Some(entry.into_iter().chain(kept).collect::<Vec<_>>().join(":"))
}

/// Whether an entry names a Kettle plugin directory, wherever its Kettle
/// keeps them: `…/kettle/agent-plugins/kettle-` and 16 hex digits, in any
/// case, as a case-insensitive volume finds it.
fn is_kettle_plugin(entry: &str) -> bool {
    fn name(path: Option<&Path>) -> Option<&str> {
        path?.file_name()?.to_str()
    }
    let path = Path::new(entry);
    let plugins = path.parent();
    let leaf = name(Some(path)).unwrap_or_default();
    let hash = leaf
        .get(..PLUGIN_NAME.len() + 1)
        .filter(|prefix| prefix.eq_ignore_ascii_case(&format!("{PLUGIN_NAME}-")))
        .and_then(|_| leaf.get(PLUGIN_NAME.len() + 1..));
    hash.is_some_and(|hash| hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        && name(plugins).is_some_and(|plugins| plugins.eq_ignore_ascii_case("agent-plugins"))
        && name(plugins.and_then(Path::parent))
            .is_some_and(|kettle| kettle.eq_ignore_ascii_case(PLUGIN_NAME))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn files() -> PluginFiles {
        PluginFiles::new(
            Path::new("/Applications/Kettle.app/Contents/MacOS/kettle"),
            "5.0.0",
        )
        .unwrap()
    }

    /// A shell's pane has Kettle's plugin when its `CLAUDE_CODE_PLUGIN_DIRS`
    /// names one, among others or not, and not for another plugin's.
    #[test]
    fn a_pane_has_the_plugin_when_its_variable_names_kettles() {
        let status = |dirs: Option<&str>| {
            claude_status(|name| match name {
                PLUGIN_DIRS_VARIABLE => dirs.map(Into::into),
                "HOME" => Some("/nowhere".into()),
                _ => None,
            })
            .in_pane
        };
        let ours = "/data/kettle/agent-plugins/kettle-00000000000000aa";
        assert!(status(Some(ours)));
        assert!(status(Some(&format!("/opt/other: {ours} "))));
        assert!(!status(Some("/opt/other/agent-plugins/kettle-zz")));
        assert!(!status(None));
    }

    #[test]
    fn the_plugin_launches_this_kettles_display_server_and_its_card_hook() {
        let files = files();
        let servers: serde_json::Value = serde_json::from_str(&files.files[1].1).unwrap();
        assert_eq!(
            servers["mcpServers"]["kettle"]["command"],
            "/Applications/Kettle.app/Contents/MacOS/kettle"
        );
        assert_eq!(
            servers["mcpServers"]["kettle"]["args"],
            serde_json::json!(["mcp", "--display", "--claude-card-hook"])
        );
        let hooks: serde_json::Value = serde_json::from_str(&files.files[2].1).unwrap();
        let post = &hooks["hooks"]["PostToolUse"][0];
        assert_eq!(post["matcher"], "mcp__plugin_kettle_kettle__kettle_show");
        assert_eq!(post["hooks"][0]["tool"], "kettle_card");
        assert_eq!(post["hooks"][0]["input"]["tool_use_id"], "${tool_use_id}");
        assert_eq!(post["hooks"][0]["timeout"], 3);
        assert!(hooks["hooks"].get("PreToolUse").is_none(), "no auto-allow");
        // Another executable is another plugin, in another directory.
        let other = PluginFiles::new(Path::new("/opt/kettle"), "5.0.0").unwrap();
        assert_ne!(other.directory_name(), files.directory_name());
        assert_eq!(
            files.directory_name(),
            self::files().directory_name(),
            "stable"
        );
        assert_eq!(
            PluginFiles::new(
                Path::new("/private/var/folders/x/AppTranslocation/y/kettle"),
                "5.0.0"
            ),
            Err(PluginRefusal::Translocated)
        );
    }

    #[test]
    fn installing_writes_a_read_only_plugin_that_verifies_and_clears_old_ones() {
        let base = kettle_test_support::private_tempdir("kettle-plugin-");
        let root = base.path().join("kettle/agent-plugins");
        let stale = root.join("kettle-0000000000000000");
        std::fs::create_dir_all(&stale).unwrap();
        let files = files();
        let directory = install(&root, &files).unwrap();
        assert_eq!(directory, root.join(files.directory_name()));
        assert_eq!(verify(&directory, &files), Ok(()));
        let mode = |path: &Path| path.symlink_metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&directory), 0o500);
        assert_eq!(mode(&directory.join("hooks")), 0o500);
        assert_eq!(mode(&directory.join("hooks/hooks.json")), 0o400);
        assert!(!stale.exists(), "an unreadable leftover is removed");
        // Installing again keeps the intact directory.
        assert_eq!(install(&root, &files).unwrap(), directory);
        remove_tree(&base.path().join("kettle")).unwrap();
    }

    /// Installing removes this executable's plugins from the same or an
    /// older version, plugins whose executable is gone, and staging an
    /// ended install left, but not another installed Kettle's plugin, nor a
    /// newer version's for this executable.
    #[test]
    fn installing_keeps_other_installs_plugins() {
        let base = kettle_test_support::private_tempdir("kettle-plugin-");
        let root = base.path().join("kettle/agent-plugins");
        let own_exe = base.path().join("kettle-bin");
        let other_exe = base.path().join("other-kettle");
        std::fs::write(&own_exe, b"").unwrap();
        std::fs::write(&other_exe, b"").unwrap();
        let own = PluginFiles::new(&own_exe, "5.0.0").unwrap();
        let older = PluginFiles::new(&own_exe, "4.10.0").unwrap();
        let newer = PluginFiles::new(&own_exe, "5.0.10").unwrap();
        let mut rebuilt = own.clone();
        rebuilt.files[0].1.push('\n');
        let other = PluginFiles::new(&other_exe, "5.0.0").unwrap();
        let gone = PluginFiles::new(&base.path().join("deleted-kettle"), "5.0.0").unwrap();
        // Placed without the cleanup an install does, so only the last
        // install below removes anything.
        kettle_state::create_private_dirs(&root).unwrap();
        let place = |files: &PluginFiles| {
            let directory = root.join(files.directory_name());
            write_plugin(&root, &directory, files);
            assert_eq!(verify(&directory, files), Ok(()));
            directory
        };
        let older_dir = place(&older);
        let rebuilt_dir = place(&rebuilt);
        let newer_dir = place(&newer);
        let other_dir = place(&other);
        let gone_dir = place(&gone);
        let left = root.join(".staging-0000000000000000");
        std::fs::create_dir(&left).unwrap();
        // Leftovers that name a live program but do not read as Kettle's.
        let other_command = other_exe.to_str().unwrap();
        let foreign = |name: &str, text: String| {
            let directory = root.join(name);
            std::fs::create_dir(&directory).unwrap();
            std::fs::write(directory.join(".mcp.json"), text).unwrap();
            directory
        };
        let not_mcp = foreign(
            "kettle-00000000000000c1",
            serde_json::json!({"mcpServers": {"kettle": {
                "command": other_command, "args": ["--version"]}}})
            .to_string(),
        );
        let overlong = foreign(
            "kettle-00000000000000c2",
            format!(
                "{}{}junk",
                serde_json::json!({"mcpServers": {"kettle": {
                    "command": other_command, "args": ["mcp", "--display"]}}}),
                " ".repeat(MAX_PLUGIN_FILE_BYTES as usize)
            ),
        );
        let directory = install(&root, &own).unwrap();
        assert!(!not_mcp.exists(), "a server that is not Kettle's");
        assert!(!overlong.exists(), "a file read only in part");
        assert!(!older_dir.exists(), "this executable's older version");
        assert!(!rebuilt_dir.exists(), "another build of this version");
        assert!(!gone_dir.exists(), "a plugin whose Kettle is gone");
        assert!(!left.exists(), "staging an ended install left");
        assert!(newer_dir.exists(), "a newer version's, upgraded in place");
        assert!(other_dir.exists(), "another installed Kettle's");
        assert_eq!(verify(&newer_dir, &newer), Ok(()));
        assert_eq!(verify(&other_dir, &other), Ok(()));
        assert_eq!(verify(&directory, &own), Ok(()));
        remove_tree(&base.path().join("kettle")).unwrap();
    }

    /// Kettles sharing the plugins directory install one at a time.
    #[test]
    fn an_install_waits_for_another_to_finish() {
        let base = kettle_test_support::private_tempdir("kettle-plugin-");
        let root = base.path().join("kettle/agent-plugins");
        kettle_state::create_private_dirs(&root).unwrap();
        let held = kettle_state::ExclusiveFileLock::acquire(&root.join(INSTALL_LOCK)).unwrap();
        let installing = std::thread::spawn({
            let root = root.clone();
            move || install(&root, &files())
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !root.join(files().directory_name()).exists(),
            "nothing written while another install holds the lock"
        );
        drop(held);
        let directory = installing.join().unwrap().unwrap();
        assert_eq!(verify(&directory, &files()), Ok(()));
        remove_tree(&base.path().join("kettle")).unwrap();
    }

    #[test]
    fn versions_compare_by_their_numbered_parts() {
        assert!(version_key("5.0.10") > version_key("5.0.9"));
        assert!(version_key("10.0.0") > version_key("9.9.9"));
        assert_eq!(version_key("5.0.0-dev"), None);
        assert_eq!(version_key(""), None);
    }

    #[test]
    fn a_changed_extra_linked_or_writable_plugin_fails_its_check() {
        let base = kettle_test_support::private_tempdir("kettle-plugin-");
        let root = base.path().join("kettle/agent-plugins");
        let files = files();
        let directory = install(&root, &files).unwrap();
        let writable = |path: &Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        let mcp = directory.join(".mcp.json");
        // A changed file.
        writable(&directory, 0o700);
        writable(&mcp, 0o600);
        std::fs::write(&mcp, b"{\"mcpServers\":{}}\n").unwrap();
        writable(&mcp, 0o400);
        writable(&directory, 0o500);
        assert_eq!(verify(&directory, &files), Err(PluginRefusal::Unverified));
        // Reinstalling repairs it.
        assert_eq!(install(&root, &files).unwrap(), directory);
        assert_eq!(verify(&directory, &files), Ok(()));
        // An extra entry.
        writable(&directory, 0o700);
        std::fs::write(directory.join("extra"), b"x").unwrap();
        writable(&directory, 0o500);
        assert_eq!(verify(&directory, &files), Err(PluginRefusal::Unverified));
        install(&root, &files).unwrap();
        // A link in place of a file.
        writable(&directory, 0o700);
        let target = base.path().join("elsewhere.json");
        std::fs::write(&target, files.files[1].1.as_bytes()).unwrap();
        writable(&mcp, 0o600);
        std::fs::remove_file(&mcp).unwrap();
        std::os::unix::fs::symlink(&target, &mcp).unwrap();
        writable(&directory, 0o500);
        assert_eq!(verify(&directory, &files), Err(PluginRefusal::Unverified));
        install(&root, &files).unwrap();
        // A writable directory.
        writable(&directory.join("hooks"), 0o700);
        assert_eq!(verify(&directory, &files), Err(PluginRefusal::Unverified));
        install(&root, &files).unwrap();
        assert_eq!(verify(&directory, &files), Ok(()));
        remove_tree(&base.path().join("kettle")).unwrap();
    }

    /// A new pane gets the offered plugin only while Claude Code's policy
    /// allows it and its directory still verifies.
    #[test]
    fn a_new_pane_gets_the_plugin_only_while_allowed_and_verified() {
        let base = kettle_test_support::private_tempdir("kettle-plugin-");
        let root = base.path().join("kettle/agent-plugins");
        let files = files();
        let directory = install(&root, &files).unwrap();
        let sources = || policy(base.path());
        offer(Some((directory.clone(), files)), None);
        assert_eq!(check_offered(sources), Some(Ok(directory.clone())));
        let managed = base.path().join("managed/managed-settings.json");
        write(&managed, FORBID);
        assert_eq!(
            check_offered(sources),
            Some(Err(PluginRefusal::SideloadDisabled))
        );
        std::fs::remove_file(&managed).unwrap();
        assert_eq!(check_offered(sources), Some(Ok(directory.clone())));
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            check_offered(sources),
            Some(Err(PluginRefusal::Unverified)),
            "no longer read-only"
        );
        offer(None, None);
        assert_eq!(check_offered(sources), None, "withdrawn");
        remove_tree(&base.path().join("kettle")).unwrap();
    }

    fn policy(base: &Path) -> PolicySources {
        PolicySources {
            managed: base.join("managed"),
            config: Some(base.join("config")),
            preferences: Vec::new(),
        }
    }

    fn write(path: &Path, text: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    const FORBID: &[u8] = br#"{"theme": "dark", "disableSideloadFlags": true}"#;

    #[test]
    fn only_claude_codes_managed_policy_can_forbid_the_plugin() {
        let base = kettle_test_support::private_tempdir("kettle-plugin-policy-");
        let sources = policy(base.path());
        assert!(!sideload_disabled(&sources), "no policy, no rule");
        // The user's own settings cannot set it; Claude Code reads it from
        // policy alone.
        write(&base.path().join("config/settings.json"), FORBID);
        assert!(!sideload_disabled(&sources));
        let managed = base.path().join("managed/managed-settings.json");
        write(&managed, br#"{"disableSideloadFlags": false}"#);
        assert!(!sideload_disabled(&sources));
        write(&managed, FORBID);
        assert!(sideload_disabled(&sources), "the managed file");
        std::fs::remove_file(&managed).unwrap();
        assert!(!sideload_disabled(&sources), "a change on disk is seen");
        let drop_ins = base.path().join("managed/managed-settings.d");
        write(&drop_ins.join(".hidden.json"), FORBID);
        write(&drop_ins.join("50-notes.txt"), FORBID);
        assert!(!sideload_disabled(&sources), "only visible JSON drop-ins");
        std::fs::create_dir(drop_ins.join("10-folder.json")).unwrap();
        assert!(!sideload_disabled(&sources), "nor a directory named as one");
        write(&drop_ins.join("50-org.json"), FORBID);
        assert!(sideload_disabled(&sources), "a drop-in");
        // The managed file and its drop-ins merge in order: the last to set
        // the rule decides.
        write(
            &drop_ins.join("90-team.json"),
            br#"{"disableSideloadFlags": false}"#,
        );
        assert!(!sideload_disabled(&sources), "a later drop-in lifts it");
        write(&drop_ins.join("95-notes.json"), br#"{"theme": "light"}"#);
        assert!(!sideload_disabled(&sources), "one that does not set it");
        write(&managed, FORBID);
        std::fs::remove_file(drop_ins.join("50-org.json")).unwrap();
        assert!(!sideload_disabled(&sources), "a drop-in lifts the file's");
        std::fs::remove_file(&managed).unwrap();
        // Merged in JavaScript's UTF-16 order, where U+10000 comes before
        // U+E000, not in UTF-8's.
        write(&drop_ins.join("\u{e000}.json"), FORBID);
        write(
            &drop_ins.join("\u{10000}.json"),
            br#"{"disableSideloadFlags": false}"#,
        );
        assert!(
            sideload_disabled(&sources),
            "the last in Claude Code's order"
        );
        std::fs::remove_dir_all(&drop_ins).unwrap();
        // A byte-order mark is dropped, as Claude Code drops it.
        write(&managed, &[b"\xef\xbb\xbf".as_slice(), FORBID].concat());
        assert!(sideload_disabled(&sources), "after a byte-order mark");
        std::fs::remove_file(&managed).unwrap();
        let remote = base.path().join("config/remote-settings.json");
        write(&remote, FORBID);
        assert!(sideload_disabled(&sources), "cached remote policy");
        write(&managed, br#"{"disableSideloadFlags": false}"#);
        assert!(
            sideload_disabled(&sources),
            "any source's rule counts, beside the managed file's"
        );
        std::fs::remove_file(&managed).unwrap();
        write(&remote, b"not json");
        assert!(
            !sideload_disabled(&sources),
            "a file that is not JSON sets nothing"
        );
    }

    #[test]
    fn a_policy_kettle_cannot_read_whole_forbids_the_plugin() {
        let base = kettle_test_support::private_tempdir("kettle-plugin-policy-");
        let sources = policy(base.path());
        let managed = base.path().join("managed/managed-settings.json");
        write(&managed, &vec![b' '; MAX_POLICY_BYTES as usize + 1]);
        assert!(sideload_disabled(&sources), "too large");
        std::fs::remove_file(&managed).unwrap();
        // A FIFO is not waited on.
        let fifo = std::ffi::CString::new(managed.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(sideload_disabled(&sources), "not a file");
        std::fs::remove_file(&managed).unwrap();
        let drop_ins = base.path().join("managed/managed-settings.d");
        for n in 0..=MAX_POLICY_DROP_INS {
            write(&drop_ins.join(format!("{n:03}.json")), b"{}");
        }
        assert!(sideload_disabled(&sources), "too many drop-ins");
        std::fs::remove_dir_all(&drop_ins).unwrap();
        // No configuration directory Kettle can place.
        assert!(sideload_disabled(&PolicySources {
            config: None,
            ..policy(base.path())
        }));
        // A policy that becomes unreadable is not answered from the cache.
        let config = base.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        assert!(!sideload_disabled(&sources));
        write(&config.join("remote-settings.json"), b"{}");
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = sideload_disabled(&sources);
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o700)).unwrap();
        // SAFETY: `geteuid` has no preconditions. Root searches any
        // directory whatever its mode.
        if unsafe { libc::geteuid() } != 0 {
            assert!(unreadable, "its directory cannot be searched");
        }
    }

    #[test]
    fn drop_ins_are_named_as_claude_code_reads_them() {
        use std::os::unix::ffi::OsStrExt as _;
        let name = |bytes: &[u8]| drop_in_name(std::ffi::OsStr::from_bytes(bytes));
        assert_eq!(name(b"50-org.json"), Some(Some("50-org.json".into())));
        assert_eq!(name(b".hidden.json"), Some(None));
        assert_eq!(name(b"notes.txt"), Some(None));
        assert_eq!(name(b"\xff.txt"), Some(None), "skipped, whatever its bytes");
        assert_eq!(name(b"\xff.json"), None, "policy Kettle cannot name");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn managed_preferences_can_forbid_the_plugin() {
        let base = kettle_test_support::private_tempdir("kettle-plugin-policy-");
        let plist = base.path().join("com.anthropic.claudecode.plist");
        let sources = PolicySources {
            preferences: vec![base.path().join("absent.plist"), plist.clone()],
            ..policy(base.path())
        };
        let preferences = |forbid: &str| {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <plist version=\"1.0\"><dict>\
                 <key>disableSideloadFlags</key><{forbid}/>\
                 </dict></plist>\n"
            )
        };
        write(&plist, preferences("false").as_bytes());
        assert!(!sideload_disabled(&sources));
        write(&plist, preferences("true").as_bytes());
        assert!(sideload_disabled(&sources));
        write(&plist, b"not a plist");
        assert!(sideload_disabled(&sources), "plutil cannot read it");
        assert!(user_name().is_some_and(|name| !name.contains('/')));
    }

    #[test]
    fn claude_codes_configuration_directory_follows_the_panes_environment() {
        use std::ffi::OsStr;
        let home = Some(OsStr::new("/home/pane"));
        assert_eq!(
            claude_config_dir(Some(OsStr::new("/srv/claude")), home),
            Some(PathBuf::from("/srv/claude"))
        );
        // Claude Code takes a relative one from wherever it starts.
        assert_eq!(claude_config_dir(Some(OsStr::new("relative")), home), None);
        let default = Some(PathBuf::from("/home/pane/.claude"));
        assert_eq!(claude_config_dir(Some(OsStr::new("")), home), default);
        assert_eq!(claude_config_dir(None, home), default);
        assert_eq!(claude_config_dir(None, None), None);
        assert_eq!(claude_config_dir(None, Some(OsStr::new(""))), None);
        assert_eq!(claude_config_dir(None, Some(OsStr::new("home"))), None);
    }

    #[test]
    fn the_plugin_comes_first_and_keeps_the_panes_other_plugins() {
        let ours = Path::new("/data/kettle/agent-plugins/kettle-00000000000000aa");
        let value = |existing: Option<&str>| plugin_dirs_value(Some(ours), existing).unwrap();
        let first = "/data/kettle/agent-plugins/kettle-00000000000000aa";
        assert_eq!(value(None), first);
        assert_eq!(value(Some("")), first);
        assert_eq!(value(Some("/a:/b")), format!("{first}:/a:/b"));
        // An outer Kettle's entry or an older version's is not kept,
        // wherever that Kettle keeps its plugins.
        assert_eq!(
            value(Some(
                "/data/kettle/agent-plugins/kettle-00000000000000bb: /a :/data/kettle/agent-plugins/kettle-00000000000000aa"
            )),
            format!("{first}: /a ")
        );
        for outer in [
            "/elsewhere/kettle/agent-plugins/kettle-00000000000000bb",
            "/Users/u/Library/Application Support/Kettle/agent-plugins/kettle-00000000000000bb/",
            "~/.local/share/kettle/agent-plugins/kettle-00000000000000bb",
            "/data/Kettle/Agent-Plugins/Kettle-00000000000000BB",
        ] {
            let existing = format!("{outer}:/a");
            assert_eq!(value(Some(&existing)), format!("{first}:/a"), "{outer}");
        }
        // Other plugins stay, even beside Kettle's or named like them.
        for mine in [
            "/data/kettle/agent-plugins/mine",
            "/data/kettle/agent-plugins/kettle-1",
            "/data/other/agent-plugins/kettle-00000000000000bb",
            "/data/kettle/plugins/kettle-00000000000000bb",
        ] {
            assert_eq!(value(Some(mine)), format!("{first}:{mine}"), "{mine}");
        }
        // Offering none, only Kettle entries go; with none, nothing changes.
        assert_eq!(
            plugin_dirs_value(
                None,
                Some("/data/kettle/agent-plugins/kettle-00000000000000bb:/a")
            ),
            Some("/a".into())
        );
        assert_eq!(plugin_dirs_value(None, Some("/a::/b")), None);
        assert_eq!(plugin_dirs_value(None, None), None);
        assert_eq!(plugin_dir_entry(Path::new("/odd:path/kettle-1")), None);
        assert_eq!(plugin_dir_entry(Path::new("relative/kettle-1")), None);
        assert_eq!(
            plugin_dirs_value(Some(Path::new("/odd:path/kettle-1")), Some("/a")),
            None
        );
    }
}
