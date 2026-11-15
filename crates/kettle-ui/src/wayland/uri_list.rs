//! `text/uri-list` as a list of local files.
//!
//! One rule serves a drop and a clipboard paste:
//!
//! - **Malformed** refuses the whole list: not UTF-8, over [`MAX_BYTES`], an
//!   entry over 16 KiB or holding a space, control character or backslash,
//!   an entry that is not a URI, or a local `file:` URI that is not exactly
//!   one absolute path (a bad escape, an encoded `/` or NUL, a `..` segment,
//!   a query, a fragment, user information, a port), or more than 32 local
//!   files.
//! - **Foreign** is a well-formed list in which some entry is not a local
//!   file: another scheme (`https:`, `sftp:`, `trash:`) or a `file:` URI on
//!   another host. Kettle types no path from it. A drop refuses it; a paste
//!   uses the clipboard's text instead.
//! - **Local** is a list of nothing but local files, the only kind that
//!   becomes paths.
use std::path::PathBuf;

pub(super) const MAX_BYTES: usize = 1024 * 1024;
const MAX_PATHS: usize = 32;
const MAX_URI_BYTES: usize = 16 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UriList {
    Local(Vec<PathBuf>),
    /// `local` says whether some entry did name a local file.
    Foreign {
        local: bool,
    },
    /// No entry: only comments and blank lines.
    Empty,
    Malformed,
}

/// The local files of a drop. Anything else refuses the drop.
pub(super) fn parse(bytes: &[u8]) -> Result<Vec<PathBuf>, ()> {
    match classify(bytes) {
        UriList::Local(paths) => Ok(paths),
        UriList::Foreign { .. } | UriList::Empty | UriList::Malformed => Err(()),
    }
}

/// `x-special/gnome-copied-files`: a first line `copy` or `cut`, then a URI
/// list. A cut pastes the same paths; nothing is moved.
pub(crate) fn classify_gnome_copied_files(bytes: &[u8]) -> UriList {
    if bytes.len() > MAX_BYTES {
        return UriList::Malformed;
    }
    let (verb, list) = match bytes.iter().position(|byte| *byte == b'\n') {
        Some(end) => (&bytes[..end], &bytes[end + 1..]),
        None => (bytes, &[][..]),
    };
    match verb.strip_suffix(b"\r").unwrap_or(verb) {
        b"copy" | b"cut" => classify(list),
        _ => UriList::Malformed,
    }
}

pub(crate) fn classify(bytes: &[u8]) -> UriList {
    if bytes.len() > MAX_BYTES {
        return UriList::Malformed;
    }
    let Ok(body) = std::str::from_utf8(bytes) else {
        return UriList::Malformed;
    };
    let mut paths = Vec::new();
    let (mut foreign, mut local, mut too_many) = (false, false, false);
    for line in body
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        match entry(line) {
            Err(()) => return UriList::Malformed,
            Ok(None) => foreign = true,
            Ok(Some(path)) => {
                local = true;
                if paths.len() == MAX_PATHS {
                    too_many = true;
                } else {
                    paths.push(path);
                }
            }
        }
    }
    if foreign {
        UriList::Foreign { local }
    } else if too_many {
        UriList::Malformed
    } else if paths.is_empty() {
        UriList::Empty
    } else {
        UriList::Local(paths)
    }
}

/// One entry: a local file's path, `None` for a well-formed URI that is not
/// a local file, or an error for a malformed one.
fn entry(line: &str) -> Result<Option<PathBuf>, ()> {
    if line.len() > MAX_URI_BYTES || line.bytes().any(|b| b <= b' ' || b == 127 || b == b'\\') {
        return Err(());
    }
    let (scheme, rest) = line.split_once(':').ok_or(())?;
    let mut letters = scheme.bytes();
    if !letters.next().is_some_and(|b| b.is_ascii_alphabetic())
        || !letters.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
        || rest.is_empty()
    {
        return Err(());
    }
    if !scheme.eq_ignore_ascii_case("file") {
        return url::Url::parse(line).map(|_| None).map_err(|_| ());
    }
    let (host, path) = rest
        .strip_prefix("//")
        .ok_or(())?
        .split_once('/')
        .ok_or(())?;
    if path.starts_with('/') || path.contains(['?', '#']) || host.contains(['@', ':']) {
        return Err(());
    }
    if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
        return url::Url::parse(line).map(|_| None).map_err(|_| ());
    }
    // Validate before Url can normalize dot segments or separators.
    let mut decoded = Vec::with_capacity(path.len());
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        let byte = if byte == b'%' {
            let hi = (bytes.next().ok_or(())? as char).to_digit(16).ok_or(())?;
            let lo = (bytes.next().ok_or(())? as char).to_digit(16).ok_or(())?;
            let decoded = (hi * 16 + lo) as u8;
            if decoded == b'/' {
                return Err(());
            }
            decoded
        } else {
            byte
        };
        if byte == 0 {
            return Err(());
        }
        decoded.push(byte);
    }
    if decoded.split(|b| *b == b'/').any(|part| part == b"..") {
        return Err(());
    }
    let uri = url::Url::parse(line).map_err(|_| ())?;
    uri.to_file_path().map(Some).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn local_uri_lists_preserve_names_and_ignore_comments() {
        assert_eq!(
            parse(b"# Files\r\nfile:///tmp/a%20b.png\r\nfile://localhost/tmp/%23%25.svg\r\n")
                .unwrap(),
            [PathBuf::from("/tmp/a b.png"), PathBuf::from("/tmp/#%.svg")]
        );
        assert_eq!(
            parse(b"file:///tmp/%FF%0A%5Cz").unwrap(),
            [PathBuf::from(std::ffi::OsString::from_vec(
                b"/tmp/\xff\n\\z".to_vec()
            ))]
        );
    }

    #[test]
    fn one_bad_uri_refuses_the_whole_drop() {
        for bad in [
            "https://example.invalid/a",
            "file://remote.invalid/a",
            "file://user@localhost/a",
            "file://localhost:123/a",
            "file:///a%00b",
            "file:///a%GG",
            "file:///a%0",
            "file:///a?query",
            "file:///a#fragment",
            "file:////host/a",
            "file:///a/../b",
            "file:///a/%2e%2e/b",
            "file:///a%2Fb",
            "file:///a\\b",
            "file:///a b",
            " file:///a",
        ] {
            let bytes = format!("file:///tmp/good\n{bad}\n");
            assert!(parse(bytes.as_bytes()).is_err(), "{bad}");
        }
        assert!(parse(b"file:///tmp/\xff").is_err());
        assert!(parse(b"# No files\n\r\n").is_err());
    }

    #[test]
    fn body_path_count_and_uri_sizes_have_exact_bounds() {
        let list = "file:///tmp/a\n".repeat(MAX_PATHS);
        assert_eq!(parse(list.as_bytes()).unwrap().len(), MAX_PATHS);
        assert!(parse((list + "file:///tmp/b\n").as_bytes()).is_err());
        let uri = format!("file:///{}", "a".repeat(MAX_URI_BYTES - "file:///".len()));
        assert!(parse(uri.as_bytes()).is_ok());
        assert!(parse((uri + "a").as_bytes()).is_err());
        let mut body = format!(
            "#{}\nfile:///tmp/a",
            "x".repeat(MAX_BYTES - 1 - 1 - "file:///tmp/a".len())
        );
        assert_eq!(body.len(), MAX_BYTES);
        assert!(parse(body.as_bytes()).is_ok());
        body.push('\n');
        assert!(parse(body.as_bytes()).is_err());
    }

    #[test]
    fn a_well_formed_list_that_is_not_all_local_files_is_foreign_not_malformed() {
        for (list, local) in [
            ("https://example.invalid/a\r\n", false),
            ("sftp://host.invalid/srv/a\n", false),
            ("trash:///a\n", false),
            ("file://remote.invalid/a\n", false),
            ("file:///tmp/a\nhttps://example.invalid/b\n", true),
            ("smb://server.invalid/share\nfile://localhost/tmp/a\n", true),
        ] {
            assert_eq!(
                classify(list.as_bytes()),
                UriList::Foreign { local },
                "{list:?}"
            );
            assert!(parse(list.as_bytes()).is_err(), "a drop refuses {list:?}");
        }
        assert_eq!(classify(b"# No files\n\r\n"), UriList::Empty);
        assert_eq!(
            classify(b"file:///tmp/a%20b\r\n"),
            UriList::Local(vec![PathBuf::from("/tmp/a b")])
        );
    }

    #[test]
    fn a_malformed_entry_refuses_the_list_whatever_else_it_holds() {
        for bad in [
            "not a uri",
            "no-scheme/path",
            "1http://example.invalid/",
            "http:",
            "https://exa mple.invalid/",
            "file:/tmp/a",
            "file:///a%GG",
            "file:///a/../b",
            "file:///a%2Fb",
            "file:///a%00b",
            "file:///a?query",
            "file://user@localhost/a",
            "file://localhost:123/a",
            "file://user@remote.invalid/a",
            "file:////host/a",
        ] {
            for list in [
                format!("{bad}\n"),
                format!("file:///tmp/good\n{bad}\n"),
                format!("https://example.invalid/a\n{bad}\n"),
            ] {
                assert_eq!(classify(list.as_bytes()), UriList::Malformed, "{list:?}");
            }
        }
        assert_eq!(classify(b"file:///tmp/\xff"), UriList::Malformed);
        let many = "file:///tmp/a\n".repeat(MAX_PATHS + 1);
        assert_eq!(classify(many.as_bytes()), UriList::Malformed);
        assert_eq!(
            classify((many + "https://example.invalid/\n").as_bytes()),
            UriList::Foreign { local: true },
            "links are not counted as files"
        );
        assert_eq!(
            classify(&vec![b'#'; MAX_BYTES + 1]),
            UriList::Malformed,
            "over the byte cap"
        );
    }

    #[test]
    fn gnome_copied_files_is_a_uri_list_after_its_verb_line() {
        let local = UriList::Local(vec![PathBuf::from("/tmp/a b"), PathBuf::from("/tmp/c")]);
        assert_eq!(
            classify_gnome_copied_files(b"copy\nfile:///tmp/a%20b\nfile:///tmp/c"),
            local
        );
        assert_eq!(
            classify_gnome_copied_files(b"cut\r\nfile:///tmp/a%20b\r\nfile:///tmp/c\r\n"),
            local
        );
        assert_eq!(
            classify_gnome_copied_files(b"copy\nsftp://host.invalid/a"),
            UriList::Foreign { local: false }
        );
        assert_eq!(classify_gnome_copied_files(b"copy\n"), UriList::Empty);
        assert_eq!(classify_gnome_copied_files(b"copy"), UriList::Empty);
        for bad in [
            &b"file:///tmp/a\nfile:///tmp/b"[..],
            b"move\nfile:///tmp/a",
            b"\nfile:///tmp/a",
            b"copy\nfile:///tmp/a%GG",
        ] {
            assert_eq!(classify_gnome_copied_files(bad), UriList::Malformed);
        }
    }
}
