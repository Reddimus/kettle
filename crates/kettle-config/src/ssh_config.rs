//! Host aliases from the user's OpenSSH client config, for the SSH launcher.
//!
//! Only `Host` lines are read, and only literal aliases that pass an ASCII
//! allowlist are kept. Every other keyword (`HostName`, `User`,
//! `IdentityFile`, ...) is skipped unparsed, `Include` is counted but not
//! followed, and `Match` blocks contribute nothing. The launcher reads the file
//! when it opens; nothing here is persisted or logged beyond a fixed reason.
//!
//! The same module holds the validators that the `ssh-host` config key, the
//! launcher and the spawn path share, so a target is judged by one rule.

use std::ffi::OsString;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// Most aliases imported from one file; later ones set `truncated`.
pub const SSH_ALIAS_CAP: usize = 256;
/// Longest alias kept, in bytes.
pub const SSH_ALIAS_MAX_BYTES: usize = 255;
/// Bytes read from the file. A longer file is parsed up to its last complete
/// line within this prefix and reported as `truncated`.
pub const SSH_CONFIG_MAX_BYTES: u64 = 256 * 1024;
/// Longest launcher target, in bytes.
pub const SSH_TARGET_MAX_BYTES: usize = 512;
/// Longest `ssh-host` name, in bytes.
pub const SSH_HOST_NAME_MAX_BYTES: usize = 128;

/// How the launcher names the imported file. A fixed string, never the
/// expanded home directory, so a screenshot does not show the user name.
pub const SSH_CONFIG_DISPLAY_PATH: &str = if cfg!(windows) {
    r"~\.ssh\config"
} else {
    "~/.ssh/config"
};

/// The literal aliases found in an `ssh_config(5)` text.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SshAliases {
    /// Distinct aliases in file order. Two spellings that differ only in ASCII
    /// case are one alias, as OpenSSH matches them; the first spelling stays.
    pub aliases: Vec<String>,
    /// More than [`SSH_ALIAS_CAP`] aliases, or more than
    /// [`SSH_CONFIG_MAX_BYTES`] of file, were present.
    pub truncated: bool,
    /// `Include` lines seen and not followed.
    pub includes_skipped: usize,
}

/// Why an existing file was not imported. Every variant displays as a fixed
/// phrase: no path, alias or `io::Error` text, so it is safe to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshImportSkip {
    /// The trusted reader answered `InvalidInput`: the path could not be
    /// resolved to a regular file. That covers a directory, FIFO or device,
    /// and also a link chain over its limits or with an unusable target.
    NotRegular,
    /// The trusted reader answered `PermissionDenied`: the file or a folder
    /// above it was refused. That covers a file, link or folder that someone
    /// other than the user, root or an administrator can change, a file with
    /// more than one hard link, a path swapped while it was opened, and a
    /// file the user may not read at all.
    InsecurePermissions,
    /// Any other failure, by kind only.
    Io(std::io::ErrorKind),
}

impl std::fmt::Display for SshImportSkip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRegular => f.write_str("the path could not be resolved to a regular file"),
            Self::InsecurePermissions => f.write_str("the file or a folder above it was refused"),
            Self::Io(kind) => write!(f, "read failed ({kind:?})"),
        }
    }
}

/// Unicode bidirectional formatting and override code points. They are not
/// `char::is_control()` (general category Cf), but they reorder text on
/// screen, so titles and launcher targets must not carry them.
pub fn is_bidi_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}'      // LRE RLE PDF LRO RLO
            | '\u{2066}'..='\u{2069}' // LRI RLI FSI PDI
            | '\u{200E}' | '\u{200F}' // LRM RLM
            | '\u{061C}' // ALM
    )
}

/// A destination the launcher may pass to `ssh` after `--`: not empty, at
/// most [`SSH_TARGET_MAX_BYTES`], not starting with `-`, and free of
/// whitespace, control and bidi formatting characters. A target with a space
/// is one argv element that ssh rejects as a hostname anyway.
pub fn is_valid_ssh_target(target: &str) -> bool {
    !target.is_empty()
        && target.len() <= SSH_TARGET_MAX_BYTES
        && !target.starts_with('-')
        && !target
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || is_bidi_format_char(c))
}

/// A launcher label from `ssh-host = NAME=target`: equal to its own trim, 1 to
/// [`SSH_HOST_NAME_MAX_BYTES`] bytes, no `=`, no control or bidi formatting
/// characters. Interior spaces are allowed; names are labels, not arguments.
pub fn is_valid_ssh_host_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= SSH_HOST_NAME_MAX_BYTES
        && name.trim() == name
        && !name.contains('=')
        && !name
            .chars()
            .any(|c| c.is_control() || is_bidi_format_char(c))
}

/// A `Host` token kept as a launcher row: 1 to [`SSH_ALIAS_MAX_BYTES`] bytes
/// of `[A-Za-z0-9._~+:-]`, not starting with `-`. This rejects pattern syntax
/// (`* ? ! ,`), quotes, `%`, `@` (ssh would split off a user, so the alias is
/// unreachable), whitespace, controls and all non-ASCII text.
fn is_literal_alias(token: &str) -> bool {
    (1..=SSH_ALIAS_MAX_BYTES).contains(&token.len())
        && !token.starts_with('-')
        && token.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'+' | b':' | b'-')
        })
}

/// Split a keyword's arguments as OpenSSH's `argv_split` does: whitespace
/// separates, single or double quotes group, a backslash escapes a quote, a
/// backslash or a space, and an unquoted `#` at a token's start ends the line.
/// `None` for unbalanced quotes, which OpenSSH rejects as a line.
fn split_arguments(arguments: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut chars = arguments.chars().peekable();
    loop {
        while chars.next_if(|c| matches!(c, ' ' | '\t')).is_some() {}
        match chars.peek() {
            None | Some('#') => return Some(tokens),
            Some(_) => {}
        }
        let mut token = String::new();
        let mut quote = None;
        while let Some(c) = chars.next() {
            match (quote, c) {
                (None, ' ' | '\t') => break,
                (None, '"' | '\'') => quote = Some(c),
                (Some(open), c) if c == open => quote = None,
                (_, '\\') => match chars.peek().copied() {
                    Some(next @ ('\\' | '"' | '\'' | ' ')) => {
                        chars.next();
                        token.push(next);
                    }
                    _ => token.push('\\'),
                },
                (_, c) => token.push(c),
            }
        }
        if quote.is_some() {
            return None;
        }
        tokens.push(token);
    }
}

/// The literal `Host` aliases in `text`, an `ssh_config(5)` file.
///
/// Pure. Keywords are case-insensitive and separated from their arguments by
/// whitespace or one `=`. A token that is not a literal alias is skipped on
/// its own, so `Host prod !old` keeps `prod`.
pub fn host_aliases(text: &str) -> SshAliases {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = SshAliases::default();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let line = line.trim_start_matches([' ', '\t']);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let end = line.find([' ', '\t', '=']).unwrap_or(line.len());
        let (keyword, rest) = line.split_at(end);
        let rest = rest.trim_start_matches([' ', '\t']);
        let rest = rest.strip_prefix('=').unwrap_or(rest);
        if keyword.eq_ignore_ascii_case("include") {
            out.includes_skipped += 1;
            continue;
        }
        if !keyword.eq_ignore_ascii_case("host") {
            continue;
        }
        let Some(tokens) = split_arguments(rest) else {
            continue;
        };
        for token in tokens {
            if !is_literal_alias(&token)
                || out
                    .aliases
                    .iter()
                    .any(|seen| seen.eq_ignore_ascii_case(&token))
            {
                continue;
            }
            if out.aliases.len() == SSH_ALIAS_CAP {
                out.truncated = true;
                continue;
            }
            out.aliases.push(token);
        }
    }
    out
}

/// `~/.ssh/config` from the environment: `HOME` on Unix and `USERPROFILE` on
/// Windows, the file Win32-OpenSSH reads. A `HOME` exported by Git Bash or
/// MSYS is ignored on Windows, as `Config::default_path_from` ignores it.
/// Empty or non-absolute values give `None`. `windows` is a parameter, and
/// absoluteness is judged by that platform's rule rather than the host's, so
/// both branches are tested on every runner.
pub fn ssh_config_path_from(
    lookup: impl Fn(&str) -> Option<OsString>,
    windows: bool,
) -> Option<PathBuf> {
    let home = lookup(if windows { "USERPROFILE" } else { "HOME" })?;
    let bytes = home.as_encoded_bytes();
    let absolute = if windows {
        let drive = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/');
        drive || bytes.starts_with(br"\\")
    } else {
        bytes.starts_with(b"/")
    };
    absolute.then(|| PathBuf::from(home).join(".ssh").join("config"))
}

/// The current user's `~/.ssh/config` path, from the process environment.
pub fn user_ssh_config_path() -> Option<PathBuf> {
    ssh_config_path_from(|key| std::env::var_os(key), cfg!(windows))
}

/// Read and parse the aliases in the file at `path`.
///
/// The open goes through `kettle_state::open_trusted_file_read_following_leaf`,
/// the reader the main config uses: it follows a dotfile-manager symlink only
/// through trusted folders, never blocks on a FIFO, and refuses a file others
/// can change (DACLs on Windows, mode and ACLs on Unix) or one with several
/// hard links. That is stricter than OpenSSH, and fails closed.
///
/// A missing file, or a dangling link to one, is `Ok` with no aliases: not
/// having the file is the normal case. At most [`SSH_CONFIG_MAX_BYTES`] are
/// read; a longer file is parsed up to its last complete line in that prefix.
pub fn read_host_aliases(path: &Path) -> Result<SshAliases, SshImportSkip> {
    use std::io::ErrorKind;

    let file = match kettle_state::open_trusted_file_read_following_leaf(path) {
        Ok((file, _)) => file,
        Err(error) => {
            return match error.kind() {
                ErrorKind::NotFound => Ok(SshAliases::default()),
                ErrorKind::InvalidInput => Err(SshImportSkip::NotRegular),
                ErrorKind::PermissionDenied => Err(SshImportSkip::InsecurePermissions),
                kind => Err(SshImportSkip::Io(kind)),
            };
        }
    };
    let mut bytes = Vec::new();
    file.take(SSH_CONFIG_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| SshImportSkip::Io(error.kind()))?;
    let cut = bytes.len() as u64 > SSH_CONFIG_MAX_BYTES;
    if cut {
        bytes.truncate(SSH_CONFIG_MAX_BYTES as usize);
        // The last line may have been cut mid-token; drop it.
        let complete = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        bytes.truncate(complete);
    }
    let mut aliases = host_aliases(&String::from_utf8_lossy(&bytes));
    aliases.truncated |= cut;
    Ok(aliases)
}

/// The aliases for the launcher, from the file at `path`. A skipped file
/// yields none. The log lines carry a fixed reason only, never the path, an
/// alias or the `io::Error` text.
pub fn user_ssh_host_aliases(path: &Path) -> SshAliases {
    match read_host_aliases(path) {
        Ok(aliases) => {
            if aliases.truncated {
                log::info!("ssh config import kept a bounded prefix of the Host aliases");
            }
            aliases
        }
        Err(reason) => {
            log::warn!("ssh config import skipped: {reason}");
            SshAliases::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aliases(text: &str) -> Vec<String> {
        host_aliases(text).aliases
    }

    #[test]
    fn host_lines_parse_in_every_keyword_spelling() {
        assert_eq!(
            aliases("Host a b\nHost=eq\nhost = sp\nHOST Up\n\tHost\ttabbed\n"),
            ["a", "b", "eq", "sp", "Up", "tabbed"]
        );
        // `Hostname` and other keywords that start like `Host` are not hosts.
        assert_eq!(
            aliases("Hostname real.example\nHostKeyAlias x\n"),
            Vec::<String>::new()
        );
        assert!(aliases("Host\n").is_empty());
    }

    #[test]
    fn only_literal_aliases_survive_the_allowlist() {
        // Patterns, negations and lists.
        for skipped in ["*", "*.corp", "!bad", "a?c", "a,b"] {
            assert!(
                aliases(&format!("Host {skipped}\n")).is_empty(),
                "{skipped}"
            );
        }
        assert_eq!(aliases("Host prod !bad\n"), ["prod"]);
        assert!(aliases("Host * !x\n").is_empty());
        // Injection, spaces, bidi, user@host, non-ASCII, overlong.
        let long = "a".repeat(256);
        for rejected in [
            "-oProxyCommand=x",
            "\"a b\"",
            "a\u{202E}b",
            "a@b",
            "é",
            "%h",
            long.as_str(),
        ] {
            assert!(
                aliases(&format!("Host {rejected}\n")).is_empty(),
                "{rejected:?} must not become a launcher row"
            );
        }
        // A quoted or escaped group is one token, so its words never leak out.
        assert!(aliases("Host \"a prod b\"\n").is_empty());
        assert!(aliases("Host a\\ prod\n").is_empty());
        assert!(aliases("Host \"unterminated prod\n").is_empty());
        // A wholly quoted literal is unquoted.
        assert_eq!(aliases("Host \"quoted\"\n"), ["quoted"]);
        // The boundary: 255 bytes, `_` and the other allowed punctuation.
        let max = "b".repeat(255);
        assert_eq!(aliases(&format!("Host {max}\n")), [max]);
        assert_eq!(
            aliases("Host my_box web.example:22 a~b c+d\n"),
            ["my_box", "web.example:22", "a~b", "c+d"]
        );
    }

    #[test]
    fn match_blocks_includes_comments_and_line_endings() {
        let parsed = host_aliases(
            "\u{feff}# leading comment\r\n\
             Include config.d/*\r\n\
             Match host foo exec \"true\"\r\n\
             \x20 HostName match.example\r\n\
             \x20 User someone\r\n\
             Host after-match # trailing comment\r\n\
             include other\n\
             Host last\n",
        );
        assert_eq!(parsed.aliases, ["after-match", "last"]);
        assert_eq!(parsed.includes_skipped, 2);
        assert!(!parsed.truncated);
    }

    #[test]
    fn duplicates_fold_ascii_case_and_keep_the_first_spelling() {
        assert_eq!(
            aliases("Host Prod prod\nHost PROD other\n"),
            ["Prod", "other"]
        );
        assert_eq!(aliases("Host b a\nHost b\n"), ["b", "a"]);
    }

    #[test]
    fn alias_count_is_capped() {
        let text: String = (0..300).map(|i| format!("Host h{i}\n")).collect();
        let parsed = host_aliases(&text);
        assert_eq!(parsed.aliases.len(), SSH_ALIAS_CAP);
        assert_eq!(parsed.aliases[255], "h255");
        assert!(parsed.truncated);
    }

    #[test]
    fn targets_and_names_are_validated() {
        for good in [
            "box",
            "me@box.example",
            "me@[::1]:22",
            "ssh://me@h:2222",
            "a=b",
        ] {
            assert!(is_valid_ssh_target(good), "{good}");
        }
        let long = "a".repeat(SSH_TARGET_MAX_BYTES + 1);
        for bad in [
            "",
            "-oProxyCommand=x",
            "-",
            "me@h -p 22",
            "a\tb",
            "x\u{7}",
            "me@\u{202E}x",
            "a\u{2066}b",
            "a\u{a0}b",
            long.as_str(),
        ] {
            assert!(!is_valid_ssh_target(bad), "{bad:?}");
        }
        assert!(is_valid_ssh_target(&"a".repeat(SSH_TARGET_MAX_BYTES)));

        for good in ["box", "my box", "Café", "-dash"] {
            assert!(is_valid_ssh_host_name(good), "{good}");
        }
        let long = "n".repeat(SSH_HOST_NAME_MAX_BYTES + 1);
        for bad in [
            "",
            " box",
            "box ",
            "a=b",
            "a\nb",
            "a\u{202E}b",
            long.as_str(),
        ] {
            assert!(!is_valid_ssh_host_name(bad), "{bad:?}");
        }
        assert!(is_valid_ssh_host_name(&"n".repeat(SSH_HOST_NAME_MAX_BYTES)));
    }

    #[test]
    fn config_path_follows_each_platforms_home_variable() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| OsString::from(*v))
            }
        };
        let both = env(&[("HOME", "/home/me"), ("USERPROFILE", r"C:\Users\me")]);
        assert_eq!(
            ssh_config_path_from(both, false),
            Some(PathBuf::from("/home/me").join(".ssh").join("config"))
        );
        assert_eq!(
            ssh_config_path_from(both, true),
            Some(PathBuf::from(r"C:\Users\me").join(".ssh").join("config")),
            "Windows reads USERPROFILE and ignores a Git Bash HOME"
        );
        assert_eq!(
            ssh_config_path_from(env(&[("HOME", "/home/me")]), true),
            None
        );
        assert_eq!(
            ssh_config_path_from(env(&[("USERPROFILE", r"\\server\share\me")]), true),
            Some(
                PathBuf::from(r"\\server\share\me")
                    .join(".ssh")
                    .join("config")
            )
        );
        for (key, value, windows) in [
            ("HOME", "", false),
            ("HOME", "relative/home", false),
            ("HOME", r"C:\Users\me", false),
            ("USERPROFILE", "", true),
            ("USERPROFILE", r"Users\me", true),
            ("USERPROFILE", "C:", true),
            ("USERPROFILE", "/home/me", true),
        ] {
            let lookup = move |k: &str| (k == key).then(|| OsString::from(value));
            assert_eq!(
                ssh_config_path_from(lookup, windows),
                None,
                "{key}={value:?}"
            );
        }
    }

    #[test]
    fn skip_reasons_are_fixed_phrases() {
        for skip in [
            SshImportSkip::NotRegular,
            SshImportSkip::InsecurePermissions,
            SshImportSkip::Io(std::io::ErrorKind::NotADirectory),
        ] {
            let text = skip.to_string();
            assert!(!text.contains('/') && !text.contains('\\'), "{text}");
        }
    }

    mod reader {
        use super::super::*;
        use std::sync::{Mutex, OnceLock};

        /// Log lines this module emitted, by thread, so concurrent tests do
        /// not see each other's lines.
        struct Capture(Mutex<Vec<(std::thread::ThreadId, String)>>);

        impl log::Log for Capture {
            fn enabled(&self, _: &log::Metadata<'_>) -> bool {
                true
            }
            fn log(&self, record: &log::Record<'_>) {
                if record
                    .target()
                    .starts_with(module_path!().trim_end_matches("::tests::reader"))
                {
                    self.0
                        .lock()
                        .unwrap()
                        .push((std::thread::current().id(), record.args().to_string()));
                }
            }
            fn flush(&self) {}
        }

        fn capture() -> &'static Capture {
            static CAPTURE: OnceLock<&'static Capture> = OnceLock::new();
            CAPTURE.get_or_init(|| {
                let capture: &'static Capture =
                    Box::leak(Box::new(Capture(Mutex::new(Vec::new()))));
                log::set_logger(capture).expect("no other logger in this test binary");
                log::set_max_level(log::LevelFilter::Trace);
                capture
            })
        }

        /// Import through the logging wrapper and return this thread's lines.
        fn import_logged(path: &Path) -> (SshAliases, Vec<String>) {
            let capture = capture();
            let me = std::thread::current().id();
            capture
                .0
                .lock()
                .unwrap()
                .retain(|(thread, _)| *thread != me);
            let aliases = user_ssh_host_aliases(path);
            let lines = capture
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|(thread, _)| *thread == me)
                .map(|(_, line)| line.clone())
                .collect();
            (aliases, lines)
        }

        fn assert_private(lines: &[String], home: &Path, needles: &[&str]) {
            let home = home.to_string_lossy();
            for line in lines {
                assert!(
                    !line.contains(home.as_ref()),
                    "a log line names the path: {line}"
                );
                assert!(!line.contains(".ssh"), "a log line names the file: {line}");
                for needle in needles {
                    assert!(
                        !line.contains(needle),
                        "a log line carries {needle:?}: {line}"
                    );
                }
            }
        }

        /// A scratch home with `.ssh` at 0700; never the user's own.
        fn scratch_home(name: &str) -> (kettle_test_support::PrivateTempDir, PathBuf) {
            let home = kettle_test_support::private_tempdir(&format!("kettle-ssh-{name}-"));
            let ssh = home.path().join(".ssh");
            std::fs::create_dir(&ssh).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let config = ssh.join("config");
            (home, config)
        }

        fn write_private(path: &Path, bytes: &[u8]) {
            std::fs::write(path, bytes).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
        }

        #[test]
        fn a_missing_file_is_empty_and_silent() {
            let (home, config) = scratch_home("missing");
            let (aliases, lines) = import_logged(&config);
            assert_eq!(aliases, SshAliases::default());
            assert!(lines.is_empty(), "{lines:?}");
            drop(home);
        }

        #[test]
        fn a_private_file_is_imported() {
            let (home, config) = scratch_home("private");
            write_private(&config, b"Host prod staging\n  HostName secret.example\n");
            assert_eq!(
                read_host_aliases(&config).unwrap().aliases,
                ["prod", "staging"]
            );
            drop(home);
        }

        #[cfg(unix)]
        #[test]
        fn a_dotfile_link_is_followed_and_a_dangling_one_is_empty() {
            let (home, config) = scratch_home("links");
            let real = home.path().join("dotfiles-ssh-config");
            write_private(&real, b"Host linked\n");
            std::os::unix::fs::symlink(&real, &config).unwrap();
            assert_eq!(read_host_aliases(&config).unwrap().aliases, ["linked"]);

            std::fs::remove_file(&real).unwrap();
            let (aliases, lines) = import_logged(&config);
            assert_eq!(
                aliases,
                SshAliases::default(),
                "a dangling link reads as no file"
            );
            assert!(lines.is_empty(), "{lines:?}");
            drop(home);
        }

        #[test]
        fn an_ssh_folder_that_is_a_file_is_skipped_quietly() {
            let home = kettle_test_support::private_tempdir("kettle-ssh-dotssh-file-");
            let ssh = home.path().join(".ssh");
            write_private(&ssh, b"Host not-a-folder\n");
            let config = ssh.join("config");
            let result = read_host_aliases(&config);
            #[cfg(unix)]
            assert_eq!(
                result,
                Err(SshImportSkip::Io(std::io::ErrorKind::NotADirectory))
            );
            // Windows reports the missing folder as a missing path.
            #[cfg(windows)]
            assert!(
                result.is_err() || result == Ok(SshAliases::default()),
                "{result:?}"
            );
            let (aliases, lines) = import_logged(&config);
            assert!(aliases.aliases.is_empty());
            assert_private(&lines, home.path(), &["not-a-folder"]);
            drop(home);
        }

        #[test]
        fn a_directory_is_not_regular() {
            let (home, config) = scratch_home("directory");
            std::fs::create_dir(&config).unwrap();
            let result = read_host_aliases(&config);
            #[cfg(unix)]
            assert_eq!(result, Err(SshImportSkip::NotRegular));
            #[cfg(windows)]
            assert!(result.is_err(), "{result:?}");
            let (_, lines) = import_logged(&config);
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert_private(&lines, home.path(), &[]);
            drop(home);
        }

        #[test]
        fn an_oversized_file_parses_its_prefix_up_to_the_last_whole_line() {
            let (home, config) = scratch_home("oversized");
            let mut text = String::from("Host first\n");
            let filler = format!("# {}\n", "x".repeat(1021));
            while text.len() + filler.len() <= SSH_CONFIG_MAX_BYTES as usize - 32 {
                text.push_str(&filler);
            }
            // `Host stradd` ends exactly at the cap: the cut alias must not
            // become a row.
            while text.len() < SSH_CONFIG_MAX_BYTES as usize - 12 {
                text.push('#');
            }
            text.push_str("\nHost straddling-the-cap\n");
            while text.len() < 300 * 1024 {
                text.push_str("Host after-cap\n");
            }
            write_private(&config, text.as_bytes());
            let (aliases, lines) = import_logged(&config);
            assert_eq!(aliases.aliases, ["first"]);
            assert!(aliases.truncated);
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert_private(&lines, home.path(), &["first", "stradd", "after-cap"]);
            drop(home);
        }

        #[cfg(unix)]
        #[test]
        fn a_file_others_can_write_is_skipped() {
            use std::os::unix::fs::PermissionsExt as _;
            let (home, config) = scratch_home("insecure");
            write_private(&config, b"Host planted\n");
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o664)).unwrap();
            assert_eq!(
                read_host_aliases(&config),
                Err(SshImportSkip::InsecurePermissions)
            );
            let (aliases, lines) = import_logged(&config);
            assert!(aliases.aliases.is_empty());
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert_private(&lines, home.path(), &["planted", "mode", "664"]);
            drop(home);
        }

        #[cfg(windows)]
        #[test]
        fn a_file_whose_dacl_grants_everyone_write_is_skipped() {
            use std::process::{Command, Stdio};
            let (home, config) = scratch_home("dacl");
            write_private(&config, b"Host planted\n");
            assert_eq!(read_host_aliases(&config).unwrap().aliases, ["planted"]);
            assert!(
                Command::new("icacls.exe")
                    .arg(&config)
                    .args(["/grant", "*S-1-1-0:M"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success()
            );
            assert_eq!(
                read_host_aliases(&config),
                Err(SshImportSkip::InsecurePermissions)
            );
            drop(home);
        }

        #[cfg(unix)]
        #[test]
        fn a_fifo_is_refused_without_blocking() {
            let (home, config) = scratch_home("fifo");
            let name = std::ffi::CString::new(config.as_os_str().as_encoded_bytes()).unwrap();
            // SAFETY: `name` is a valid NUL-terminated path.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            let (tx, rx) = std::sync::mpsc::channel();
            let path = config.clone();
            std::thread::spawn(move || tx.send(read_host_aliases(&path)).unwrap());
            let result = rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("opening a FIFO must not block");
            assert_eq!(result, Err(SshImportSkip::NotRegular));
            drop(home);
        }
    }
}
