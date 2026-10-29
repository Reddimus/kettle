pub(crate) fn descendant_pid(bytes: &[u8]) -> Option<u32> {
    String::from_utf8_lossy(bytes)
        .split("JOB_DESCENDANT ")
        .skip(1)
        .find_map(|tail| {
            let (line, _) = tail.split_once('\n')?;
            let digits = line.strip_suffix('\r').unwrap_or(line);
            if digits.bytes().all(|byte| byte.is_ascii_digit()) {
                digits.parse().ok()
            } else {
                None
            }
        })
}

#[test]
fn descendant_announcement_waits_for_the_complete_pid_line() {
    for line in [
        b"JOB_DESCENDANT 12345\r\n".as_slice(),
        b"JOB_DESCENDANT 12345\n".as_slice(),
    ] {
        for split in 0..line.len() {
            assert_eq!(
                descendant_pid(&line[..split]),
                None,
                "partial announcement at byte {split}"
            );
        }
        assert_eq!(descendant_pid(line), Some(12345));
    }
    assert_eq!(descendant_pid(b"JOB_DESCENDANT \n"), None);
    assert_eq!(descendant_pid(b"JOB_DESCENDANT 123x\n"), None);
    assert_eq!(descendant_pid(b"JOB_DESCENDANT 4294967296\n"), None);
    assert_eq!(
        descendant_pid(b"startup\r\nJOB_DESCENDANT 12345\r\nmore"),
        Some(12345)
    );
}
