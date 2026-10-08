#![cfg(feature = "test-worker")]
mod common;
use kettle_media::{wire::*, *};
use std::io::Write;
use std::process::{Child, Command, Stdio};

struct OwnedChild(Option<Child>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn exchange(input: &[u8]) -> Vec<u8> {
    let mut guard = OwnedChild(Some(
        Command::new(env!("CARGO_BIN_EXE_media-test-worker"))
            .env_clear()
            .env("LC_ALL", "C")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    ));
    let child = guard.0.as_mut().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input).unwrap();
    drop(stdin);
    // Keep ownership in the guard until explicit reap, including on assertion failure.
    let mut stdout = child.stdout.take().unwrap();
    let mut result = Vec::new();
    std::io::Read::read_to_end(&mut stdout, &mut result).unwrap();
    let status = child.wait().unwrap();
    guard.0.take();
    assert!(status.success());
    result
}
fn encoded(f: Frame, d: Direction) -> Vec<u8> {
    encode(&f, d).unwrap()
}
fn prefix() -> Vec<u8> {
    encoded(Frame::Hello(common::hello()), Direction::ParentToWorker)
}
fn expected_reply(code: FailureCode) -> Vec<u8> {
    let mut b = encoded(Frame::Ready(common::ready()), Direction::WorkerToParent);
    b.extend(encoded(
        Frame::Failure(Failure { code }),
        Direction::WorkerToParent,
    ));
    b
}
#[test]
fn raster_exact_replies_and_reap() {
    let mut input = prefix();
    input.extend(encoded(
        Frame::Job(common::job()),
        Direction::ParentToWorker,
    ));
    let mut expected = encoded(Frame::Ready(common::ready()), Direction::WorkerToParent);
    expected.extend(encoded(
        Frame::Rendered(common::rendered()),
        Direction::WorkerToParent,
    ));
    assert_eq!(exchange(&input), expected);
}
#[test]
fn handshake_mismatch() {
    let mut hello = common::hello();
    hello.build_id.source_hash = "ffff".into();
    assert_eq!(
        exchange(&encoded(Frame::Hello(hello), Direction::ParentToWorker)),
        encoded(
            Frame::Failure(Failure {
                code: FailureCode::RestartRequired
            }),
            Direction::WorkerToParent
        )
    );
}
#[test]
fn oversize_frame() {
    let mut input = prefix();
    let mut header = MAGIC.to_vec();
    header.extend(PROTOCOL_VERSION.to_le_bytes());
    header.push(4);
    header.extend(u32::MAX.to_le_bytes());
    input.extend(header);
    assert_eq!(exchange(&input), expected_reply(FailureCode::TooLarge));
}
#[test]
fn trailing_payload_bytes() {
    let mut input = prefix();
    let mut job = encoded(Frame::Job(common::job()), Direction::ParentToWorker);
    let n = u32::from_le_bytes(job[7..11].try_into().unwrap());
    job[7..11].copy_from_slice(&(n + 1).to_le_bytes());
    job.push(0);
    input.extend(job);
    assert_eq!(exchange(&input), expected_reply(FailureCode::BadParams));
}
#[test]
fn index_out_of_range() {
    for index in [1, 32] {
        let mut input = prefix();
        let mut j = common::job();
        j.kind = JobKind::MarkdownDiagrams { index: 1 };
        let mut job = encoded(Frame::Job(j), Direction::ParentToWorker);
        job[12] = index;
        input.extend(job);
        assert_eq!(
            exchange(&input),
            expected_reply(FailureCode::IndexOutOfRange)
        );
    }
}
#[test]
fn malformed_truncation() {
    let mut input = prefix();
    input.extend(MAGIC);
    input.push(PROTOCOL_VERSION.to_le_bytes()[0]);
    assert_eq!(exchange(&input), expected_reply(FailureCode::BadParams));
}
