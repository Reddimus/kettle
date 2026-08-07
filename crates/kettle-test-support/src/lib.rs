//! Shared filesystem fixtures for Kettle's workspace tests.

use std::path::Path;

/// An automatically removed private scratch directory.
#[derive(Debug)]
pub struct PrivateTempDir(tempfile::TempDir);

impl PrivateTempDir {
    /// Return the scratch directory path.
    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

impl AsRef<Path> for PrivateTempDir {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}

impl std::ops::Deref for PrivateTempDir {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.path()
    }
}

/// Create an automatically cleaned-up scratch directory suitable for private
/// Kettle state.
///
/// Unix requests owner-only permissions explicitly instead of inheriting a
/// permissive ambient umask. Windows stages beneath the user profile because
/// the shared temporary directory can grant deletion rights to other users.
pub fn private_tempdir(prefix: &str) -> PrivateTempDir {
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(windows)]
    let dir = {
        let base = std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .expect("Windows tests require LOCALAPPDATA or USERPROFILE");
        builder
            .tempdir_in(base)
            .expect("create private test directory in the user profile")
    };
    #[cfg(not(windows))]
    let dir = builder.tempdir().expect("create private test directory");
    PrivateTempDir(dir)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn private_tempdir_has_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = super::private_tempdir("kettle-test-support-");
        assert_eq!(
            std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
