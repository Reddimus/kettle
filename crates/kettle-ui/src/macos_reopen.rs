//! Reopen the installed macOS build without closing existing terminals.

#[cfg(any(target_os = "macos", test))]
fn installed_is_newer(running: &str, installed: &str) -> bool {
    let parse =
        |text: &str| kettle_update::Version::parse(text.strip_prefix('v').unwrap_or(text)).ok();
    let running = running.split_whitespace().next().and_then(parse);
    matches!((running, parse(installed)), (Some(running), Some(installed)) if installed > running)
}

#[cfg(any(target_os = "macos", test))]
fn read_metadata(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    const MAX_BYTES: u64 = 256 * 1024;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}

#[cfg(target_os = "macos")]
pub(crate) use imp::{finish, launch, request};

#[cfg(target_os = "macos")]
mod imp {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};

    use objc2_app_kit::{NSRunningApplication, NSWorkspace, NSWorkspaceOpenConfiguration};
    use objc2_foundation::{NSArray, NSDictionary, NSError, NSString, NSURL};
    use objc2_foundation_03::{
        NSData, NSPropertyListMutabilityOptions, NSPropertyListSerialization,
    };
    use winit::event_loop::EventLoopProxy;

    use crate::app::UserEvent;

    static PENDING: AtomicBool = AtomicBool::new(false);

    fn bundle_version(bytes: &[u8]) -> Option<String> {
        use objc2_foundation_03::{NSDictionary, NSString, ns_string};
        let data = NSData::with_bytes(bytes);
        // SAFETY: data owns its bytes for synchronous immutable decoding; the optional format
        // output is null.
        let value = unsafe {
            NSPropertyListSerialization::propertyListWithData_options_format_error(
                &data,
                NSPropertyListMutabilityOptions::Immutable,
                std::ptr::null_mut(),
            )
        }
        .ok()?;
        let dictionary = value.downcast::<NSDictionary>().ok()?;
        dictionary
            .objectForKey(ns_string!("CFBundleShortVersionString"))?
            .downcast::<NSString>()
            .ok()
            .map(|value| value.to_string())
    }

    fn newer_bundle(install: &kettle_update::InstallSnapshot, running: &str) -> Option<PathBuf> {
        let bundle = install.bundle_path()?;
        let bytes = super::read_metadata(&bundle.join("Contents/Info.plist")).ok()?;
        super::installed_is_newer(running, &bundle_version(&bytes)?).then(|| bundle.to_owned())
    }

    pub(crate) fn request(
        install: kettle_update::InstallSnapshot,
        running: &str,
        proxy: EventLoopProxy<UserEvent>,
    ) {
        if PENDING.swap(true, Ordering::AcqRel) {
            return;
        }
        let running = running.to_owned();
        if std::thread::Builder::new()
            .name("kettle-dock-reopen".into())
            .spawn(move || {
                objc2_06::rc::autoreleasepool(|_| {
                    if let Some(bundle) = newer_bundle(&install, &running)
                        && proxy.send_event(UserEvent::DockReopenReady(bundle)).is_ok()
                    {
                        return;
                    }
                    finish();
                });
            })
            .is_err()
        {
            finish();
            log::warn!("could not start installed-build lookup");
        }
    }

    pub(crate) fn finish() {
        PENDING.store(false, Ordering::Release);
    }

    pub(crate) fn launch(bundle: &Path, proxy: EventLoopProxy<UserEvent>) {
        use std::os::unix::ffi::OsStrExt as _;
        let Ok(path) = std::ffi::CString::new(bundle.as_os_str().as_bytes()) else {
            finish();
            return;
        };
        let argument = NSString::from_str("--reopen");
        let arguments = NSArray::from_vec(vec![argument]);
        let pairs: Vec<_> = [
            "XDG_CONFIG_HOME",
            "XDG_STATE_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
        ]
        .into_iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| (NSString::from_str(name), NSString::from_str(&value)))
        })
        .collect();
        let keys: Vec<_> = pairs.iter().map(|(key, _)| &**key).collect();
        let values: Vec<_> = pairs.iter().map(|(_, value)| value.clone()).collect();
        let environment = NSDictionary::from_vec(&keys, values);
        let completion =
            block2::RcBlock::new(move |app: *mut NSRunningApplication, error: *mut NSError| {
                let opened = !app.is_null() && error.is_null();
                if !opened {
                    log::warn!("could not reopen the installed Kettle build");
                }
                let _ = proxy.send_event(UserEvent::DockReopenFinished(opened));
            });
        // Force this bundle's executable instead of substituting the old running app.
        // SAFETY: path is a live NUL-terminated filesystem string; launch runs on the UI thread.
        // Foundation copies the URL and workspace retains the configuration and completion block.
        unsafe {
            let url = NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(
                std::ptr::NonNull::new(path.as_ptr().cast_mut())
                    .expect("CString has a non-null pointer"),
                true,
                None,
            );
            let configuration = NSWorkspaceOpenConfiguration::configuration();
            configuration.setCreatesNewApplicationInstance(true);
            configuration.setAllowsRunningApplicationSubstitution(false);
            configuration.setArguments(&arguments);
            configuration.setEnvironment(&environment);
            NSWorkspace::sharedWorkspace().openApplicationAtURL_configuration_completionHandler(
                &url,
                &configuration,
                Some(&completion),
            );
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn bundle_version_accepts_xml_and_rejects_non_string_versions() {
            assert_eq!(bundle_version(br#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleShortVersionString</key><string>5.5.2</string></dict></plist>"#).as_deref(), Some("5.5.2"));
            assert!(bundle_version(br#"<plist version="1.0"><dict><key>CFBundleShortVersionString</key><integer>5</integer></dict></plist>"#).is_none());
            assert!(bundle_version(b"not a plist").is_none());
        }

        #[test]
        fn bundle_version_accepts_binary_plists() {
            assert_eq!(
                bundle_version(include_bytes!("../tests/fixtures/reopen-version.plist")).as_deref(),
                Some("5.5.2")
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_strictly_newer_installed_version_launches() {
        assert!(installed_is_newer("5.5.1 (old)", "5.5.2"));
        assert!(installed_is_newer("v5.5.1", "5.5.2"));
        assert!(!installed_is_newer("5.5.2", "5.5.2"));
        assert!(!installed_is_newer("5.5.2", "5.5.1"));
        assert!(!installed_is_newer("5.5.2", "invalid"));
        assert!(!installed_is_newer("", "5.5.2"));
        for invalid in ["5.5.3 garbage", "5.5.3\nextra", "vv5.5.3", " 5.5.3"] {
            assert!(!installed_is_newer("5.5.2", invalid), "{invalid:?}");
        }
        assert!(!installed_is_newer("vv5.5.2", "5.5.3"));
    }

    #[test]
    fn metadata_reads_are_regular_and_bounded() {
        let root = kettle_test_support::private_tempdir("kettle-reopen-");
        let path = root.path().join("Info.plist");
        std::fs::write(&path, b"metadata").unwrap();
        assert_eq!(read_metadata(&path).unwrap(), b"metadata");
        std::fs::write(&path, vec![0; 256 * 1024 + 1]).unwrap();
        assert!(read_metadata(&path).is_err());
        assert!(read_metadata(root.path()).is_err());
        #[cfg(unix)]
        {
            let fifo = root.path().join("fifo");
            let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
            // SAFETY: name is a live NUL-terminated path for this synchronous mkfifo call.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            assert!(read_metadata(&fifo).is_err());
        }
    }
}
