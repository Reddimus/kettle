use super::{production_source, take_opener_slot};

#[cfg(unix)]
#[test]
fn a_custom_file_handler_is_reaped_after_exit() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::{Duration, Instant};

    let root = tempfile::tempdir().unwrap();
    let spaced = root.path().join("handler directory");
    std::fs::create_dir(&spaced).unwrap();
    let root = std::fs::canonicalize(&spaced).unwrap();
    let handler = root.join("handler");
    let pid_file = root.join("pid");
    std::fs::write(&pid_file, b"").unwrap();
    std::fs::write(
        &handler,
        b"#!/bin/sh\nprintf '%s\\n' \"$$\" > \"${0%/*}/pid\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&handler, std::fs::Permissions::from_mode(0o700)).unwrap();

    super::open_checked_file(&pid_file, Some(handler.to_str().unwrap()));
    let deadline = Instant::now() + Duration::from_secs(5);
    let pid: libc::pid_t = loop {
        let text = std::fs::read_to_string(&pid_file).unwrap();
        if text.ends_with('\n') {
            break text.trim().parse().unwrap();
        }
        assert!(Instant::now() < deadline, "handler did not publish its pid");
        std::thread::sleep(Duration::from_millis(10));
    };
    let reaped = loop {
        // SAFETY: signal zero only queries the child whose pid the handler wrote.
        if unsafe { libc::kill(pid, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if !reaped {
        // SAFETY: clean up only this handler's exited child if the regression returns.
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
    }
    assert!(reaped, "the custom handler was left running or unreaped");
}

/// A link to a program or a shortcut does not open: by extension, or a
/// file with no extension that is executable or begins like a shortcut.
/// A document stays openable even with its executable bits set, as on
/// FAT and exFAT volumes, and so does a folder, even one named like a
/// script. A missing file resolves to nothing.
#[cfg(unix)]
#[test]
fn programs_are_refused_and_documents_and_folders_open() {
    use super::{FileLinkCheck, check_file_link};
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let file = |name: &str, mode: u32, body: &[u8]| {
        let path = root_path.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    };
    let refused =
        |path: &std::path::Path| matches!(check_file_link(path), FileLinkCheck::Program(_));
    assert!(
        refused(&file("tool", 0o755, b"x")),
        "an executable with no extension"
    );
    assert!(
        refused(&file("build.sh", 0o644, b"x")),
        "a script by its extension"
    );
    assert!(refused(&file("Setup.EXE", 0o644, b"x")));
    assert!(
        refused(&file(
            "Calculator alias",
            0o644,
            b"book\0\0\0\0mark\0\0\0\0rest"
        )),
        "a macOS alias"
    );
    assert!(
        refused(&file("launcher", 0o644, b"[Desktop Entry]\nExec=x\n")),
        "a Linux launcher"
    );
    assert!(
        refused(&file(
            "commented",
            0o644,
            b"\xEF\xBB\xBF# made by hand\n\n  # more\r\n[Desktop Entry]\nExec=x\n"
        )),
        "a launcher behind a byte-order mark, comments and blank lines"
    );
    let config = file("settings", 0o644, b"# Config File\n[settings]\nkey=1\n");
    assert_eq!(
        check_file_link(&config),
        FileLinkCheck::Open(config.clone()),
        "a config file whose first group is not a desktop entry"
    );
    assert!(
        refused(&file("report.pdf", 0o644, b"book\0\0\0\0mark\0\0\0\0rest")),
        "a macOS alias renamed like a document"
    );
    assert!(
        refused(&file("a.out", 0o755, b"x")),
        "an executable with an unknown extension"
    );
    let data = file("archive.dat", 0o644, b"x");
    assert_eq!(
        check_file_link(&data),
        FileLinkCheck::Open(data.clone()),
        "the same name, not executable"
    );
    let plain = file("plain", 0o644, b"text");
    assert_eq!(check_file_link(&plain), FileLinkCheck::Open(plain.clone()));
    let notes = file("notes.md", 0o777, b"# notes");
    assert_eq!(
        check_file_link(&notes),
        FileLinkCheck::Open(notes.clone()),
        "a document on exFAT"
    );
    assert_eq!(
        check_file_link(&root_path),
        FileLinkCheck::Open(root_path.clone()),
        "a folder"
    );
    let scripts = root_path.join("archive.sh");
    std::fs::create_dir(&scripts).unwrap();
    assert_eq!(
        check_file_link(&scripts),
        FileLinkCheck::Open(scripts.clone()),
        "a folder named like a script"
    );
    let bundle = root_path.join("Calculator.app");
    std::fs::create_dir(&bundle).unwrap();
    assert!(refused(&bundle), "an application bundle");
    let unnamed_bundle = root_path.join("Unnamed");
    std::fs::create_dir_all(unnamed_bundle.join("Contents")).unwrap();
    std::fs::write(unnamed_bundle.join("Contents").join("Info.plist"), b"x").unwrap();
    assert!(refused(&unnamed_bundle), "a bundle by its contents");
    assert_eq!(
        check_file_link(&root_path.join("missing")),
        FileLinkCheck::Unresolved
    );
}

/// The opener follows symlinks, so the check does too: a document name
/// linking to a program, or a plain name linking to a bundle, is refused,
/// and a link to a document opens the document itself.
#[cfg(unix)]
#[test]
fn a_symlink_is_judged_by_what_it_resolves_to() {
    use super::{FileLinkCheck, check_file_link};
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let root = tempfile::tempdir().unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let program = root_path.join("evil");
    std::fs::write(&program, b"x").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bundle = root_path.join("Calculator.app");
    std::fs::create_dir(&bundle).unwrap();
    let document = root_path.join("real.pdf");
    std::fs::write(&document, b"%PDF").unwrap();
    for (link, target) in [
        ("notes.md", &program),
        ("readme", &bundle),
        ("doc.pdf", &bundle),
    ] {
        let path = root_path.join(link);
        symlink(target, &path).unwrap();
        assert!(
            matches!(check_file_link(&path), FileLinkCheck::Program(_)),
            "{link}"
        );
    }
    let to_document = root_path.join("latest.pdf");
    symlink(&document, &to_document).unwrap();
    assert_eq!(check_file_link(&to_document), FileLinkCheck::Open(document));
    // A link named like a program is refused whatever it points at.
    let named = root_path.join("run.command");
    symlink(root_path.join("real.pdf"), &named).unwrap();
    assert!(matches!(check_file_link(&named), FileLinkCheck::Program(_)));
}

/// Openers are bounded: a stalled path cannot pile up a thread per click,
/// and a finished one frees its slot.
#[test]
fn file_link_openers_are_bounded() {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let held = (0..3)
        .map(|_| take_opener_slot(&COUNTER, 3).expect("a free slot"))
        .collect::<Vec<_>>();
    assert!(
        take_opener_slot(&COUNTER, 3).is_none(),
        "all slots are taken"
    );
    drop(held);
    assert!(take_opener_slot(&COUNTER, 3).is_some());
    assert_eq!(COUNTER.load(std::sync::atomic::Ordering::Acquire), 0);
}

/// A file link is decided before anything opens it, the custom handler
/// included, and off the UI thread; one that does not decode is refused
/// rather than handed on. The opener and the handler get the resolved
/// path, never the link's own text.
#[test]
fn file_links_are_checked_off_the_ui_thread_before_any_opener() {
    let src = production_source();
    let body = src
        .split("fn open_url(&mut self, ws: &WindowState, uri: &str, source: LinkSource) {")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("open_url");
    let local = body
        .find("let Some(path) = kettle_core::links::local_file_path(uri) else {")
        .expect("open_url decodes a local file link");
    let refuse = body[local..]
        .find("return;")
        .expect("an undecodable link is refused")
        + local;
    let spawn = body
        .find("spawn_file_link_opener(path, handler, self.ui_text);")
        .expect("a spawned opener");
    let custom = body
        .find("if self.cfg.use_custom_url_handler")
        .expect("the custom handler for other links");
    assert!(local < refuse && refuse < spawn && spawn < custom);

    let opener = src
        .split("fn spawn_file_link_opener(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("spawn_file_link_opener");
    let thread = opener.find(".spawn(move ||").expect("its own thread");
    let check = opener.find("check_file_link(&path)").expect("the check");
    let open = opener
        .find("open_checked_file(&resolved, handler.as_deref())")
        .expect("the open");
    assert!(thread < check && check < open);

    let open_checked = src
        .split("fn open_checked_file(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("open_checked_file");
    assert!(open_checked.contains("file_url_for_path(resolved)"));
    assert!(open_checked.contains("open::that_detached(resolved)"));
    assert!(
        !open_checked.contains("uri"),
        "the link text never reaches an opener"
    );
}

/// A classic Finder alias keeps its data in the resource fork; its
/// Finder flag still marks it, whatever its name.
#[cfg(target_os = "macos")]
#[test]
fn a_finder_alias_is_refused_by_its_flag() {
    use super::{FileLinkCheck, check_file_link};
    use std::os::unix::ffi::OsStrExt as _;
    let root = tempfile::tempdir().unwrap();
    let alias = std::fs::canonicalize(root.path())
        .unwrap()
        .join("report.pdf");
    std::fs::write(&alias, b"").unwrap();
    let mut info = [0u8; 32];
    info[8] = 0x80; // kIsAlias
    let c_path = std::ffi::CString::new(alias.as_os_str().as_bytes()).unwrap();
    // SAFETY: both names are NUL-terminated and `info` is readable for its length.
    let set = unsafe {
        libc::setxattr(
            c_path.as_ptr(),
            c"com.apple.FinderInfo".as_ptr(),
            info.as_ptr().cast(),
            info.len(),
            0,
            0,
        )
    };
    assert_eq!(set, 0, "set the Finder info");
    assert!(matches!(check_file_link(&alias), FileLinkCheck::Program(_)));
}

/// Win32 drops a trailing dot or space and opens reserved names as
/// devices, so a verbatim path holding either is not kept.
#[test]
fn a_verbatim_path_must_read_the_same_to_win32() {
    use super::win32_keeps_path;
    assert!(win32_keeps_path(r"C:\docs\report.pdf"));
    assert!(win32_keeps_path(r"C:\docs\console.txt"));
    assert!(win32_keeps_path(r"C:\"));
    for path in [
        r"C:\docs\report.pdf ",
        r"C:\docs.\a.pdf",
        r"C:\docs\CON",
        r"C:\docs\nul.txt",
        r"C:\docs\com1",
        r"C:\docs\LPT9.log",
        r"C:\docs\aux .txt",
    ] {
        assert!(!win32_keeps_path(path), "{path}");
    }
}

/// On Windows a resolved path is kept only as a local drive path.
#[cfg(windows)]
#[test]
fn a_resolved_windows_path_must_be_a_local_drive() {
    use super::local_resolved_path;
    use std::path::PathBuf;
    assert_eq!(
        local_resolved_path(PathBuf::from(r"\\?\C:\Users\me\a.pdf")),
        Some(PathBuf::from(r"C:\Users\me\a.pdf"))
    );
    assert_eq!(
        local_resolved_path(PathBuf::from(r"\\?\UNC\host\share\a.pdf")),
        None
    );
    assert_eq!(
        local_resolved_path(PathBuf::from(r"\\?\Volume{0000}\a.pdf")),
        None
    );
}
