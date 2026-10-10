//! `kettle show`: send a local image, SVG or Mermaid diagram to the media
//! shelf of the pane
//! this command runs in, in the Kettle it runs inside. It never reaches
//! another Kettle, never opens anything on screen, and never needs full
//! control: agent previews (`agent-display`) are enough.

use std::io::Read;
use std::path::{Path, PathBuf};

use kettle_ctl::show::{SHOW_CALL_TIMEOUT, ShowRequest, ShowResult, ShowSource};
use kettle_ctl::{Client, CtlError};
use kettle_media::{ExternalAttested, FailureCode, MAX_MERMAID_BYTES, NativePath};

use crate::ShowArgs;

/// Most bytes of other media read from stdin: what one request line holds
/// once they are base64-encoded. The whole request is checked before it is
/// sent, so a source near this is refused there with the same advice.
const MAX_STDIN_BYTES: usize = kettle_ctl::protocol::MAX_LINE_BYTES / 4 * 3;
/// What `kettle show` says when stdin could not be read.
const STDIN_UNREADABLE: &str = "Could not read the media from standard input.";
/// What it says when Kettle's reply was not one it knows.
const UNKNOWN_REPLY: &str = "Kettle answered in a form this command does not know.";
/// What it says when Kettle refused for a reason it does not know.
const UNKNOWN_REFUSAL: &str = "Kettle refused the media for a reason this command does not know.";

/// Run `kettle show …`; returns the process exit code (0 shown, 1 not).
pub fn run_show(args: ShowArgs) -> i32 {
    match show(args) {
        Ok(result) => {
            println!("{}", confirmation(&result));
            0
        }
        Err(message) => {
            eprintln!("kettle show: {message}");
            1
        }
    }
}

fn show(args: ShowArgs) -> Result<ShowResult, String> {
    let source = if args.source == Path::new("-") {
        stdin_source(&mut std::io::stdin().lock(), args.mermaid)?
    } else {
        file_source(&args.source, args.mermaid)?
    };
    let params = request_params(ShowRequest {
        source,
        title: args.title,
        key: args.key,
        pane: None,
        inline: None,
    })?;
    let mut client = Client::discover_display(None).map_err(|error| failure_text(&error))?;
    let result = client
        .call_with_timeout("show", params, SHOW_CALL_TIMEOUT)
        .map_err(|error| failure_text(&error))?;
    serde_json::from_value(result).map_err(|_| UNKNOWN_REPLY.into())
}

/// `request`'s params, refused as Kettle would refuse them, and refused as
/// too large when the whole request a client frames, its escapes and base64
/// included, would not fit one line: a file path carries any size.
pub(crate) fn request_params(request: ShowRequest) -> Result<serde_json::Value, String> {
    let params = request
        .into_params()
        .map_err(|failure| failure.model_message().to_string())?;
    if !kettle_ctl::protocol::request_fits("show", &params) {
        return Err(FailureCode::TooLarge.model_message().into());
    }
    Ok(params)
}

/// What `input` holds, refused rather than truncated once it is more than
/// its kind may be: with `mermaid`, UTF-8 Mermaid text of at most 64 KiB;
/// else bytes the worker classifies, of at most [`MAX_STDIN_BYTES`].
fn stdin_source(input: &mut impl Read, mermaid: bool) -> Result<ShowSource, String> {
    let cap = if mermaid {
        MAX_MERMAID_BYTES
    } else {
        MAX_STDIN_BYTES
    };
    let mut bytes = Vec::new();
    input
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| STDIN_UNREADABLE.to_string())?;
    if bytes.len() > cap {
        return Err(FailureCode::TooLarge.model_message().into());
    }
    if bytes.is_empty() {
        return Err(FailureCode::BadParams.model_message().into());
    }
    if mermaid {
        let text = String::from_utf8(bytes).map_err(|_| FailureCode::BadParams.model_message())?;
        return Ok(ShowSource::Mermaid(text));
    }
    Ok(ShowSource::Image(bytes))
}

/// The file at `path`, made absolute, bounded before the file system is
/// asked anything, and attested by the device and inode of what this
/// command opens there: Kettle's worker reads only that file, so it shows
/// nothing this command could not read itself. With `mermaid` it renders
/// as a diagram.
pub(crate) fn file_source(path: &Path, mermaid: bool) -> Result<ShowSource, String> {
    // As given, before `absolute` drops its `.` parts: a spelling too long
    // for Kettle is refused however short it would come out.
    if native_len(path) > kettle_media::MAX_PATH_BYTES {
        return Err(FailureCode::BadParams.model_message().into());
    }
    let path: PathBuf = std::path::absolute(path)
        .map_err(|_| FailureCode::FileNotFound.model_message().to_string())?;
    if path.to_str().is_none() {
        return Err("Media paths must be valid Unicode to send to Kettle.".into());
    }
    let native = NativePath::from_path(&path)
        .map_err(|_| FailureCode::BadParams.model_message().to_string())?;
    let attestation = attest(&path).map_err(|failure| failure.model_message().to_string())?;
    Ok(ShowSource::File {
        path: native,
        attestation,
        mermaid,
    })
}

/// The bytes `path` takes as Kettle spells a native path: UTF-16 on
/// Windows, its own bytes elsewhere.
fn native_len(path: &Path) -> usize {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        path.as_os_str().encode_wide().count() * 2
    }
    #[cfg(not(windows))]
    {
        path.as_os_str().len()
    }
}

/// Open `path` as the worker will, read-only, without waiting on a pipe and
/// never as a controlling terminal, and attest what was opened if it is a
/// regular file. A file this command may look up but not read is refused
/// here, as the worker would refuse it.
#[cfg(unix)]
fn attest(path: &Path) -> Result<ExternalAttested, FailureCode> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::PermissionDenied => FailureCode::FilePermission,
            _ => FailureCode::FileNotFound,
        })?;
    let opened = file.metadata().map_err(|_| FailureCode::FileNotFound)?;
    if !opened.file_type().is_file() {
        return Err(FailureCode::FileNotRegular);
    }
    Ok(ExternalAttested {
        dev: opened.dev(),
        ino: opened.ino(),
    })
}

/// Windows has no media worker, and Kettle refuses media there before it
/// looks at the file.
#[cfg(not(unix))]
fn attest(path: &Path) -> Result<ExternalAttested, FailureCode> {
    let metadata = std::fs::metadata(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::PermissionDenied => FailureCode::FilePermission,
        _ => FailureCode::FileNotFound,
    })?;
    if !metadata.is_file() {
        return Err(FailureCode::FileNotRegular);
    }
    Ok(ExternalAttested { dev: 0, ino: 0 })
}

/// What to say when Kettle did not take the media: the fixed wording for
/// each failure, which names no path or source and never suggests turning
/// on full control. Kettle names a refusal by its code and reason; its
/// words, which a stale or foreign server could make anything, are not
/// repeated.
pub(crate) fn failure_text(error: &CtlError) -> String {
    let failure = match error {
        CtlError::NotInKettle => FailureCode::NotInKettle,
        CtlError::NoServer | CtlError::Io(_) => FailureCode::DisplayDisabled,
        CtlError::Server { code, reason, .. } => {
            match FailureCode::from_wire(code, reason.as_deref()) {
                Some(failure) => failure,
                None => return UNKNOWN_REFUSAL.into(),
            }
        }
        CtlError::TimedOut | CtlError::Cancelled => FailureCode::RenderTimeout,
        CtlError::Protocol(_) | CtlError::Unusable(_) => return UNKNOWN_REPLY.into(),
    };
    failure.model_message().into()
}

fn confirmation(result: &ShowResult) -> String {
    let sender = if result.verified {
        ""
    } else {
        ", from an unverified sender"
    };
    format!(
        "Sent to the Kettle media shelf of pane {} ({} {}x{}{sender}). You have not seen its contents.",
        result.pane, result.kind, result.width, result.height
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(code: &str, reason: Option<&str>, message: &str) -> CtlError {
        CtlError::Server {
            code: code.into(),
            message: message.into(),
            reason: reason.map(Into::into),
        }
    }

    #[test]
    fn failures_use_fixed_wording_that_never_suggests_full_control() {
        for (error, expected) in [
            (CtlError::NotInKettle, FailureCode::NotInKettle),
            (CtlError::NoServer, FailureCode::DisplayDisabled),
            (
                CtlError::Io(std::io::Error::other("refused")),
                FailureCode::DisplayDisabled,
            ),
            (CtlError::TimedOut, FailureCode::RenderTimeout),
            (CtlError::Cancelled, FailureCode::RenderTimeout),
            (
                server("unknown_method", None, "unknown method 'show'"),
                FailureCode::UnknownMethod,
            ),
        ] {
            let text = failure_text(&error);
            assert_eq!(text, expected.model_message());
            assert!(!text.contains("full"), "{text}");
        }
    }

    /// Every refusal Kettle sends is said in this command's own words, by its
    /// code and reason; whatever words came with it, and a code or reason it
    /// does not know, are never repeated.
    #[test]
    fn a_refusal_is_said_by_its_code_never_by_the_servers_words() {
        let hostile = "\u{1b}]52;c;aGk=\u{7}/home/user/secret.png";
        for failure in FailureCode::ALL {
            let text = failure_text(&server(failure.code(), failure.reason(), hostile));
            assert_eq!(text, failure.model_message(), "{failure:?}");
        }
        for (code, reason) in [
            ("file_refused", None),
            ("file_refused", Some("gone")),
            ("render_failed", Some("timeout\u{1b}")),
            ("no_such_code", None),
        ] {
            assert_eq!(
                failure_text(&server(code, reason, hostile)),
                UNKNOWN_REFUSAL
            );
        }
        assert_eq!(
            failure_text(&CtlError::Protocol(hostile.into())),
            UNKNOWN_REPLY
        );
        assert_eq!(
            failure_text(&CtlError::Unusable(hostile.into())),
            UNKNOWN_REPLY
        );
    }

    /// Stdin is refused rather than truncated: Mermaid at 64 KiB is taken
    /// and one byte more is too large, as other media is at what a request
    /// carries; empty input, Mermaid that is not UTF-8, and a read that
    /// fails are refused in fixed words.
    #[test]
    fn stdin_is_bounded_to_the_byte_and_never_truncated() {
        let read = |bytes: Vec<u8>, mermaid| stdin_source(&mut bytes.as_slice(), mermaid);
        let at = vec![b'%'; MAX_MERMAID_BYTES];
        assert_eq!(
            read(at.clone(), true).unwrap(),
            ShowSource::Mermaid(String::from_utf8(at.clone()).unwrap())
        );
        let mut over = at;
        over.push(b'%');
        assert_eq!(
            read(over, true).unwrap_err(),
            FailureCode::TooLarge.model_message()
        );
        assert_eq!(
            read(vec![0; MAX_STDIN_BYTES], false).unwrap(),
            ShowSource::Image(vec![0; MAX_STDIN_BYTES])
        );
        assert_eq!(
            read(vec![0; MAX_STDIN_BYTES + 1], false).unwrap_err(),
            FailureCode::TooLarge.model_message()
        );
        for mermaid in [false, true] {
            assert_eq!(
                read(Vec::new(), mermaid).unwrap_err(),
                FailureCode::BadParams.model_message()
            );
        }
        assert_eq!(
            read(vec![b'g', 0xff], true).unwrap_err(),
            FailureCode::BadParams.model_message()
        );
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("/home/user/secret.png: gone"))
            }
        }
        assert_eq!(
            stdin_source(&mut Broken, false).unwrap_err(),
            STDIN_UNREADABLE
        );
    }

    /// The whole request is checked as a client frames it: inline media
    /// whose base64 fills the line is refused with the advice to pass a
    /// file path, and Mermaid, escapes and all, always fits.
    #[test]
    fn the_whole_request_is_bounded_with_its_encoding() {
        let request = |source| ShowRequest {
            source,
            title: Some("\"".repeat(kettle_ctl::show::MAX_SHOW_TITLE_BYTES)),
            key: None,
            pane: None,
            inline: None,
        };
        assert_eq!(
            request_params(request(ShowSource::Image(vec![0; MAX_STDIN_BYTES]))).unwrap_err(),
            FailureCode::TooLarge.model_message()
        );
        let escapes = "\u{1}".repeat(MAX_MERMAID_BYTES);
        assert!(request_params(request(ShowSource::Mermaid(escapes))).is_ok());
        // The largest image that fits, and one byte more: base64 grows in
        // steps of three bytes, so the edge is found by search.
        let fits = |len: usize| request_params(request(ShowSource::Image(vec![0; len]))).is_ok();
        let edge = (0..MAX_STDIN_BYTES)
            .rev()
            .step_by(3)
            .find(|&len| fits(len))
            .unwrap();
        let edge = (edge..edge + 3)
            .take_while(|&len| fits(len))
            .last()
            .unwrap();
        assert!(fits(edge) && !fits(edge + 1));
        let line = |len: usize| {
            let params = request(ShowSource::Image(vec![0; len]))
                .into_params()
                .unwrap();
            serde_json::to_vec(&kettle_ctl::protocol::Request {
                v: kettle_ctl::protocol::PROTOCOL_VERSION,
                id: u64::MAX,
                method: "show".into(),
                params,
                caller: Some(kettle_ctl::protocol::PeerClaim::WIDEST),
            })
            .unwrap()
            .len()
        };
        assert!(line(edge) <= kettle_ctl::protocol::MAX_LINE_BYTES);
        assert!(line(edge + 3) > kettle_ctl::protocol::MAX_LINE_BYTES);
    }

    #[test]
    fn a_file_is_absolute_and_attested_and_others_are_refused() {
        let directory = kettle_test_support::private_tempdir("kettle-show-cli-");
        let file = directory.path().join("plot.png");
        std::fs::write(&file, b"png").unwrap();
        let ShowSource::File { path, mermaid, .. } = file_source(&file, false).unwrap() else {
            panic!("a file source");
        };
        assert_eq!(path, NativePath::from_path(&file).unwrap());
        assert!(!mermaid);
        let ShowSource::File { mermaid: true, .. } = file_source(&file, true).unwrap() else {
            panic!("a Mermaid file source");
        };
        assert_eq!(
            file_source(directory.path(), false).unwrap_err(),
            FailureCode::FileNotRegular.model_message()
        );
        assert_eq!(
            file_source(&directory.path().join("missing.png"), false).unwrap_err(),
            FailureCode::FileNotFound.model_message()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let ShowSource::File { attestation, .. } = file_source(&file, false).unwrap() else {
                unreachable!()
            };
            let metadata = std::fs::metadata(&file).unwrap();
            assert_eq!(
                (attestation.dev, attestation.ino),
                (metadata.dev(), metadata.ino())
            );
        }
    }

    /// The attestation is of what the command could open: a file it may
    /// look up but not read is refused as the worker would refuse it; a
    /// named pipe is refused at once, never waited on for a writer; a
    /// symbolic link is attested by the file it leads to and sent by its
    /// own name.
    #[cfg(unix)]
    #[test]
    fn a_file_is_attested_by_opening_it_as_the_worker_will() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let directory = kettle_test_support::private_tempdir("kettle-show-cli-");
        let file = directory.path().join("plot.png");
        std::fs::write(&file, b"png").unwrap();
        let link = directory.path().join("link.png");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        let ShowSource::File {
            path, attestation, ..
        } = file_source(&link, false).unwrap()
        else {
            panic!("a file source");
        };
        assert_eq!(path, NativePath::from_path(&link).unwrap());
        let target = std::fs::metadata(&file).unwrap();
        assert_eq!(
            (attestation.dev, attestation.ino),
            (target.dev(), target.ino())
        );
        let pipe = directory.path().join("pipe.png");
        let name = std::ffi::CString::new(pipe.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (sent, opened) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sent.send(file_source(&pipe, false));
        });
        let opened = opened
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("a named pipe is refused without waiting for a writer");
        assert_eq!(
            opened.unwrap_err(),
            FailureCode::FileNotRegular.model_message()
        );
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipped the unreadable-file case: root reads past mode bits");
            return;
        }
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o200)).unwrap();
        assert!(
            std::fs::metadata(&file).is_ok(),
            "it can still be looked up"
        );
        assert_eq!(
            file_source(&file, false).unwrap_err(),
            FailureCode::FilePermission.model_message()
        );
    }

    /// A path too long is refused before anything asks the file system
    /// about it: it names no file that could be missing.
    #[cfg(unix)]
    #[test]
    fn a_path_too_long_is_refused_before_the_file_system_sees_it() {
        let directory = kettle_test_support::private_tempdir("kettle-show-cli-");
        let base = directory.path().join("missing");
        let base_len = base.as_os_str().len();
        let long = base.join("n".repeat(kettle_media::MAX_PATH_BYTES + 1 - base_len - 1));
        assert_eq!(long.as_os_str().len(), kettle_media::MAX_PATH_BYTES + 1);
        assert_eq!(
            file_source(&long, false).unwrap_err(),
            FailureCode::BadParams.model_message()
        );
        let at = base.join("n".repeat(kettle_media::MAX_PATH_BYTES - base_len - 1));
        assert_eq!(
            file_source(&at, false).unwrap_err(),
            FailureCode::FileNotFound.model_message(),
            "at the cap, the file system is asked"
        );
        // A spelling over the cap that would shorten once made absolute.
        let file = directory.path().join("plot.png");
        std::fs::write(&file, b"png").unwrap();
        let dots = "./".repeat(kettle_media::MAX_PATH_BYTES / 2);
        let spelled = directory.path().join(format!("{dots}plot.png"));
        assert!(spelled.as_os_str().len() > kettle_media::MAX_PATH_BYTES);
        assert!(std::path::absolute(&spelled).unwrap().as_os_str().len() < 4096);
        assert_eq!(
            file_source(&spelled, false).unwrap_err(),
            FailureCode::BadParams.model_message()
        );
    }

    /// A path is measured as Kettle spells it: one unit for each of its
    /// bytes elsewhere, two for each UTF-16 unit on Windows.
    #[test]
    fn a_path_is_measured_as_kettle_spells_it() {
        let path = Path::new("caf\u{e9}");
        let want = if cfg!(windows) { 8 } else { 5 };
        assert_eq!(native_len(path), want);
    }

    #[test]
    fn the_confirmation_names_the_pane_and_says_nothing_was_seen() {
        let result = ShowResult {
            pane: 3,
            verified: false,
            window: 1,
            item: 9,
            kind: "svg".into(),
            width: 640,
            height: 480,
            warnings: vec![],
            inline: None,
        };
        let text = confirmation(&result);
        assert!(text.contains("pane 3") && text.contains("svg 640x480"));
        assert!(text.contains("unverified") && text.contains("not seen"));
    }
}
