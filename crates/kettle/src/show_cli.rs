//! `kettle show`: send a local image, SVG or Mermaid diagram to the media
//! shelf of the pane
//! this command runs in, in the Kettle it runs inside. It never reaches
//! another Kettle, never opens anything on screen, and never needs full
//! control: agent previews (`agent-display`) are enough.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use kettle_ctl::show::{SHOW_CALL_TIMEOUT, ShowRequest, ShowResult, ShowSource};
use kettle_ctl::{Client, CtlError};
use kettle_media::{ExternalAttested, FailureCode, NativePath};

use crate::ShowArgs;

/// Room left in the 1 MiB request line for everything but the params: the
/// envelope and the caller's claim.
const REQUEST_ENVELOPE_BYTES: usize = 1024;

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
        ShowSource::Image(read_stdin()?)
    } else {
        file_source(&args.source)?
    };
    let params = ShowRequest {
        source,
        title: args.title,
        key: args.key,
        pane: None,
        inline: None,
    }
    .into_params()
    .map_err(|failure| failure.model_message().to_string())?;
    let size = serde_json::to_vec(&params).map_or(usize::MAX, |bytes| bytes.len());
    if size > kettle_ctl::protocol::MAX_LINE_BYTES - REQUEST_ENVELOPE_BYTES {
        return Err(FailureCode::TooLarge.model_message().into());
    }
    let mut client = Client::discover_display(None).map_err(|error| failure_text(&error))?;
    let result = client
        .call_with_timeout("show", params, SHOW_CALL_TIMEOUT)
        .map_err(|error| failure_text(&error))?;
    serde_json::from_value(result)
        .map_err(|_| "Kettle answered in a form this command does not know.".into())
}

/// Bytes from stdin, refused once they could not fit one request.
fn read_stdin() -> Result<Vec<u8>, String> {
    let cap = (kettle_ctl::protocol::MAX_LINE_BYTES / 4) * 3;
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read stdin: {error}"))?;
    if bytes.len() > cap {
        return Err(FailureCode::TooLarge.model_message().into());
    }
    if bytes.is_empty() {
        return Err(FailureCode::BadParams.model_message().into());
    }
    Ok(bytes)
}

/// The file at `path`, made absolute, attested by the device and inode it
/// has now: Kettle refuses it if what it opens is another file.
pub(crate) fn file_source(path: &Path) -> Result<ShowSource, String> {
    let path: PathBuf = std::path::absolute(path)
        .map_err(|_| FailureCode::FileNotFound.model_message().to_string())?;
    let metadata = std::fs::metadata(&path).map_err(|error| {
        match error.kind() {
            std::io::ErrorKind::PermissionDenied => FailureCode::FilePermission,
            _ => FailureCode::FileNotFound,
        }
        .model_message()
        .to_string()
    })?;
    if !metadata.is_file() {
        return Err(FailureCode::FileNotRegular.model_message().into());
    }
    if path.to_str().is_none() {
        return Err("Media paths must be valid Unicode to send to Kettle.".into());
    }
    let native = NativePath::from_path(&path)
        .map_err(|_| FailureCode::BadParams.model_message().to_string())?;
    Ok(ShowSource::File {
        path: native,
        attestation: attestation(&metadata),
    })
}

#[cfg(unix)]
fn attestation(metadata: &std::fs::Metadata) -> ExternalAttested {
    use std::os::unix::fs::MetadataExt as _;
    ExternalAttested {
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}

/// Windows has no media worker, and Kettle refuses media there before it
/// looks at the file.
#[cfg(not(unix))]
fn attestation(_metadata: &std::fs::Metadata) -> ExternalAttested {
    ExternalAttested { dev: 0, ino: 0 }
}

/// What to say when Kettle did not take the media: the fixed wording for
/// each failure, which names no path or source and never suggests turning
/// on full control.
pub(crate) fn failure_text(error: &CtlError) -> String {
    match error {
        CtlError::NotInKettle => FailureCode::NotInKettle.model_message().into(),
        CtlError::NoServer | CtlError::Io(_) => FailureCode::DisplayDisabled.model_message().into(),
        CtlError::Server { code, .. } if code == "unknown_method" => {
            FailureCode::UnknownMethod.model_message().into()
        }
        CtlError::Server { message, .. } => message.clone(),
        CtlError::TimedOut | CtlError::Cancelled => {
            FailureCode::RenderTimeout.model_message().into()
        }
        CtlError::Protocol(_) | CtlError::Unusable(_) => {
            "Kettle answered in a form this command does not know.".into()
        }
    }
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
            (
                CtlError::Server {
                    code: "unknown_method".into(),
                    message: "unknown method 'show'".into(),
                },
                FailureCode::UnknownMethod,
            ),
        ] {
            let text = failure_text(&error);
            assert_eq!(text, expected.model_message());
            assert!(!text.contains("full"), "{text}");
        }
        let server = CtlError::Server {
            code: "file_refused".into(),
            message: FailureCode::FileNotFound.model_message().into(),
        };
        assert_eq!(
            failure_text(&server),
            FailureCode::FileNotFound.model_message()
        );
    }

    #[test]
    fn a_file_is_absolute_and_attested_and_others_are_refused() {
        let directory = kettle_test_support::private_tempdir("kettle-show-cli-");
        let file = directory.path().join("plot.png");
        std::fs::write(&file, b"png").unwrap();
        let ShowSource::File { path, .. } = file_source(&file).unwrap() else {
            panic!("a file source");
        };
        assert_eq!(path, NativePath::from_path(&file).unwrap());
        assert_eq!(
            file_source(directory.path()).unwrap_err(),
            FailureCode::FileNotRegular.model_message()
        );
        assert_eq!(
            file_source(&directory.path().join("missing.png")).unwrap_err(),
            FailureCode::FileNotFound.model_message()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let ShowSource::File { attestation, .. } = file_source(&file).unwrap() else {
                unreachable!()
            };
            let metadata = std::fs::metadata(&file).unwrap();
            assert_eq!(
                (attestation.dev, attestation.ino),
                (metadata.dev(), metadata.ino())
            );
        }
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
