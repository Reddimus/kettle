#![forbid(unsafe_code)]
//! Feature-gated test fixture. No renderer, filesystem reads, retries or production setup.
#[path = "../common/mod.rs"]
mod common;
use kettle_media::{wire::*, *};
use std::io;

fn error_code(e: WireError) -> FailureCode {
    match e {
        WireError::RestartRequired => FailureCode::RestartRequired,
        WireError::Validation(ValidationError::TooLarge) => FailureCode::TooLarge,
        WireError::Validation(ValidationError::IndexOutOfRange) => FailureCode::IndexOutOfRange,
        _ => FailureCode::BadParams,
    }
}
fn fail(out: &mut impl io::Write, code: FailureCode) -> Result<(), WireError> {
    write_frame(
        out,
        &Frame::Failure(Failure { code }),
        Direction::WorkerToParent,
    )
}
fn run() -> Result<(), WireError> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let hello = match read_frame(&mut input, Direction::ParentToWorker) {
        Ok(Some(Frame::Hello(h))) => h,
        Ok(None) => return Ok(()),
        Ok(_) => return fail(&mut output, FailureCode::BadParams),
        Err(e) => return fail(&mut output, error_code(e)),
    };
    // This fixed fixture has completed its only setup before emitting Ready.
    let ready = common::ready();
    if check_ready(&hello, &ready) != HandshakeOutcome::Compatible {
        return fail(&mut output, FailureCode::RestartRequired);
    }
    write_frame(&mut output, &Frame::Ready(ready), Direction::WorkerToParent)?;
    match read_frame(&mut input, Direction::ParentToWorker) {
        Ok(Some(Frame::Job(j))) if j.kind == JobKind::Raster => {
            let Source::Bytes(bytes) = j.source else {
                return fail(&mut output, FailureCode::UnsupportedMedia);
            };
            let mut result = common::rendered();
            result.digest = content_digest(&bytes, None).map_err(WireError::Validation)?;
            write_frame(
                &mut output,
                &Frame::Rendered(result),
                Direction::WorkerToParent,
            )
        }
        Ok(Some(Frame::Job(Job {
            kind: JobKind::MarkdownDiagrams { index },
            ..
        }))) if index >= 1 => fail(&mut output, FailureCode::IndexOutOfRange),
        Ok(Some(Frame::Job(_))) => fail(&mut output, FailureCode::UnsupportedMedia),
        Ok(None) => Ok(()),
        Ok(_) => fail(&mut output, FailureCode::BadParams),
        Err(e) => fail(&mut output, error_code(e)),
    }
}
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    if run().is_err() {
        std::process::exit(1);
    }
}
