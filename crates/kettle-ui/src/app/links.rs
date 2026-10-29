use super::*;

pub(super) fn warn_refused_terminal_url(_uri: &str, warn: impl FnOnce(&str)) {
    warn("refused to open an unsafe URL from terminal output");
}

/// Whether an OSC 7 working directory is safe to turn into a `file://` URL and
/// hand to the OS opener.
///
/// The cwd comes from untrusted PTY output (`parse_osc7`
/// stores whatever a program reports), and a crafted `file://x//evil.host/share`
/// yields a stored cwd of `//evil.host/share`. Building `file://{cwd}` from that
/// produces a UNC-style URL that, when opened on Windows, connects to the
/// attacker over SMB and leaks the user's NTLM hash. The
/// `is_safe_url` locality check already rejects the resulting URL, but this is
/// the one call site that *constructs* `file://` from raw untrusted input, so
/// it gets an explicit local-only guard too (defense-in-depth). Crucially this
/// check does NO filesystem stat — `Path::is_dir` on a `//host` path would
/// itself route over SMB — it's a pure string check: reject UNC/authority
/// (`//`, leading `\`) and traversal.
pub(super) fn cwd_is_local(cwd: &str) -> bool {
    !cwd.is_empty() && !cwd.starts_with("//") && !cwd.starts_with('\\') && !cwd.contains("..")
}

/// What opening a local file link would do, decided on the opener thread.
#[derive(Debug, PartialEq)]
pub(super) enum FileLinkCheck {
    /// Open this resolved path: a document or a folder.
    Open(std::path::PathBuf),
    /// Refuse: it is a program or a shortcut. Holds the name to report.
    Program(String),
    /// Refuse quietly: it does not resolve to a local file or folder.
    Unresolved,
}

/// Whether opening the local file `path` would run a program or follow a
/// shortcut. Symlinks are resolved first, because the OS opener follows them:
/// `notes.md` linking to an application must not launch it. Refused when
/// either the name or the resolved name names a program or a shortcut (see
/// [`kettle_core::links::names_program_or_shortcut`]), when a folder is a
/// macOS bundle by its contents, when a file is a shortcut by its first bytes
/// (a macOS alias, which the opener follows whatever its name, or a Linux
/// launcher), or when an executable file lacks a known document extension:
/// macOS runs `a.out` in Terminal. A document whose executable bits are set,
/// as every file on a FAT or exFAT volume is, still opens.
///
/// This looks at an untrusted path, which can stall on a network mount, so
/// it runs off the UI thread. What it resolves is what opens, but a local
/// process that rewrites the file between the check and the open is not
/// stopped.
pub(super) fn check_file_link(path: &std::path::Path) -> FileLinkCheck {
    use kettle_core::links::names_program_or_shortcut;
    let Ok(resolved) = std::fs::canonicalize(path) else {
        return FileLinkCheck::Unresolved;
    };
    let Some(resolved) = local_resolved_path(resolved) else {
        return FileLinkCheck::Unresolved;
    };
    let Ok(metadata) = std::fs::metadata(&resolved) else {
        return FileLinkCheck::Unresolved;
    };
    let is_dir = metadata.is_dir();
    let program = names_program_or_shortcut(path, is_dir)
        || names_program_or_shortcut(&resolved, is_dir)
        || (is_dir && resolved.join("Contents").join("Info.plist").exists())
        || (metadata.is_file()
            && (starts_like_a_shortcut(&resolved)
                || (is_executable(&metadata) && !names_a_document(&resolved))));
    if program {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        FileLinkCheck::Program(name)
    } else {
        FileLinkCheck::Open(resolved)
    }
}

/// `resolved` as a path the opener may take: on Windows a canonical path is
/// a verbatim `\\?\C:\…`, kept as `C:\…`, while a share (`\\?\UNC\…`) or a
/// volume GUID is refused. Elsewhere, any absolute path.
pub(super) fn local_resolved_path(resolved: std::path::PathBuf) -> Option<std::path::PathBuf> {
    if !cfg!(windows) {
        return resolved.is_absolute().then_some(resolved);
    }
    let text = resolved.to_str()?;
    let drive = |path: &str| {
        let b = path.as_bytes();
        b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/')
    };
    let local = text.strip_prefix(r"\\?\").unwrap_or(text);
    (drive(local) && win32_keeps_path(local)).then(|| std::path::PathBuf::from(local))
}

/// Whether Win32 reads the drive path `local` as written once the verbatim
/// `\\?\` prefix is gone: no name ends with a dot or a space, which Win32
/// drops, and none is a reserved device name (`CON`, `NUL`, `COM1`…), which
/// Win32 opens as the device.
pub(super) fn win32_keeps_path(local: &str) -> bool {
    local.split(['\\', '/']).skip(1).all(|name| {
        let stem = name.split('.').next().unwrap_or(name).trim_end();
        let device = matches!(
            stem.to_ascii_uppercase().as_str(),
            "CON" | "PRN" | "AUX" | "NUL"
        ) || (stem.len() == 4
            && ["COM", "LPT"].iter().any(|prefix| {
                stem.get(..3)
                    .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
            })
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
        !name.ends_with(['.', ' ']) && !device
    })
}

/// Whether a file's mode lets it run (Unix); never on Windows, which runs by
/// extension.
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

/// Whether `path` has the extension of a common document, image, media,
/// archive, text or source file, which the OS opener shows in an app rather
/// than runs even when its executable bits are set.
fn names_a_document(path: &std::path::Path) -> bool {
    const DOCUMENTS: &[&str] = &[
        "pdf", "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "tif", "tiff", "heic", "ico",
        "psd", "mp4", "mov", "m4v", "mkv", "webm", "avi", "mp3", "m4a", "wav", "flac", "ogg",
        "opus", "aac", "txt", "md", "markdown", "rst", "adoc", "org", "tex", "html", "htm", "css",
        "json", "jsonl", "yaml", "yml", "toml", "xml", "csv", "tsv", "log", "rtf", "doc", "docx",
        "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "pages", "numbers", "key", "epub",
        "zip", "tar", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "c", "h", "cc", "cpp", "hpp",
        "rs", "go", "java", "kt", "swift", "ts", "tsx", "jsx", "sql", "ini", "conf", "cfg", "lock",
        "ipynb", "diff", "patch", "mmd", "dot",
    ];
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            DOCUMENTS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// Whether a file is a shortcut an opener follows: a macOS alias, by its
/// Finder flag (a classic alias keeps its data in the resource fork) or by
/// its bookmark data, or a Linux desktop launcher, whose first group, past
/// blank and comment lines, is `[Desktop Entry]`.
fn starts_like_a_shortcut(path: &std::path::Path) -> bool {
    use std::io::Read as _;
    if finder_alias_flag(path) {
        return true;
    }
    let mut head = Vec::with_capacity(4096);
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    if file.take(4096).read_to_end(&mut head).is_err() {
        return false;
    }
    if head.starts_with(b"book") && head.get(8..12) == Some(b"mark") {
        return true;
    }
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&head);
    head.split(|&byte| byte == b'\n')
        .map(<[u8]>::trim_ascii)
        .find(|line| !line.is_empty() && !line.starts_with(b"#"))
        .is_some_and(|group| {
            [
                &b"[Desktop Entry]"[..],
                b"[Desktop Action",
                b"[KDE Desktop Entry]",
            ]
            .iter()
            .any(|launcher| group.starts_with(launcher))
        })
}

/// Whether macOS marks `path` as a Finder alias (`kIsAlias` in the Finder
/// flags of its `com.apple.FinderInfo` attribute).
#[cfg(target_os = "macos")]
fn finder_alias_flag(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    const IS_ALIAS: u16 = 0x8000;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut info = [0u8; 32];
    // SAFETY: both names are NUL-terminated C strings that outlive the call,
    // and `info` is writable for the length passed.
    let read = unsafe {
        libc::getxattr(
            c_path.as_ptr(),
            c"com.apple.FinderInfo".as_ptr(),
            info.as_mut_ptr().cast(),
            info.len(),
            0,
            0,
        )
    };
    read >= 10 && u16::from_be_bytes([info[8], info[9]]) & IS_ALIAS != 0
}

#[cfg(not(target_os = "macos"))]
fn finder_alias_flag(_path: &std::path::Path) -> bool {
    false
}

/// At most this many file links are checked and opened at once; a stalled
/// network path cannot pile up threads click by click.
const MAX_FILE_LINK_OPENERS: usize = 4;
static FILE_LINK_OPENERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A slot among `limit` concurrent users of `counter`, released on drop, or
/// `None` when all are taken.
pub(super) fn take_opener_slot(
    counter: &'static std::sync::atomic::AtomicUsize,
    limit: usize,
) -> Option<OpenerSlot> {
    use std::sync::atomic::Ordering;
    // Rust 1.99 renames fetch_update to try_update; the MSRV (1.95) predates it.
    #[allow(deprecated)]
    let taken = counter.fetch_update(Ordering::AcqRel, Ordering::Acquire, |taken| {
        (taken < limit).then_some(taken + 1)
    });
    taken.ok().map(|_| OpenerSlot(counter))
}

pub(super) struct OpenerSlot(&'static std::sync::atomic::AtomicUsize);

impl Drop for OpenerSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// Check the local file link `path` and open what it resolves to, on its own
/// thread: with the custom URL handler, if one is set, given a file URL for
/// the resolved path, or with the system opener. A program or shortcut is
/// refused with a notification.
fn spawn_file_link_opener(
    path: std::path::PathBuf,
    handler: Option<String>,
    tr: kettle_i18n::Translator,
) {
    let Some(slot) = take_opener_slot(&FILE_LINK_OPENERS, MAX_FILE_LINK_OPENERS) else {
        log::warn!("too many file links are still opening; ignoring this one");
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("kettle-open-link".into())
        .spawn(move || {
            let _slot = slot;
            match check_file_link(&path) {
                FileLinkCheck::Program(name) => fire_notify(
                    tr.text(kettle_i18n::Text::NotifyTitleProgramNotOpened),
                    &tr.notify_body_program_not_opened(&name),
                ),
                FileLinkCheck::Unresolved => {
                    log::warn!("not opening a file link that names no local file or folder");
                    // Say so: from behind tmux this is usually a path an ssh
                    // inside it printed, which names a file on that machine.
                    let name = kettle_core::links::file_url_for_path(&path)
                        .map(|url| link_display_name(&url, 48))
                        .unwrap_or_default();
                    fire_notify(
                        tr.text(kettle_i18n::Text::NotifyTitleRemoteFileNotOpened),
                        &tr.notify_body_file_link_not_found(&name),
                    );
                }
                FileLinkCheck::Open(resolved) => open_checked_file(&resolved, handler.as_deref()),
            }
        });
    if let Err(e) = spawned {
        log::warn!("could not start the link opener: {e}");
    }
}

// Start the waiter before its child so a thread-spawn failure cannot lose a child handle.
pub(super) fn spawn_custom_url_handler(cmd: &str, target: &str) -> std::io::Result<()> {
    let cmd = cmd.to_string();
    let target = target.to_string();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("kettle-url-handler".into())
        .spawn(move || {
            match std::process::Command::new(&cmd)
                .arg(&target)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    let _ = tx.send(Ok(()));
                    if let Err(e) = child.wait() {
                        log::warn!("custom-url-handler {cmd:?}: could not reap child: {e}");
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            }
        })?;
    rx.recv().map_err(std::io::Error::other)?
}

/// Open a checked, resolved local file: through `handler` with a file URL
/// built from the resolved path, falling back to the system opener, which
/// takes the path itself. Neither sees the link's own text, so neither can
/// read a different path out of it.
pub(super) fn open_checked_file(resolved: &std::path::Path, handler: Option<&str>) {
    if let (Some(cmd), Some(url)) = (handler, kettle_core::links::file_url_for_path(resolved)) {
        match spawn_custom_url_handler(cmd, &url) {
            Ok(_) => return,
            Err(e) => {
                log::warn!("custom-url-handler {cmd:?}: {e}; opening the file with the system")
            }
        }
    }
    if let Err(e) = open::that_detached(resolved) {
        log::warn!("failed to open a terminal-supplied file link: {e}");
    }
}

/// Whether a file named `name` is one a lane can preview, by its extension:
/// the raster formats the media worker decodes, SVG, Mermaid, Markdown,
/// whose Mermaid diagrams are a gallery, PDF, and the common video
/// containers, shown as a poster (`.ts` is left out: it names TypeScript far
/// more often).
/// The worker still decides by the bytes; this only picks what to offer.
pub(super) fn previewable_name(name: &str) -> bool {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    matches!(
        extension.as_deref(),
        Some(
            "png"
                | "jpg"
                | "jpeg"
                | "webp"
                | "bmp"
                | "gif"
                | "tif"
                | "tiff"
                | "heic"
                | "heif"
                | "avif"
                | "svg"
                | "mmd"
                | "mermaid"
                | "md"
                | "markdown"
                | "pdf"
                | "mp4"
                | "m4v"
                | "mov"
                | "webm"
                | "mkv"
                | "avi"
                | "mpg"
                | "mpeg"
                | "m2ts"
                | "mts"
                | "ogv"
                | "wmv"
                | "flv"
        )
    )
}

/// Where a link handed to `App::open_url` came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LinkSource {
    /// Kettle's own link (the release page).
    Kettle,
    /// A pane's output, with that pane's origin (`None`: it has gone).
    Pane(Option<PathOrigin>),
}

/// What opening a link from a pane's output does (see `App::open_pane_link`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LinkGate {
    Open,
    /// A local file link from a remote pane, or one that has gone.
    Refuse,
}

/// Whether a link from a pane with `origin` (`None`: the pane has gone) may
/// open. Only a `file://` link names a local file; web and mail links open
/// wherever they were printed. A multiplexer's server runs on this computer,
/// so its panes' file links open as local ones do; a file an ssh inside it
/// named is usually missing here, which the opener reports.
pub(super) fn link_gate(uri: &str, origin: Option<PathOrigin>) -> LinkGate {
    let file_link = uri
        .get(..7)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file://"));
    match (file_link, origin) {
        (false, _) | (true, Some(PathOrigin::Local)) => LinkGate::Open,
        (true, Some(PathOrigin::Remote) | None) => LinkGate::Refuse,
    }
}

/// The name a prompt or notification shows for a file link: its file name,
/// or the link itself when it names none. The text is untrusted, so
/// characters that reorder or hide text (bidirectional controls, zero-width
/// and other format characters) show as `\u{FFFD}`, and a name wider than
/// `max_cols` columns keeps its start and end around an ellipsis, so the
/// warning beside it stays readable.
pub(super) fn link_display_name(uri: &str, max_cols: usize) -> String {
    use unicode_width::UnicodeWidthChar as _;
    let name = kettle_core::links::local_file_path(uri)
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| uri.to_string());
    let shown = name
        .chars()
        .map(|c| {
            let format = matches!(
                c,
                '\u{00AD}'
                    | '\u{061C}'
                    | '\u{180E}'
                    | '\u{200B}'..='\u{200F}'
                    | '\u{2028}'..='\u{202E}'
                    | '\u{2060}'..='\u{206F}'
                    | '\u{FEFF}'
                    | '\u{FFF9}'..='\u{FFFB}'
            );
            if c.is_control() || format {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect::<Vec<_>>();
    let width = |chars: &[char]| chars.iter().map(|c| c.width().unwrap_or(0)).sum::<usize>();
    if width(&shown) <= max_cols {
        return shown.into_iter().collect();
    }
    // Half the budget, less the ellipsis, from each end.
    let side = max_cols.saturating_sub(1) / 2;
    let mut head = 0;
    let mut used = 0;
    while let Some(c) = shown.get(head) {
        let w = c.width().unwrap_or(0);
        if used + w > side {
            break;
        }
        used += w;
        head += 1;
    }
    let mut tail = shown.len();
    used = 0;
    while tail > head {
        let w = shown[tail - 1].width().unwrap_or(0);
        if used + w > side {
            break;
        }
        used += w;
        tail -= 1;
    }
    shown[..head]
        .iter()
        .chain(['…'].iter())
        .chain(&shown[tail..])
        .collect()
}

/// The pane `ws.links` were scanned from: the focused pane at the last scan.
pub(super) fn links_pane(ws: &WindowState) -> Option<u64> {
    ws.links_scan_key.as_ref().and_then(|key| key.0.1)
}

/// Where the files a pane's output names live, as far as Kettle can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PathOrigin {
    /// This computer, including behind tmux, screen or zellij, whose server
    /// runs here.
    Local,
    /// Another machine or a container: a detected remote session, or a
    /// remote shell or container client in the pane.
    Remote,
}

/// Where a pane's printed paths live: remote when it runs a detected remote
/// session or a remote or container client, and local otherwise.
///
/// What runs now decides: the foreground program, shell scripts included
/// (`sh -c 'tmux attach'`). The command the pane was launched with counts
/// only when it is itself a client (`ssh host`, `env TERM=x ssh host`): a
/// launch script that ran ssh and then `exec bash` must not mark the pane
/// remote for good. Output a pane printed while it was remote stays on
/// screen after it returns local; links are gated by what runs when they are
/// opened.
pub(super) fn pane_path_origin(pane: &crate::mux::Pane) -> PathOrigin {
    path_origin(
        pane.remote_context.is_some() || argv_is_nonlocal_client(&pane.argv),
        &pane.argv,
        pane.foreground_process
            .as_ref()
            .map(|process| process.argv.as_slice()),
    )
}

/// [`pane_path_origin`] from its parts: whether a remote session is known,
/// the launch command and the foreground program's command line.
pub(super) fn path_origin(
    remote: bool,
    launch: &[String],
    foreground: Option<&[String]>,
) -> PathOrigin {
    let launch_client = argv_client_with(launch, 0, false);
    let foreground_client = foreground.and_then(|argv| argv_client(argv, 0));
    match (
        remote,
        any_client([launch_client, foreground_client].into_iter()),
    ) {
        (true, _) | (false, Some(Client::Remote)) => PathOrigin::Remote,
        (false, None) => PathOrigin::Local,
    }
}

/// What kind of client a command line runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Client {
    Remote,
}

/// The remote or container client `argv` runs, looking through a shell's
/// `-c` script, every command in it (`sh -c 'cd ~ && "ssh" host'`), and
/// through wrappers that run another command (`env TERM=xterm ssh host`,
/// `sudo -u me ssh host`, `nice -n 5 mosh host`). Nesting deeper than four
/// levels counts as remote: it fails closed rather than reading as local.
pub(super) fn argv_client(argv: &[String], depth: usize) -> Option<Client> {
    argv_client_with(argv, depth, true)
}

/// [`argv_client`], reading shell `-c` scripts only when `scripts` is set: a
/// pane's launch command counts only when it is itself a client, wrapped or
/// not, since a launch script may long since have moved on.
fn argv_client_with(argv: &[String], depth: usize, scripts: bool) -> Option<Client> {
    if depth > 4 {
        return Some(Client::Remote);
    }
    let argv0 = argv.first()?;
    let name = argv0
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(argv0)
        .to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    let rest = &argv[1..];
    match name {
        "ssh" | "autossh" | "mosh" | "mosh-client" | "et" | "telnet" | "docker" | "podman"
        | "kubectl" | "lxc-attach" => Some(Client::Remote),
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "fish" if !scripts => None,
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "fish" => {
            // The argument after a flag cluster holding `c` (`-c`, `-lc`).
            let flag = rest.iter().position(|arg| {
                arg.len() > 1
                    && arg.starts_with('-')
                    && !arg.starts_with("--")
                    && arg[1..].contains('c')
            })?;
            // A script past the parser's bounds fails closed.
            let Some(commands) = shell_commands(rest.get(flag + 1)?) else {
                return Some(Client::Remote);
            };
            any_client(commands.iter().map(|words| {
                let words = command_words(words);
                // A command the parser cannot name (`$cmd`, `` `cmd` ``,
                // `eval …`) fails closed.
                match words.first() {
                    Some(first) if first == "eval" || first.contains(['$', '`']) => {
                        Some(Client::Remote)
                    }
                    _ => argv_client(words, depth + 1),
                }
            }))
        }
        "env" | "nohup" | "nice" | "caffeinate" | "sudo" | "doas" | "time" | "sshpass" => {
            // Options that take a separate value, per wrapper.
            let takes_value: &[&str] = match name {
                "env" => &["-u", "-C", "--unset", "--chdir"],
                "nice" => &["-n", "--adjustment"],
                "caffeinate" => &["-t", "-w"],
                "sudo" => &[
                    "-u",
                    "-g",
                    "-h",
                    "-p",
                    "-C",
                    "-D",
                    "-r",
                    "-t",
                    "-U",
                    "-T",
                    "--user",
                    "--group",
                    "--host",
                    "--prompt",
                    "--close-from",
                    "--chdir",
                    "--role",
                    "--type",
                    "--other-user",
                    "--command-timeout",
                ],
                "doas" => &["-u", "-C", "-a"],
                "time" => &["-o", "-f", "--output", "--format"],
                "sshpass" => &["-p", "-f", "-d", "-P"],
                _ => &[],
            };
            let mut index = 0;
            while let Some(arg) = rest.get(index) {
                if arg == "--" {
                    index += 1;
                    break;
                } else if arg.starts_with('-') {
                    index += if takes_value.contains(&arg.as_str()) {
                        2
                    } else {
                        1
                    };
                } else if name == "env" && arg.contains('=') {
                    index += 1;
                } else {
                    break;
                }
            }
            // An option this table does not know (`--preserve-env X`) may
            // have taken the next word as its value: then the command starts
            // one word later.
            let uncertain = index
                .checked_sub(1)
                .and_then(|before| rest.get(before))
                .is_some_and(|option| {
                    option.starts_with('-')
                        && option != "--"
                        && !option.contains('=')
                        && !takes_value.contains(&option.as_str())
                });
            argv_client_with(rest.get(index..)?, depth + 1, scripts).or_else(|| {
                uncertain
                    .then(|| argv_client_with(rest.get(index + 1..)?, depth + 1, scripts))
                    .flatten()
            })
        }
        _ => None,
    }
}

/// The client any of `clients` runs.
fn any_client(mut clients: impl Iterator<Item = Option<Client>>) -> Option<Client> {
    clients.find_map(|client| client)
}

/// A command's words past leading redirections and assignments, the shell
/// keywords before a
/// command (`if`, `then`, `do`, `!`, `{`…), and the precommand words the shell
/// runs it through (`exec`, `command`, `builtin`, `nohup`, `noglob`,
/// `nocorrect`) with their options (`command -p`, `exec -a NAME`).
fn command_words(words: &[String]) -> &[String] {
    let mut index = 0;
    while let Some(word) = words.get(index) {
        // A redirection (`2>/dev/null`, `>out`, `&>log`), and its target when
        // it stands apart (`2> /dev/null`).
        let redirection = word.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
        if redirection.starts_with(['<', '>']) {
            let operator_only = redirection
                .trim_start_matches(['<', '>', '&', '|'])
                .is_empty();
            index += if operator_only { 2 } else { 1 };
        } else if word.contains('=') && !word.starts_with('=') && !word.starts_with('-') {
            index += 1;
        } else if matches!(
            word.as_str(),
            "if" | "then"
                | "else"
                | "elif"
                | "do"
                | "while"
                | "until"
                | "!"
                | "{"
                | "not"
                | "and"
                | "or"
                | "begin"
        ) {
            // Shell keywords that come before a command, fish's included.
            index += 1;
        } else if matches!(
            word.as_str(),
            "exec" | "command" | "builtin" | "nohup" | "noglob" | "nocorrect"
        ) {
            let precommand = word.as_str();
            index += 1;
            while let Some(option) = words.get(index).filter(|option| option.starts_with('-')) {
                // `command -v ssh` names a program without running it.
                if precommand == "command" && option.contains(['v', 'V']) {
                    return &[];
                }
                index += if precommand == "exec" && option == "-a" {
                    2
                } else {
                    1
                };
                if option == "--" {
                    break;
                }
            }
        } else {
            break;
        }
    }
    words.get(index..).unwrap_or_default()
}

/// The simple commands in a shell script, as words with their quotes
/// removed: `;`, `&`, `|`, newlines and parentheses separate commands, and
/// single quotes, double quotes and backslashes quote. Command substitutions
/// (`$(…)`, `` `…` ``) are commands too, nested at most four deep; in command
/// position one reads as `$SUBST`, which a caller cannot name. `None` for a
/// script past the bounds (4 KiB, 16 commands, the nesting) or with a
/// substitution left open, which a caller must not read as holding no
/// client.
fn shell_commands(script: &str) -> Option<Vec<Vec<String>>> {
    shell_commands_at(script, 0)
}

fn shell_commands_at(script: &str, depth: usize) -> Option<Vec<Vec<String>>> {
    if script.len() > 4096 || depth > 4 {
        return None;
    }
    let (outer, substitutions) = split_substitutions(script)?;
    let mut commands = split_commands(&outer)?;
    for substitution in substitutions {
        commands.extend(shell_commands_at(&substitution, depth + 1)?);
        if commands.len() > 16 {
            return None;
        }
    }
    Some(commands)
}

/// `script` with each command substitution outside single quotes replaced by
/// `$SUBST`, and the substitutions' own text. `None` for one left open.
fn split_substitutions(script: &str) -> Option<(String, Vec<String>)> {
    let mut outer = String::with_capacity(script.len());
    let mut substitutions = Vec::new();
    let mut chars = script.chars().peekable();
    let (mut single, mut double) = (false, false);
    while let Some(c) = chars.next() {
        match c {
            '\\' if !single => {
                outer.push(c);
                outer.extend(chars.next());
            }
            '\'' if !double => {
                single = !single;
                outer.push(c);
            }
            '"' if !single => {
                double = !double;
                outer.push(c);
            }
            '$' if !single && chars.peek() == Some(&'(') => {
                chars.next();
                let mut depth = 1usize;
                let mut body = String::new();
                // Parentheses inside quotes do not count (`$(printf ')')`).
                let (mut inner_single, mut inner_double) = (false, false);
                loop {
                    let c = chars.next()?;
                    match c {
                        '\\' if !inner_single => {
                            body.push(c);
                            body.extend(chars.next());
                            continue;
                        }
                        '\'' if !inner_double => inner_single = !inner_single,
                        '"' if !inner_single => inner_double = !inner_double,
                        '(' if !inner_single && !inner_double => depth += 1,
                        ')' if !inner_single && !inner_double => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        break;
                    }
                    body.push(c);
                }
                substitutions.push(body);
                outer.push_str("$SUBST");
            }
            '`' if !single => {
                let mut body = String::new();
                loop {
                    match chars.next()? {
                        '`' => break,
                        '\\' => body.extend(chars.next()),
                        c => body.push(c),
                    }
                }
                substitutions.push(body);
                outer.push_str("$SUBST");
            }
            c => outer.push(c),
        }
    }
    Some((outer, substitutions))
}

/// The simple commands in a script with no substitutions left (see
/// [`shell_commands`]).
fn split_commands(script: &str) -> Option<Vec<Vec<String>>> {
    let mut commands = vec![Vec::new()];
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = script.chars();
    let end_word = |word: &mut String, in_word: &mut bool, commands: &mut Vec<Vec<String>>| {
        if *in_word {
            commands
                .last_mut()
                .expect("a command")
                .push(std::mem::take(word));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                word.extend(chars.by_ref().take_while(|&c| c != '\''));
            }
            '"' => {
                in_word = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        // A backslash-newline continues the line.
                        '\\' => match chars.next() {
                            Some('\n') | None => {}
                            Some(c) => word.push(c),
                        },
                        c => word.push(c),
                    }
                }
            }
            '\\' => match chars.next() {
                // A backslash-newline continues the word.
                Some('\n') | None => {}
                Some(c) => {
                    in_word = true;
                    word.push(c);
                }
            },
            // A redirection starts a word of its own (`tmux</dev/tty`),
            // keeping a file descriptor before it (`2>`).
            '<' | '>' if in_word && !word.chars().all(|c| c.is_ascii_digit() || c == '&') => {
                end_word(&mut word, &mut in_word, &mut commands);
                in_word = true;
                word.push(c);
            }
            // `2>&1` and `&>file` are redirections, not command separators.
            '&' if word.ends_with(['>', '<']) || chars.clone().next() == Some('>') => {
                in_word = true;
                word.push(c);
            }
            ';' | '&' | '|' | '\n' | '(' | ')' => {
                end_word(&mut word, &mut in_word, &mut commands);
                if commands.last().is_some_and(|command| !command.is_empty()) {
                    if commands.len() == 16 {
                        return None;
                    }
                    commands.push(Vec::new());
                }
            }
            c if c.is_whitespace() => end_word(&mut word, &mut in_word, &mut commands),
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    end_word(&mut word, &mut in_word, &mut commands);
    commands.retain(|command| !command.is_empty());
    Some(commands)
}

/// The local `file://` URL a path hint names: `~/` resolved from `home`, a
/// relative path from the pane's `cwd`, `.` segments dropped. This touches no
/// filesystem: the path and an OSC 7 `cwd` are untrusted terminal text, and a
/// stat would let them reach a network share or an automount (`/net/host`),
/// leaking the user's address and stalling the UI thread. The OS opener, run
/// detached, resolves symlinks. `None`, so the hint is copied instead, when
/// the base is unknown or not local, or the path climbs with `..`, whose
/// meaning after a symlink only the filesystem knows; always on Windows,
/// where a link to a `\\host\share` would contact that host.
pub(super) fn hint_path_url(text: &str, cwd: Option<&str>, home: Option<&str>) -> Option<String> {
    use std::path::{Component, Path, PathBuf};
    if cfg!(windows) {
        return None;
    }
    let local = |base: &str| cwd_is_local(base).then(|| Path::new(base).to_path_buf());
    let joined = if let Some(rest) = text.strip_prefix("~/") {
        local(home?)?.join(rest)
    } else if text.starts_with('/') {
        PathBuf::from(text)
    } else if text.as_bytes().get(1) == Some(&b':') {
        // A drive-letter path names nothing local off Windows.
        return None;
    } else {
        local(cwd?)?.join(text)
    };
    let mut resolved = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => return None,
            other => resolved.push(other),
        }
    }
    let text = resolved.to_str()?;
    if !cwd_is_local(text) || !resolved.is_absolute() {
        return None;
    }
    // Percent-encode everything but the characters a file URL's path keeps.
    let mut url = String::from("file://");
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            url.push(char::from(byte));
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    Some(url)
}
/// Cache key for the viewport link re-scan: `(focus, focused pane's
/// output_generation, its geometry_generation, scroll display_offset,
/// focused cwd)`. It uses the focused pane's generation, not the tab's
/// `last_output_at`, because the activity latch updates that only for
/// background tabs, so active-tab output would never invalidate the scan. The
/// geometry generation covers a resize, which reflows text with no output:
/// zooming a pane, splitting next to it, or resizing the window.
pub(crate) type LinksScanKey = (
    FocusKey,
    Option<u64>,
    Option<u64>,
    Option<usize>,
    Option<String>,
);

/// Re-scan the focused pane's visible links into `ws.links` unless the key
/// shows nothing they depend on has changed. The brief lock to read
/// display_offset is cheap; the avoided work is `kettle_core::links`'
/// per-cell regex pass.
pub(super) fn scan_links(ws: &mut WindowState) {
    let key: LinksScanKey = {
        let focus = (ws.mux.active, ws.mux.active_focus());
        let focused = ws.mux.focused().map(|pane| &*pane);
        (
            focus,
            // Cheap atomic and short-lock reads.
            focused.map(|p| p.term.output_generation()),
            focused.map(|p| p.term.geometry_generation()),
            focused.and_then(|p| p.term.term.lock().ok().map(|t| t.grid().display_offset())),
            focused.and_then(|p| p.term.current_dir_or_native()),
        )
    };
    if ws.links_scan_key.as_ref() == Some(&key) {
        return;
    }
    // Focused interactive apps like `less -R` redraw visible text by writing
    // output, not by changing terminal scrollback display_offset, and a
    // resize reflows it with no output at all. Re-scan on either so path and
    // URL underline rects stay attached to the current text.
    ws.links = ws
        .mux
        .focused()
        .and_then(|p| {
            p.term
                .term
                .lock()
                .ok()
                .map(|t| kettle_core::links::links_with_cwd(&t, key.4.as_deref()))
        })
        .unwrap_or_default();
    ws.links_scan_key = Some(key);
}

impl App {
    /// Open a link taken from pane `pane`'s output. A local file link from a
    /// remote pane is refused with a notification, since the path names a file
    /// on that machine; a pane that has gone counts as remote. Other links open
    /// through [`Self::open_url`].
    pub(super) fn open_pane_link(&mut self, ws: &mut WindowState, pane: Option<u64>, uri: &str) {
        let normalized = kettle_core::links::normalize_url(uri);
        let uri = normalized.as_str();
        let origin = pane
            .and_then(|id| ws.mux.panes.get(&id))
            .map(pane_path_origin);
        let tr = self.ui_text;
        match link_gate(uri, origin) {
            LinkGate::Open => self.open_url(ws, uri, LinkSource::Pane(origin)),
            LinkGate::Refuse => fire_notify(
                tr.text(kettle_i18n::Text::NotifyTitleRemoteFileNotOpened),
                &tr.notify_body_remote_file_not_opened(&link_display_name(uri, 48)),
            ),
        }
    }

    /// Open a URL. Unsafe URLs are refused, and Lua URL handlers may claim or
    /// rewrite it first. Then, for Terminator parity
    /// (terminatorlib/config.py:86-87 `use_custom_url_handler` +
    /// `custom_url_handler`), open it via the custom external program if
    /// configured and non-empty, else via the cross-platform `open` crate.
    /// The custom program is invoked as
    ///   <custom_url_handler> <uri>
    /// detached, so kettle doesn't block on the handler exiting.
    /// Errors log::warn.
    pub(super) fn open_url(&mut self, ws: &WindowState, uri: &str, source: LinkSource) {
        if !kettle_core::links::is_safe_url(uri) {
            warn_refused_terminal_url(uri, |message| log::warn!("{message}"));
            return;
        }
        // Terminator plugin parity: fire
        // `LuaEvent::UrlClicked`. Fired before pattern-handler
        // dispatch so analytics / logging / workflow-trigger
        // plugins see ALL URL clicks, regardless of which
        // handler eventually opens them. The
        // `try_url_handler` chain below still owns the "actually
        // launch" decision; this event is observation-only.
        self.fire_lua_event(
            ws,
            crate::LuaEvent::UrlClicked(uri.to_string()),
            "url_clicked hook",
        );
        // Terminator plugin parity: Lua URL handlers get first dispatch. A
        // handler that claims the URL either handles it itself or returns a
        // URL for the custom-handler / system-open paths below.
        let outcome = self
            .lua_engine
            .as_ref()
            .map(|engine| engine.try_url_handler(uri))
            .unwrap_or(crate::lua::UrlHandlerOutcome::Fallthrough);
        // URL-handler callbacks are distinct from UrlClicked observers and can
        // queue the same side effects. Drain before any claimed-handler return.
        self.drain_lua_hook_commands(ws, "URL handler");
        // A handler may REWRITE the URL — that is the documented contract in
        // `docs/examples/init.lua` and the shape every example there uses.
        let rewritten;
        let uri = match &outcome {
            crate::lua::UrlHandlerOutcome::Handled => return,
            crate::lua::UrlHandlerOutcome::Open(target) => {
                // Re-check the allowlist. The handler is the user's own code,
                // but it is fed untrusted terminal text, so it must not be a
                // way to turn that text into a URL kettle would have refused.
                if !kettle_core::links::is_safe_url(target) {
                    log::warn!("lua url_handler returned an unsafe URL; refusing to open it");
                    return;
                }
                // A handler may turn a web link from a remote pane into a local
                // file link, or a confirmed file into another, so the pane's
                // gate applies to any different link it returns.
                if let LinkSource::Pane(origin) = source
                    && target != uri
                    && link_gate(target, origin) != LinkGate::Open
                {
                    log::warn!(
                        "lua url_handler returned a local file link for a pane whose files \
                         may be elsewhere; refusing to open it"
                    );
                    return;
                }
                rewritten = target.clone();
                rewritten.as_str()
            }
            crate::lua::UrlHandlerOutcome::Fallthrough => uri,
        };
        // A local file link is checked before anything opens it, the custom
        // handler included: a program or a shortcut is refused, since opening
        // it would run it or follow it elsewhere. One that does not decode to
        // a local path is refused here. The check looks at an untrusted path,
        // which can stall on a network mount, so it and the open run off the
        // UI thread.
        if uri
            .get(..7)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file://"))
        {
            let Some(path) = kettle_core::links::local_file_path(uri) else {
                log::warn!("refusing a file link that names no local path");
                return;
            };
            let handler = (self.cfg.use_custom_url_handler
                && !self.cfg.custom_url_handler.is_empty())
            .then(|| self.cfg.custom_url_handler.clone());
            spawn_file_link_opener(path, handler, self.ui_text);
            return;
        }
        if self.cfg.use_custom_url_handler && !self.cfg.custom_url_handler.is_empty() {
            let cmd = self.cfg.custom_url_handler.clone();
            system_opener::open_custom(&cmd, uri);
            return;
        }
        system_opener::open(uri, |e| {
            log::warn!("failed to open a terminal-supplied URL: {e}");
        });
    }

    pub(super) fn update_links(&mut self, ws: &mut WindowState) {
        scan_links(ws);
    }
}
