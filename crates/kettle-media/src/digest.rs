use sha2::{Digest as _, Sha256};

use crate::{Digest, PathIdentity, ValidationError, validate_identity};

/// SHA-256 of a domain marker, content length, content, and optional opened-path metadata.
/// This is a cache identity, not an attestation. No path spelling is hashed or returned.
pub fn content_digest(
    content: &[u8],
    path_identity: Option<PathIdentity>,
) -> Result<Digest, ValidationError> {
    validate_identity(path_identity)?;
    let len = u64::try_from(content.len()).map_err(|_| ValidationError::TooLarge)?;
    let mut h = Sha256::new();
    h.update(b"kettle-media-content-v1\0");
    h.update(len.to_le_bytes());
    h.update(content);
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
    Ok(Digest {
        sha256: h.finalize().into(),
        path_identity,
    })
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
}
