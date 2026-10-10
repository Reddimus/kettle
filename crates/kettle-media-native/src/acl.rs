//! Extended ACLs, which on macOS can let another user write a file whose mode
//! says otherwise. On Linux an entry that grants write shows in the group mode
//! bits, which a mode check already refuses.

use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

use libc::{c_char, c_int, c_void, ssize_t};

const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
/// Permissions that only read. An entry that allows any other counts as
/// letting someone write.
const READ_ONLY: [&[u8]; 5] = [
    b"read",
    b"execute",
    b"readattr",
    b"readextattr",
    b"readsecurity",
];

unsafe extern "C" {
    fn acl_get_link_np(path: *const c_char, kind: c_int) -> *mut c_void;
    fn acl_to_text(acl: *mut c_void, length: *mut ssize_t) -> *mut c_char;
    fn acl_free(object: *mut c_void) -> c_int;
}

/// Whether the ACL on `path` itself, not through a link, allows anyone
/// more than reading. Deny entries only take rights away. An ACL that
/// cannot be read counts as allowing.
pub fn grants_write(path: &Path) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return true;
    };
    // SAFETY: `path` is NUL-terminated and outlives the call.
    let acl = unsafe { acl_get_link_np(path.as_ptr(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        // A file without an extended ACL reads as ENOENT.
        return std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT);
    }
    let mut length: ssize_t = 0;
    // SAFETY: `acl` is the live ACL just returned, and `length` a valid
    // place for its text length.
    let text = unsafe { acl_to_text(acl, &mut length) };
    let grants = text.is_null() || {
        // SAFETY: acl_to_text returns a NUL-terminated string, valid
        // until it is freed below.
        let grants = text_grants_write(unsafe { CStr::from_ptr(text) }.to_bytes());
        // SAFETY: `text` came from acl_to_text and is freed once.
        unsafe { acl_free(text.cast()) };
        grants
    };
    // SAFETY: `acl` came from acl_get_link_np and is freed once.
    unsafe { acl_free(acl) };
    grants
}

/// Whether an `acl_to_text` listing has an entry that allows more than
/// reading. Entries read `tag:uuid:name:id:allow[,flags]:permissions`; an
/// entry in any other shape counts as allowing.
pub fn text_grants_write(text: &[u8]) -> bool {
    text.split(|&byte| byte == b'\n')
        .filter(|entry| !entry.is_empty() && !entry.starts_with(b"!#"))
        .any(|entry| {
            let fields: Vec<&[u8]> = entry.split(|&byte| byte == b':').collect();
            let [_, _, _, _, kind, permissions] = fields.as_slice() else {
                return true;
            };
            let deny = kind.split(|&byte| byte == b',').next() == Some(b"deny");
            !deny
                && permissions
                    .split(|&byte| byte == b',')
                    .any(|permission| !READ_ONLY.contains(&permission))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acl_text_counts_only_read_and_deny_entries_as_safe() {
        let everyone = "group:ABCDEFAB-CDEF-ABCD-EFAB-CDEF0000000C:everyone:12";
        for (text, grants) in [
            ("!#acl 1\n".to_string(), false),
            (format!("!#acl 1\n{everyone}:deny:delete\n"), false),
            (
                format!("!#acl 1\n{everyone}:deny,file_inherit:write,delete\n"),
                false,
            ),
            (format!("!#acl 1\n{everyone}:allow:read,readattr\n"), false),
            (format!("!#acl 1\n{everyone}:allow:write\n"), true),
            (
                format!("!#acl 1\n{everyone}:allow,file_inherit:read,chown\n"),
                true,
            ),
            (
                format!("!#acl 1\n{everyone}:deny:delete\n{everyone}:allow:append\n"),
                true,
            ),
            // An entry in another shape counts as allowing.
            ("!#acl 1\nuser:allow:read\n".to_string(), true),
        ] {
            assert_eq!(text_grants_write(text.as_bytes()), grants, "{text}");
        }
    }
}
