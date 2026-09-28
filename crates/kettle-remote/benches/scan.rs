//! Cost of one remote-context poll. Kettle polls pane process trees on redraw,
//! so this cost repeats about twice a second while a cursor blinks.
//!
//! - `pane_rooted`: `RemoteScanner::refresh_roots` for one pane-like tree, the
//!   path the app uses. Linux and macOS walk only that tree.
//! - `whole_process_table`: `RemoteScanner::refresh`, the sysinfo snapshot of
//!   every process with argv and cwd. Platforms other than Linux and macOS use
//!   it for every poll.
//!
//! Run: `cargo bench -p kettle-remote`. The whole-table figure scales with the
//! number of processes on the machine, so compare runs on the same host.

use criterion::{Criterion, criterion_group, criterion_main};
use kettle_remote::RemoteScanner;

/// A shell with one child, in its own process group so the whole tree can be
/// stopped when the bench ends.
fn pane_stand_in() -> Option<std::process::Child> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        std::process::Command::new("/bin/sh")
            .args(["-c", "/bin/sleep 600 & wait"])
            .stdin(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .ok()
    }
    #[cfg(not(unix))]
    {
        None
    }
}

fn stop_stand_in(pane: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("/bin/kill")
            .args(["-KILL", &format!("-{}", pane.id())])
            .status();
    }
    let _ = pane.kill();
    let _ = pane.wait();
}

fn scans(c: &mut Criterion) {
    let mut pane = pane_stand_in();
    let root = pane
        .as_ref()
        .map_or(std::process::id(), std::process::Child::id);
    let mut group = c.benchmark_group("remote_scan");
    let mut rooted = RemoteScanner::new();
    group.bench_function("pane_rooted", |b| b.iter(|| rooted.refresh_roots(&[root])));
    let mut full = RemoteScanner::new();
    group.bench_function("whole_process_table", |b| b.iter(|| full.refresh()));
    group.finish();
    if let Some(pane) = pane.as_mut() {
        stop_stand_in(pane);
    }
}

criterion_group!(benches, scans);
criterion_main!(benches);
