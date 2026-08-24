//! v2.20.0 P3: extractor front-stage throughput benches — the first criterion
//! benches in the repo. The extractor sits between the 64KiB PTY reads and
//! the alacritty VT parser, so its per-byte cost multiplies into every
//! throughput number kettle posts. Three workloads mirror the cross-terminal
//! harness payloads from the archived
//! [`v3.3.0` generator](https://github.com/Reddimus/kettle/blob/v3.3.0/scripts/perf/gen-payloads.ps1):
//!
//! - `plain_flood`: pure ASCII text + newlines — the bulk-copy fast path.
//! - `sgr_heavy`: short colored runs (`ESC [3Xm word`) — many tiny
//!   pass-through escapes, the worst case for scan restarts.
//! - `osc_spam`: title + cwd + prompt-mark OSC sequences between text —
//!   exercises the sequence accumulator and finish path.
//! - `non_ascii_flood`: emoji + CJK prose — no escapes at all, but every byte
//!   of it is one the C1 control values live among: `0x90`/`0x9d`/`0x9f` are
//!   ordinary UTF-8 continuation bytes, and 🐝 (`F0 9F 90 9D`) carries three
//!   of them inside one character. An ASCII-only corpus cannot see a scan
//!   that treats those values as interesting and pays per non-ASCII
//!   character to decide they are not — which is how a pass-through scan
//!   turns quadratic on exactly the text most of the world types.
//!
//! Run: `cargo bench -p kettle-vt`. Compare medians across commits; CI does
//! not gate on these (Surface-class hardware variance), they are a local
//! before/after instrument.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use kettle_vt::Extractor;

fn payload_plain() -> Vec<u8> {
    let mut v = Vec::with_capacity(1 << 20);
    let line = b"The quick brown fox jumps over the lazy dog 0123456789\r\n";
    while v.len() < (1 << 20) {
        v.extend_from_slice(line);
    }
    v
}

fn payload_sgr() -> Vec<u8> {
    let mut v = Vec::with_capacity(1 << 20);
    let mut color = 0u8;
    while v.len() < (1 << 20) {
        v.extend_from_slice(format!("\x1b[3{}mword\x1b[0m ", color % 8).as_bytes());
        color = color.wrapping_add(1);
        if color.is_multiple_of(16) {
            v.extend_from_slice(b"\r\n");
        }
    }
    v
}

fn payload_osc() -> Vec<u8> {
    let mut v = Vec::with_capacity(1 << 20);
    let mut n = 0u32;
    while v.len() < (1 << 20) {
        v.extend_from_slice(format!("\x1b]2;title {n}\x07").as_bytes());
        v.extend_from_slice(b"\x1b]7;file://host/tmp/dir\x1b\\");
        v.extend_from_slice(b"\x1b]133;A\x07$ some prompt text\r\n");
        n = n.wrapping_add(1);
    }
    v
}

fn payload_non_ascii() -> Vec<u8> {
    let mut v = Vec::with_capacity(1 << 20);
    while v.len() < (1 << 20) {
        v.extend_from_slice("吾輩は猫である。名前はまだ無い。".as_bytes());
        v.extend_from_slice("\u{1F41D}\u{1F41D}\u{1F41D} привет ✳\r\n".as_bytes());
    }
    v
}

fn bench_extract(c: &mut Criterion) {
    let cases = [
        ("plain_flood", payload_plain()),
        ("sgr_heavy", payload_sgr()),
        ("osc_spam", payload_osc()),
        ("non_ascii_flood", payload_non_ascii()),
    ];
    let mut g = c.benchmark_group("extractor_feed");
    for (name, payload) in &cases {
        g.throughput(Throughput::Bytes(payload.len() as u64));
        g.bench_function(*name, |b| {
            b.iter(|| {
                let mut ex = Extractor::new();
                // Feed in PTY-read-sized chunks so chunk-boundary state
                // handling is part of the measured path.
                let mut total = 0usize;
                for chunk in payload.chunks(64 * 1024) {
                    total += ex.feed(chunk).len();
                }
                total
            })
        });
    }
    g.finish();
}

criterion_group!(benches, bench_extract);
criterion_main!(benches);
