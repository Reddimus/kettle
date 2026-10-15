use std::path::PathBuf;

pub(super) const MAX_BYTES: usize = 1024 * 1024;
const MAX_PATHS: usize = 32;
const MAX_URI_BYTES: usize = 16 * 1024;

pub(super) fn parse(bytes: &[u8]) -> Result<Vec<PathBuf>, ()> {
    if bytes.len() > MAX_BYTES {
        return Err(());
    }
    let body = std::str::from_utf8(bytes).map_err(|_| ())?;
    let mut paths = Vec::new();
    for line in body
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        if paths.len() == MAX_PATHS
            || line.len() > MAX_URI_BYTES
            || line.bytes().any(|b| b <= b' ' || b == 127 || b == b'\\')
        {
            return Err(());
        }
        let prefix = line.get(..7).ok_or(())?;
        if !prefix.eq_ignore_ascii_case("file://") {
            return Err(());
        }
        let (host, path) = line[7..].split_once('/').ok_or(())?;
        if !(host.is_empty() || host.eq_ignore_ascii_case("localhost"))
            || path.starts_with('/')
            || path.contains(['?', '#'])
        {
            return Err(());
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
        paths.push(uri.to_file_path().map_err(|_| ())?);
    }
    if paths.is_empty() { Err(()) } else { Ok(paths) }
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
}
