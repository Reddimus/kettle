//! Cross-platform owner-only permissions for already-open private objects.

use std::fs::File;
use std::io;

/// Restrict an already-open private file to the current user.
///
/// Unix applies mode `0600`. Windows replaces the inherited DACL with one
/// protected, full-access ACE for the current token user after confirming that
/// the object owner is either that user or the token's default owner. The
/// latter matters for elevated tokens whose newly created objects are owned by
/// the Administrators SID. The Windows implementation reopens the same kernel
/// file object with `WRITE_DAC`, so it cannot be redirected through a path race.
pub fn restrict_private_file(file: &File) -> io::Result<()> {
    restrict_private_object(file)
}

#[cfg(unix)]
fn restrict_private_object(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}

#[cfg(windows)]
fn restrict_private_object(file: &File) -> io::Result<()> {
    windows::restrict_private_object(file)
}

#[cfg(not(any(unix, windows)))]
fn restrict_private_object(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAceEx, DACL_SECURITY_INFORMATION,
        EqualSid, GetLengthSid, GetTokenInformation, IsValidSid, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_OWNER, TOKEN_QUERY,
        TOKEN_USER, TokenOwner, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ALL_ACCESS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, READ_CONTROL, ReOpenFile, WRITE_DAC,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    struct TokenHandle(HANDLE);

    impl Drop for TokenHandle {
        fn drop(&mut self) {
            // SAFETY: this wrapper owns the handle returned by OpenProcessToken.
            unsafe { CloseHandle(self.0) };
        }
    }

    struct TokenUserSid {
        _buffer: Vec<u64>,
        sid: PSID,
    }

    fn current_user_sid() -> io::Result<TokenUserSid> {
        let mut token = std::ptr::null_mut();
        // SAFETY: GetCurrentProcess returns a valid pseudo-handle and `token`
        // points to writable storage for the returned real token handle.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = TokenHandle(token);

        let mut len = 0_u32;
        // SAFETY: the null-buffer call is the documented size query.
        unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut len) };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        let words = (len as usize).div_ceil(std::mem::size_of::<u64>());
        let mut buffer = vec![0_u64; words];
        // SAFETY: `buffer` is aligned and at least `len` bytes long.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                len,
                &mut len,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful GetTokenInformation populated a TOKEN_USER header
        // whose SID points into `buffer`, retained by TokenUserSid.
        let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the current process token has an invalid user SID",
            ));
        }
        Ok(TokenUserSid {
            _buffer: buffer,
            sid,
        })
    }

    struct TokenOwnerSid {
        _buffer: Vec<u64>,
        sid: PSID,
    }

    fn current_default_owner_sid() -> io::Result<TokenOwnerSid> {
        let mut token = std::ptr::null_mut();
        // SAFETY: GetCurrentProcess returns a valid pseudo-handle and `token`
        // points to writable storage for the returned real token handle.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = TokenHandle(token);

        let mut len = 0_u32;
        // SAFETY: the null-buffer call is the documented size query.
        unsafe { GetTokenInformation(token.0, TokenOwner, std::ptr::null_mut(), 0, &mut len) };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        let words = (len as usize).div_ceil(std::mem::size_of::<u64>());
        let mut buffer = vec![0_u64; words];
        // SAFETY: `buffer` is aligned and at least `len` bytes long.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenOwner,
                buffer.as_mut_ptr().cast(),
                len,
                &mut len,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful GetTokenInformation populated a TOKEN_OWNER
        // header whose SID points into `buffer`, retained by TokenOwnerSid.
        let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_OWNER>()).Owner };
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the current process token has an invalid default-owner SID",
            ));
        }
        Ok(TokenOwnerSid {
            _buffer: buffer,
            sid,
        })
    }

    fn reopen_for_acl(file: &File) -> io::Result<OwnedHandle> {
        // SAFETY: `file` owns a valid handle. ReOpenFile resolves that same
        // kernel object rather than a path and returns a separately owned
        // handle with the rights SetSecurityInfo requires.
        let handle = unsafe {
            ReOpenFile(
                file.as_raw_handle() as HANDLE,
                READ_CONTROL | WRITE_DAC,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_FLAG_OPEN_REPARSE_POINT,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: ReOpenFile transferred ownership of this valid handle.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

    impl Drop for SecurityDescriptor {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: GetSecurityInfo allocated the descriptor with
                // LocalAlloc and transferred it to this wrapper.
                unsafe { LocalFree(self.0.cast()) };
            }
        }
    }

    fn require_current_token_owner(
        handle: HANDLE,
        current_user: PSID,
        default_owner: PSID,
    ) -> io::Result<()> {
        let mut owner = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: every output pointer is valid and the reopened handle has
        // READ_CONTROL. Unrequested group/DACL/SACL outputs are null.
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let _descriptor = SecurityDescriptor(descriptor);
        if owner.is_null()
            || (unsafe { EqualSid(owner, current_user) } == 0
                && unsafe { EqualSid(owner, default_owner) } == 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to change the ACL of an object not owned by the current token",
            ));
        }
        Ok(())
    }

    pub(super) fn restrict_private_object(file: &File) -> io::Result<()> {
        let current_user = current_user_sid()?;
        let default_owner = current_default_owner_sid()?;
        let handle = reopen_for_acl(file)?;
        require_current_token_owner(
            handle.as_raw_handle() as HANDLE,
            current_user.sid,
            default_owner.sid,
        )?;

        // One ACCESS_ALLOWED_ACE has a four-byte SID placeholder. Add the
        // actual variable-length SID and round storage up to u64 alignment.
        let sid_len = unsafe { GetLengthSid(current_user.sid) } as usize;
        let acl_len = std::mem::size_of::<ACL>()
            .checked_add(std::mem::size_of::<ACCESS_ALLOWED_ACE>() - std::mem::size_of::<u32>())
            .and_then(|base| base.checked_add(sid_len))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "ACL size overflow"))?;
        let words = acl_len.div_ceil(std::mem::size_of::<u64>());
        let mut acl_storage = vec![0_u64; words];
        let acl = acl_storage.as_mut_ptr().cast::<ACL>();
        let acl_capacity = u32::try_from(acl_storage.len() * std::mem::size_of::<u64>())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "ACL is too large"))?;
        // SAFETY: acl_storage is aligned, writable, and `acl_capacity` bytes.
        if unsafe { windows_sys::Win32::Security::InitializeAcl(acl, acl_capacity, ACL_REVISION) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `acl` is initialized with enough room for this ACE and the
        // token SID remains alive in current_user.
        if unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION, 0, FILE_ALL_ACCESS, current_user.sid) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: SetSecurityInfo consumes neither the handle nor ACL. The
        // protected DACL disables inherited broad entries.
        let status = unsafe {
            SetSecurityInfo(
                handle.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null(),
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    #[cfg(test)]
    pub(super) fn has_current_user_only_dacl(file: &File) -> io::Result<bool> {
        use windows_sys::Win32::Security::GetAce;

        let current_user = current_user_sid()?;
        let default_owner = current_default_owner_sid()?;
        let handle = reopen_for_acl(file)?;
        let mut owner = std::ptr::null_mut();
        let mut dacl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: output pointers are valid and the handle has READ_CONTROL.
        let status = unsafe {
            GetSecurityInfo(
                handle.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let _descriptor = SecurityDescriptor(descriptor);
        if owner.is_null()
            || dacl.is_null()
            || (unsafe { EqualSid(owner, current_user.sid) } == 0
                && unsafe { EqualSid(owner, default_owner.sid) } == 0)
            || unsafe { (*dacl).AceCount } != 1
        {
            return Ok(false);
        }
        let mut ace = std::ptr::null_mut();
        // SAFETY: the DACL reports exactly one ACE at index zero.
        if unsafe { GetAce(dacl, 0, &mut ace) } == 0 || ace.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: AddAccessAllowedAceEx created this ACE; check the header
        // before interpreting its access-allowed layout.
        let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
        if unsafe { (*allowed).Header.AceType } != 0 {
            return Ok(false);
        }
        let sid = unsafe { std::ptr::addr_of!((*allowed).SidStart).cast_mut().cast() };
        Ok(unsafe { EqualSid(sid, current_user.sid) } != 0
            && unsafe { (*allowed).Mask } == FILE_ALL_ACCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restrict_private_file_accepts_a_current_user_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private");
        let file = File::create(&path).unwrap();

        restrict_private_file(&file).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        }
        #[cfg(windows)]
        assert!(windows::has_current_user_only_dacl(&file).unwrap());
    }
}
