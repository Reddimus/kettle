//! Who signed a running process's code, as macOS validates it.
//!
//! Display provenance and harness checks need the signer of a process Kettle
//! did not start. The answer comes from the kernel and the Security
//! framework, never from the process's own words or its path, and it belongs
//! to one process instance: every check is bound to the process's audit
//! token, whose pid version changes when the process runs another program,
//! and the token is read again afterwards.
//!
//! The check is the running code's, without hashing its file again, which for
//! a large program takes most of a second:
//!
//! 1. the kernel reports the process's code as valid, which it keeps only
//!    while every page it loaded matched its code directory;
//! 2. the program file's signature over its code directory, and its
//!    certificate chain, meet the requirement, with no network lookup;
//! 3. that code directory is the one the kernel runs, by hash.

use crate::process::ProcessIdentity;

/// A code requirement in Apple's requirement language. Only Kettle's own
/// constants make one: a requirement that fails to parse makes the Security
/// framework throw past its C interface, so text from outside never reaches
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requirement(&'static str);

impl Requirement {
    /// Code signed with a certificate Apple issued: Developer ID, the App
    /// Store, or Apple's own.
    pub const APPLE_ISSUED: Self = Self("anchor apple generic");

    /// Apple's own code, signed with Apple's own certificate.
    pub const APPLE_OWN: Self = Self("anchor apple");

    /// Claude Code as Anthropic signs it: its identifier, under a
    /// certificate Apple issued to Anthropic's team.
    pub const CLAUDE_CODE: Self = Self(
        r#"anchor apple generic and identifier "com.anthropic.claude-code" and certificate leaf[subject.OU] = "Q6L2SF6YDW""#,
    );

    pub const fn text(self) -> &'static str {
        self.0
    }

    /// Every requirement Kettle checks, for the test that parses them all.
    pub const ALL: [Self; 3] = [Self::APPLE_ISSUED, Self::APPLE_OWN, Self::CLAUDE_CODE];
}

/// A running process's signature, read after it met a requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// The signing identifier, such as `com.anthropic.claude-code`.
    pub identifier: String,
    /// The Apple team identifier; `None` for Apple's own platform code and
    /// some older signatures.
    pub team: Option<String>,
    /// The leaf certificate's subject, such as `Developer ID Application:
    /// Anthropic PBC (Q6L2SF6YDW)`, or `macOS Software Signing` for Apple's
    /// own.
    pub authority: Option<String>,
    /// Whether it is Apple's own code, signed with Apple's own certificate.
    pub apple: bool,
    /// The program file the process ran while it was checked.
    pub executable: std::path::PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureError {
    /// This platform has no code signatures to read.
    Unsupported,
    /// The process exited, another now has its pid, or it ran another
    /// program while it was being checked.
    Gone,
    /// The code is unsigned, ad-hoc signed or altered, or it does not meet
    /// the requirement.
    NotValid,
    /// The OS could not answer.
    Os,
}

/// Longest identifier, team or authority kept; a longer one is not read, as
/// no real signature carries one.
pub const MAX_SIGNATURE_FIELD_BYTES: usize = 256;

/// Validate the code `process` runs against `requirement` and read who
/// signed it.
pub fn signature(
    process: ProcessIdentity,
    requirement: Requirement,
) -> Result<Signature, SignatureError> {
    imp::signature(process, requirement)
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{MAX_SIGNATURE_FIELD_BYTES, Requirement, Signature, SignatureError};
    use crate::process::{self, ProcessIdentity};
    use std::ffi::c_void;

    type CFTypeRef = *const c_void;
    type CFIndex = isize;
    type OSStatus = i32;
    type SecCSFlags = u32;

    /// An opaque C struct only ever handled by address.
    #[repr(C)]
    struct Opaque {
        _private: [u8; 0],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CFRange {
        location: CFIndex,
        length: CFIndex,
    }

    /// The kernel's `audit_token_t`: a process instance, its pid in the
    /// sixth word and its pid version, which an exec changes, in the eighth.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct AuditToken {
        val: [u32; 8],
    }

    const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const SEC_CS_DEFAULT_FLAGS: SecCSFlags = 0;
    /// `kSecCSSigningInformation`: include the certificate chain and team.
    const SEC_CS_SIGNING_INFORMATION: SecCSFlags = 1 << 1;
    /// `kSecCSDoNotValidateExecutable`: the kernel validates the running
    /// pages, so the file is not hashed again.
    const SEC_CS_DO_NOT_VALIDATE_EXECUTABLE: SecCSFlags = 1 << 1;
    /// `kSecCSDoNotValidateResources`: a command-line program has no bundle
    /// resources, and an app's are not what runs.
    const SEC_CS_DO_NOT_VALIDATE_RESOURCES: SecCSFlags = 1 << 2;
    /// `kSecCSNoNetworkAccess`: no revocation lookup leaves the machine, so
    /// checking a sender reveals nothing about it to anyone.
    const SEC_CS_NO_NETWORK_ACCESS: SecCSFlags = 1 << 29;
    /// `TASK_AUDIT_TOKEN` and its size in 32-bit words.
    const TASK_AUDIT_TOKEN: u32 = 15;
    const TASK_AUDIT_TOKEN_COUNT: u32 = 8;
    /// `csops` operations: the code's status flags, and its code directory
    /// hash as the kernel holds it.
    const CS_OPS_STATUS: u32 = 0;
    const CS_OPS_CDHASH: u32 = 5;
    const CS_CDHASH_LEN: usize = 20;
    /// The kernel's `CS_VALID`: every page the process loaded matched its
    /// code directory.
    const CS_VALID: u32 = 0x1;
    /// `errSecCSNoSuchCode` and `errSecCSStaticCodeNotFound`: no such guest.
    const NO_SUCH_CODE: [OSStatus; 2] = [-67065, -67068];
    /// Failures of the OS itself rather than findings about the code:
    /// `errSecAllocate`, `errSecCSInternalError`, `errSecInternalComponent`,
    /// `errSecNotAvailable` and `errSecMemoryError`.
    const OS_FAILURES: [OSStatus; 5] = [-108, -67048, -2070, -25291, -67672];

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFTypeDictionaryKeyCallBacks: Opaque;
        static kCFTypeDictionaryValueCallBacks: Opaque;
        fn CFRelease(cf: CFTypeRef);
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFArrayGetTypeID() -> usize;
        fn CFDataGetTypeID() -> usize;
        fn CFDataCreate(allocator: CFTypeRef, bytes: *const u8, length: CFIndex) -> CFTypeRef;
        fn CFDataGetLength(data: CFTypeRef) -> CFIndex;
        fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
        fn CFDictionaryCreate(
            allocator: CFTypeRef,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            count: CFIndex,
            key_callbacks: *const Opaque,
            value_callbacks: *const Opaque,
        ) -> CFTypeRef;
        fn CFDictionaryGetValue(dictionary: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFStringCreateWithBytes(
            allocator: CFTypeRef,
            bytes: *const u8,
            count: CFIndex,
            encoding: u32,
            is_external: u8,
        ) -> CFTypeRef;
        fn CFStringGetLength(string: CFTypeRef) -> CFIndex;
        fn CFStringGetBytes(
            string: CFTypeRef,
            range: CFRange,
            encoding: u32,
            loss_byte: u8,
            is_external: u8,
            buffer: *mut u8,
            max_length: CFIndex,
            used: *mut CFIndex,
        ) -> CFIndex;
        fn CFArrayGetCount(array: CFTypeRef) -> CFIndex;
        fn CFArrayGetValueAtIndex(array: CFTypeRef, index: CFIndex) -> CFTypeRef;
    }

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        static kSecGuestAttributeAudit: CFTypeRef;
        static kSecCodeInfoIdentifier: CFTypeRef;
        static kSecCodeInfoTeamIdentifier: CFTypeRef;
        static kSecCodeInfoCertificates: CFTypeRef;
        static kSecCodeInfoUnique: CFTypeRef;
        fn SecCodeCopyGuestWithAttributes(
            host: CFTypeRef,
            attributes: CFTypeRef,
            flags: SecCSFlags,
            guest: *mut CFTypeRef,
        ) -> OSStatus;
        fn SecCodeCopyStaticCode(
            code: CFTypeRef,
            flags: SecCSFlags,
            static_code: *mut CFTypeRef,
        ) -> OSStatus;
        fn SecRequirementCreateWithString(
            text: CFTypeRef,
            flags: SecCSFlags,
            requirement: *mut CFTypeRef,
        ) -> OSStatus;
        fn SecStaticCodeCheckValidity(
            static_code: CFTypeRef,
            flags: SecCSFlags,
            requirement: CFTypeRef,
        ) -> OSStatus;
        fn SecCodeCopySigningInformation(
            code: CFTypeRef,
            flags: SecCSFlags,
            information: *mut CFTypeRef,
        ) -> OSStatus;
        fn SecCertificateCopySubjectSummary(certificate: CFTypeRef) -> CFTypeRef;
    }

    unsafe extern "C" {
        /// This task's own port name, what `mach_task_self()` reads.
        static mach_task_self_: libc::mach_port_t;
        fn task_name_for_pid(
            target: libc::mach_port_t,
            pid: libc::c_int,
            name: *mut libc::mach_port_t,
        ) -> libc::kern_return_t;
        fn mach_port_deallocate(
            task: libc::mach_port_t,
            name: libc::mach_port_t,
        ) -> libc::kern_return_t;
        fn csops_audittoken(
            pid: libc::pid_t,
            ops: u32,
            buffer: *mut c_void,
            size: usize,
            token: *const AuditToken,
        ) -> libc::c_int;
    }

    /// An owned Core Foundation reference, released when dropped.
    pub(super) struct Owned(CFTypeRef);

    impl Owned {
        /// Take ownership of a reference a Create or Copy call returned;
        /// `None` for a null one, which is never released.
        pub(super) fn new(reference: CFTypeRef) -> Option<Self> {
            if reference.is_null() {
                None
            } else {
                Some(Self(reference))
            }
        }
    }

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a non-null reference this value owns, from
            // a Create or Copy call, released exactly once here.
            unsafe { CFRelease(self.0) }
        }
    }

    /// What a failed Security call means: the process is gone, the OS could
    /// not answer, or else the code is not validly signed as required, which
    /// covers every finding about it, its certificates included.
    pub(super) fn failure(status: OSStatus) -> SignatureError {
        if NO_SUCH_CODE.contains(&status) {
            SignatureError::Gone
        } else if OS_FAILURES.contains(&status) {
            SignatureError::Os
        } else {
            SignatureError::NotValid
        }
    }

    /// An owned result of a Security call that writes one reference.
    fn copied(status: OSStatus, reference: CFTypeRef) -> Result<Owned, SignatureError> {
        let owned = Owned::new(reference);
        match (status, owned) {
            (0, Some(owned)) => Ok(owned),
            (0, None) => Err(SignatureError::Os),
            (status, _) => Err(failure(status)),
        }
    }

    /// Whether `value` is a non-null object of the type `type_id` names.
    fn is_a(value: CFTypeRef, type_id: usize) -> bool {
        // SAFETY: `value` is a non-null Core Foundation object borrowed from
        // a dictionary or array the caller keeps alive.
        !value.is_null() && unsafe { CFGetTypeID(value) } == type_id
    }

    fn cf_string(text: &str) -> Option<Owned> {
        let count = CFIndex::try_from(text.len()).ok()?;
        // SAFETY: `text` is valid for `count` bytes of UTF-8; the call copies
        // them and returns an owned string or null.
        Owned::new(unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                text.as_ptr(),
                count,
                CF_STRING_ENCODING_UTF8,
                0,
            )
        })
    }

    /// A Core Foundation string as UTF-8, if `value` is one no longer than
    /// [`MAX_SIGNATURE_FIELD_BYTES`].
    fn rust_string(value: CFTypeRef) -> Option<String> {
        // SAFETY: a type-id query, which takes no ownership.
        if !is_a(value, unsafe { CFStringGetTypeID() }) {
            return None;
        }
        // SAFETY: `value` is a string, checked above.
        let length = unsafe { CFStringGetLength(value) };
        let range = CFRange {
            location: 0,
            length,
        };
        let mut buffer = vec![0u8; MAX_SIGNATURE_FIELD_BYTES];
        let mut used: CFIndex = 0;
        // SAFETY: `buffer` is live and writable for its length, which is
        // passed as the maximum; `used` receives how many bytes were written.
        let converted = unsafe {
            CFStringGetBytes(
                value,
                range,
                CF_STRING_ENCODING_UTF8,
                0,
                0,
                buffer.as_mut_ptr(),
                buffer.len() as CFIndex,
                &mut used,
            )
        };
        // Every character must convert; a partial conversion means the field
        // was longer than the buffer.
        if converted != length {
            return None;
        }
        buffer.truncate(usize::try_from(used).ok()?);
        String::from_utf8(buffer).ok()
    }

    /// The bytes of a Core Foundation data object, if `value` is one.
    fn data_bytes(value: CFTypeRef) -> Option<Vec<u8>> {
        // SAFETY: a type-id query, which takes no ownership.
        if !is_a(value, unsafe { CFDataGetTypeID() }) {
            return None;
        }
        // SAFETY: `value` is a data object, checked above; its byte pointer
        // is valid for its length while the caller keeps it alive.
        unsafe {
            let length = usize::try_from(CFDataGetLength(value)).ok()?;
            let bytes = CFDataGetBytePtr(value);
            (!bytes.is_null()).then(|| std::slice::from_raw_parts(bytes, length).to_vec())
        }
    }

    /// The audit token of the process with `pid` now.
    pub(super) fn audit_token(pid: u32) -> Result<AuditToken, SignatureError> {
        let os_pid = libc::c_int::try_from(pid).map_err(|_| SignatureError::Gone)?;
        // SAFETY: reads this task's own port name, set before main and never
        // changed, which needs no release.
        let task = unsafe { mach_task_self_ };
        let mut port: libc::mach_port_t = 0;
        // SAFETY: `port` receives a send right on success, released below.
        if unsafe { task_name_for_pid(task, os_pid, &mut port) } != libc::KERN_SUCCESS {
            return Err(SignatureError::Gone);
        }
        let mut token = AuditToken { val: [0; 8] };
        let mut count = TASK_AUDIT_TOKEN_COUNT;
        // SAFETY: `token` holds `count` 32-bit words, the flavor's size, and
        // `count` receives how many were written.
        let result = unsafe {
            libc::task_info(
                port,
                TASK_AUDIT_TOKEN,
                token.val.as_mut_ptr().cast(),
                &mut count,
            )
        };
        // SAFETY: `port` is the send right task_name_for_pid gave, released
        // exactly once.
        unsafe { mach_port_deallocate(task, port) };
        if result != libc::KERN_SUCCESS || count != TASK_AUDIT_TOKEN_COUNT || token.val[5] != pid {
            return Err(SignatureError::Gone);
        }
        Ok(token)
    }

    /// A `csops` answer about exactly the process instance `token` names.
    fn csops(token: &AuditToken, operation: u32, buffer: &mut [u8]) -> Result<(), SignatureError> {
        let pid = libc::pid_t::try_from(token.val[5]).map_err(|_| SignatureError::Gone)?;
        // SAFETY: `buffer` is live and writable for its length, which is
        // passed; `token` is a live audit token.
        let result = unsafe {
            csops_audittoken(
                pid,
                operation,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                token,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(SignatureError::Gone)
        }
    }

    /// The code object for exactly the process instance `token` names.
    fn guest(token: &AuditToken) -> Result<Owned, SignatureError> {
        // SAFETY: `token` is valid for its size; the call copies it.
        let data = Owned::new(unsafe {
            CFDataCreate(
                std::ptr::null(),
                std::ptr::from_ref(token).cast(),
                std::mem::size_of::<AuditToken>() as CFIndex,
            )
        })
        .ok_or(SignatureError::Os)?;
        // SAFETY: the key is a constant the framework exports; the dictionary
        // retains the key and value through the callbacks passed.
        let attributes = Owned::new(unsafe {
            let keys = [kSecGuestAttributeAudit];
            let values = [data.0];
            CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            )
        })
        .ok_or(SignatureError::Os)?;
        let mut code: CFTypeRef = std::ptr::null();
        // SAFETY: `attributes` is a live dictionary and `code` receives an
        // owned reference on success.
        let status = unsafe {
            SecCodeCopyGuestWithAttributes(
                std::ptr::null(),
                attributes.0,
                SEC_CS_DEFAULT_FLAGS,
                &mut code,
            )
        };
        copied(status, code)
    }

    /// The value for `key` in a live dictionary, borrowed for as long as the
    /// dictionary lives.
    fn value(dictionary: &Owned, key: CFTypeRef) -> CFTypeRef {
        // SAFETY: `dictionary` is a live dictionary and `key` a constant the
        // framework exports.
        unsafe { CFDictionaryGetValue(dictionary.0, key) }
    }

    /// Whether the file's code meets `requirement`, checked without hashing
    /// the file and without the network.
    fn meets(file: &Owned, requirement: Requirement) -> Result<(), SignatureError> {
        let requirement = parse(requirement)?;
        // SAFETY: `file` and `requirement` are live, owned references.
        let status = unsafe {
            SecStaticCodeCheckValidity(
                file.0,
                SEC_CS_DO_NOT_VALIDATE_EXECUTABLE
                    | SEC_CS_DO_NOT_VALIDATE_RESOURCES
                    | SEC_CS_NO_NETWORK_ACCESS,
                requirement.0,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(failure(status))
        }
    }

    pub(super) fn signature(
        process: ProcessIdentity,
        requirement: Requirement,
    ) -> Result<Signature, SignatureError> {
        let executable = process::executable(process).map_err(|_| SignatureError::Gone)?;
        let token = audit_token(process.pid())?;
        let checked = check(&token, requirement);
        // Whatever the checks found describes the instance the token names;
        // only that instance, still running the same file, keeps it.
        let same = audit_token(process.pid()).is_ok_and(|now| now == token)
            && process::executable(process).is_ok_and(|now| now == executable);
        if !same {
            return Err(SignatureError::Gone);
        }
        let (identifier, team, authority, apple) = checked?;
        Ok(Signature {
            identifier,
            team,
            authority,
            apple,
            executable,
        })
    }

    type Checked = (String, Option<String>, Option<String>, bool);

    fn check(token: &AuditToken, requirement: Requirement) -> Result<Checked, SignatureError> {
        // 1. The kernel's word that the running code matches its directory.
        let mut status = [0u8; 4];
        csops(token, CS_OPS_STATUS, &mut status)?;
        if u32::from_ne_bytes(status) & CS_VALID == 0 {
            return Err(SignatureError::NotValid);
        }
        let mut running = [0u8; CS_CDHASH_LEN];
        csops(token, CS_OPS_CDHASH, &mut running)?;

        // 2. The file's signature and certificates meet the requirement. This
        // runs before any signing information is read, so the chain is
        // evaluated without the network first and only reused after.
        let code = guest(token)?;
        let mut file: CFTypeRef = std::ptr::null();
        // SAFETY: `code` is live; `file` receives an owned static code.
        let status = unsafe { SecCodeCopyStaticCode(code.0, SEC_CS_DEFAULT_FLAGS, &mut file) };
        let file = copied(status, file)?;
        meets(&file, requirement)?;
        let apple = meets(&file, Requirement::APPLE_OWN).is_ok();

        // 3. The file's code directory is the one the kernel runs.
        let mut information: CFTypeRef = std::ptr::null();
        // SAFETY: `file` is live; `information` receives an owned dictionary.
        let status = unsafe {
            SecCodeCopySigningInformation(file.0, SEC_CS_SIGNING_INFORMATION, &mut information)
        };
        let signing = copied(status, information)?;
        // SAFETY: the key is a constant the framework exports.
        let signed = data_bytes(value(&signing, unsafe { kSecCodeInfoUnique }));
        if signed.as_deref() != Some(&running[..]) {
            return Err(SignatureError::NotValid);
        }

        // SAFETY: the keys are constants the framework exports.
        let (identifier, team, certificates) = unsafe {
            (
                value(&signing, kSecCodeInfoIdentifier),
                value(&signing, kSecCodeInfoTeamIdentifier),
                value(&signing, kSecCodeInfoCertificates),
            )
        };
        let identifier = rust_string(identifier).ok_or(SignatureError::NotValid)?;
        Ok((
            identifier,
            rust_string(team),
            leaf_subject(certificates),
            apple,
        ))
    }

    /// A Kettle requirement constant, compiled.
    fn parse(requirement: Requirement) -> Result<Owned, SignatureError> {
        let text = cf_string(requirement.text()).ok_or(SignatureError::Os)?;
        let mut parsed: CFTypeRef = std::ptr::null();
        // SAFETY: `text` is a live string of a requirement Kettle's own tests
        // parse; `parsed` receives an owned requirement on success.
        let status =
            unsafe { SecRequirementCreateWithString(text.0, SEC_CS_DEFAULT_FLAGS, &mut parsed) };
        copied(status, parsed)
    }

    /// The subject summary of the first certificate, the signing leaf.
    fn leaf_subject(certificates: CFTypeRef) -> Option<String> {
        // SAFETY: a type-id query, which takes no ownership.
        if !is_a(certificates, unsafe { CFArrayGetTypeID() })
            // SAFETY: `certificates` is an array, checked first.
            || unsafe { CFArrayGetCount(certificates) } < 1
        {
            return None;
        }
        // SAFETY: the array holds at least one certificate, checked above;
        // the summary is an owned string or null.
        let summary = Owned::new(unsafe {
            SecCertificateCopySubjectSummary(CFArrayGetValueAtIndex(certificates, 0))
        })?;
        rust_string(summary.0)
    }

    #[cfg(test)]
    pub(super) fn parses(requirement: Requirement) -> bool {
        parse(requirement).is_ok()
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::{Requirement, Signature, SignatureError};
    use crate::process::ProcessIdentity;

    pub(super) fn signature(
        _: ProcessIdentity,
        _: Requirement,
    ) -> Result<Signature, SignatureError> {
        Err(SignatureError::Unsupported)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::process;

    /// A child running Apple's own `sleep`, stopped when dropped.
    struct Sleeper(std::process::Child);

    impl Sleeper {
        fn start() -> Self {
            Self(
                std::process::Command::new("/bin/sleep")
                    .arg("30")
                    .spawn()
                    .expect("start /bin/sleep"),
            )
        }

        fn identity(&self) -> ProcessIdentity {
            process::identity(self.0.id()).expect("the sleeper's identity")
        }
    }

    impl Drop for Sleeper {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn every_requirement_kettle_checks_parses() {
        for requirement in Requirement::ALL {
            assert!(imp::parses(requirement), "{requirement:?}");
        }
    }

    #[test]
    fn a_failure_is_the_codes_unless_the_process_left_or_the_os_failed() {
        assert_eq!(imp::failure(-67065), SignatureError::Gone, "no such code");
        assert_eq!(imp::failure(-108), SignatureError::Os, "allocation");
        assert_eq!(imp::failure(-67062), SignatureError::NotValid, "unsigned");
        assert_eq!(
            imp::failure(-67050),
            SignatureError::NotValid,
            "requirement"
        );
        // An expired certificate is a finding about the code, outside the
        // code-signing error range.
        assert_eq!(imp::failure(-2_147_409_654), SignatureError::NotValid);
    }

    #[test]
    fn a_null_reference_is_never_released() {
        // Releasing null halts the process; this would not return.
        assert!(imp::Owned::new(std::ptr::null()).is_none());
    }

    #[test]
    fn apples_own_code_meets_the_apple_issued_requirement() {
        let sleeper = Sleeper::start();
        let signature =
            signature(sleeper.identity(), Requirement::APPLE_ISSUED).expect("a valid signature");
        assert_eq!(signature.identifier, "com.apple.sleep");
        assert_eq!(signature.team, None, "Apple's platform code has no team");
        assert!(signature.apple);
        assert_eq!(
            signature.authority.as_deref(),
            Some("macOS Software Signing")
        );
        assert_eq!(
            std::fs::canonicalize(&signature.executable).unwrap(),
            std::fs::canonicalize("/bin/sleep").unwrap()
        );
    }

    #[test]
    fn a_requirement_the_code_does_not_meet_is_refused() {
        let sleeper = Sleeper::start();
        let anthropic =
            Requirement(r#"anchor apple generic and certificate leaf[subject.OU] = "Q6L2SF6YDW""#);
        assert!(imp::parses(anthropic));
        assert_eq!(
            signature(sleeper.identity(), anthropic),
            Err(SignatureError::NotValid)
        );
    }

    #[test]
    fn apples_own_code_is_not_claude_code() {
        let sleeper = Sleeper::start();
        assert_eq!(
            signature(sleeper.identity(), Requirement::CLAUDE_CODE),
            Err(SignatureError::NotValid)
        );
    }

    #[test]
    fn a_test_binary_is_not_apple_issued() {
        // Test binaries carry only the linker's ad-hoc signature.
        let me = process::current().expect("this process").identity;
        assert_eq!(
            signature(me, Requirement::APPLE_ISSUED),
            Err(SignatureError::NotValid)
        );
    }

    #[test]
    fn another_process_on_the_same_pid_is_gone() {
        let sleeper = Sleeper::start();
        let identity = sleeper.identity();
        let other = ProcessIdentity::new(identity.pid(), identity.start() + 1);
        assert_eq!(
            signature(other, Requirement::APPLE_ISSUED),
            Err(SignatureError::Gone)
        );
    }

    #[test]
    fn an_exec_changes_the_audit_token() {
        // A shell that execs another program keeps its pid and start time;
        // only the audit token's pid version says it changed.
        let mut shell = std::process::Command::new("/bin/sh")
            .args(["-c", "read line; exec /bin/sleep 30"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .expect("start a shell");
        let pid = shell.id();
        let before = imp::audit_token(pid).expect("the shell's token");
        let identity = process::identity(pid).unwrap();
        use std::io::Write as _;
        writeln!(shell.stdin.as_mut().unwrap()).unwrap();
        let sleep = std::fs::canonicalize("/bin/sleep").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while process::executable(identity)
            .ok()
            .and_then(|path| std::fs::canonicalize(path).ok())
            != Some(sleep.clone())
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the shell never exec'd"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let after = imp::audit_token(pid).expect("the program's token");
        assert_eq!(process::identity(pid).unwrap(), identity);
        assert_ne!(before, after);
        let _ = shell.kill();
        let _ = shell.wait();
    }
}
