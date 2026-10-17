#[test]
fn reader_sidechannels_share_the_generation_ordered_output_gate() {
    let source = super::production_source();
    let reader_start = source
        .find(".name(\"kettle-pty-reader\"")
        .expect("PTY reader thread present");
    let reader_tail = &source[reader_start..];
    let reader_end = reader_tail
        .find("\n        Ok(Terminal {")
        .expect("PTY reader thread end");
    let reader = &reader_tail[..reader_end];

    assert!(
        !reader.contains("(waker)();"),
        "parser sidechannels must not bypass the per-pane output gate"
    );
    assert!(
        reader.contains("publish_output_if_ready("),
        "PTY reads must publish through the synchronized-output guard"
    );
    let helper_start = source
        .find("fn publish_output_if_ready(")
        .expect("output publication guard present");
    let helper_tail = &source[helper_start..];
    let helper_end = helper_tail
        .find("\n}\n\n")
        .map(|offset| offset + 2)
        .expect("output publication guard end");
    let helper = &helper_tail[..helper_end];
    let pending = helper
        .find("sync_timeout().is_some()")
        .expect("DEC 2026 pending-state check present");
    let generation = helper
        .find("out_gen.fetch_add(")
        .expect("output generation publication present");
    let wake = helper
        .find("output_wake.request();")
        .expect("gated output wake present");
    assert!(
        pending < generation && generation < wake,
        "pending sync must suppress publication; otherwise Release generation precedes wake"
    );
}

#[test]
fn pty_pump_spawn_failure_is_observable_and_closes_the_pane() {
    let source = super::production_source();
    let name = source
        .find(".name(\"kettle-pty-pump\"")
        .expect("PTY pump thread present");
    let start = source[..name]
        .rfind("if let Err(error)")
        .expect("PTY pump spawn error is handled");
    let end = source[name..]
        .find("\n                    loop {")
        .map(|offset| name + offset)
        .expect("outer reader loop follows pump creation");
    let spawn = &source[start..end];
    assert!(
        spawn.contains("log::error!(\"failed to spawn PTY pump thread: {error}\")")
            && spawn.contains("proxy.send_event_exit();")
            && spawn.contains("return;"),
        "thread exhaustion must leave an actionable diagnostic and a normal pane exit"
    );
    assert!(
        !spawn.contains("let _ = std::thread::Builder"),
        "pump creation errors must not be discarded"
    );
}
