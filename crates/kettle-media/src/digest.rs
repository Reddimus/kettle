use std::io;

use sha2::{Digest as _, Sha256};

use crate::{Digest, PathIdentity, ValidationError, validate_identity};

/// How much [`content_digest_read`] reads at a time.
const READ_CHUNK: usize = 64 * 1024;

/// SHA-256 of a domain marker, content length, content, and optional opened-path metadata.
/// This is a cache identity, not an attestation. No path spelling is hashed or returned.
pub fn content_digest(
    content: &[u8],
    path_identity: Option<PathIdentity>,
) -> Result<Digest, ValidationError> {
    validate_identity(path_identity)?;
    let len = u64::try_from(content.len()).map_err(|_| ValidationError::TooLarge)?;
    let mut h = framed(len);
    h.update(content);
    Ok(finish(h, path_identity))
}

/// [`content_digest`] of exactly `len` bytes read from `content` a chunk at a time, so a file
/// need not be held whole to compare it with what was shown. Content that ends before `len`
/// bytes, or goes on past them, is `InvalidData`.
pub fn content_digest_read(
    mut content: impl io::Read,
    len: u64,
    path_identity: Option<PathIdentity>,
) -> io::Result<Digest> {
    validate_identity(path_identity)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid path identity"))?;
    let mut h = framed(len);
    let mut left = len;
    let mut chunk = vec![0; READ_CHUNK];
    loop {
        let read = match content.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        left = left.checked_sub(read as u64).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "the content is past its length")
        })?;
        h.update(&chunk[..read]);
    }
    if left != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the content ended before its length",
        ));
    }
    Ok(finish(h, path_identity))
}

/// A hasher that has taken the domain marker and the content's length.
fn framed(len: u64) -> Sha256 {
    let mut h = Sha256::new();
    h.update(b"kettle-media-content-v1\0");
    h.update(len.to_le_bytes());
    h
}

/// The digest of a hasher that has taken the content, once it takes the opened-path
/// metadata.
fn finish(mut h: Sha256, path_identity: Option<PathIdentity>) -> Digest {
    h.update([u8::from(path_identity.is_some())]);
    if let Some(p) = path_identity {
        for b in [
            p.dev.to_le_bytes(),
            p.ino.to_le_bytes(),
            p.size.to_le_bytes(),
            p.mtime_seconds.to_le_bytes(),
        ] {
            h.update(b);
        }
        h.update(p.mtime_nanos.to_le_bytes());
    }
    Digest {
        sha256: h.finalize().into(),
        path_identity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The framing is fixed: a value computed independently (Python's hashlib over the same
    /// marker, length, content and identity bytes) pins it, so a change to any field or its
    /// order shows here.
    #[test]
    fn content_digest_framing_is_stable() {
        let digest = content_digest(b"abc", None).unwrap();
        let hex: String = digest.sha256.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "b99d58c472addc137150dbd73f01c54b29267199de0a8bc91c8173d4ce467b83"
        );
    }

    /// Content read a chunk at a time digests as the same content held whole, with or
    /// without an identity, however the reader splits it.
    #[test]
    fn read_content_digests_as_held_content() {
        /// A reader that hands back at most `step` bytes a call.
        struct Trickle<'a> {
            rest: &'a [u8],
            step: usize,
        }
        impl io::Read for Trickle<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let n = self.step.min(buffer.len()).min(self.rest.len());
                buffer[..n].copy_from_slice(&self.rest[..n]);
                self.rest = &self.rest[n..];
                Ok(n)
            }
        }
        let identity = PathIdentity {
            dev: 7,
            ino: 11,
            size: 3,
            mtime_seconds: 1_700_000_000,
            mtime_nanos: 5,
        };
        for len in [
            0,
            1,
            READ_CHUNK - 1,
            READ_CHUNK,
            READ_CHUNK + 1,
            3 * READ_CHUNK + 7,
        ] {
            let content: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
            for path_identity in [None, Some(identity)] {
                let held = content_digest(&content, path_identity).unwrap();
                for step in [1, 4096, usize::MAX] {
                    let read = content_digest_read(
                        Trickle {
                            rest: &content,
                            step,
                        },
                        len as u64,
                        path_identity,
                    )
                    .unwrap();
                    assert_eq!(read, held, "{len} {step} {path_identity:?}");
                }
            }
        }
    }

    /// Content shorter or longer than its stated length is refused, never digested as
    /// what it is not.
    #[test]
    fn read_content_must_be_its_length() {
        let content = [1_u8; 10];
        for len in [9, 11, 0] {
            let error = content_digest_read(&content[..], len, None).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{len}");
        }
    }
}
