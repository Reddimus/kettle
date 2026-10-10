//! Kettle's media worker. The GUI starts one per job from beside its own
//! executable and talks to it in `kettle-media`'s frames over stdin and
//! stdout: Hello in, Ready out, one Job in, one Rendered or Failure out, exit.
//!
//! Nothing runs before the early setup at the top of `main`: on Linux and
//! macOS it closes every descriptor inherited above stderr and turns off core
//! dumps (on Linux the process also becomes non-dumpable), before any
//! argument, environment variable, input or library is touched. Then come a
//! panic hook that reports one fixed line, the resource limits, and a watchdog
//! that ends the process when the parent stalls or a phase overruns. stdout
//! carries frames only, through one writer.
//!
//! `kettle-media-render` renders raster and SVG jobs; other kinds are refused
//! as `UnsupportedMedia`.

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod early_unix;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod worker;

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn main() {
    if early_unix::sweep_fds_and_disable_dumping().is_err() {
        std::process::exit(worker::EXIT_SETUP);
    }
    std::panic::set_hook(Box::new(|_| worker::report_panic()));
    if early_unix::install_limits().is_err() {
        std::process::exit(worker::EXIT_SETUP);
    }
    let Ok(watchdog) = worker::Watchdog::start(worker::READY_DEADLINE) else {
        std::process::exit(worker::EXIT_SETUP);
    };
    let mut output = std::io::stdout().lock();
    std::process::exit(worker::serve(
        &mut std::io::stdin().lock(),
        &mut output,
        &watchdog,
    ));
}

/// There is no media worker on this platform, and Kettle never starts one
/// here. Exits with the setup failure code.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn main() {
    std::process::exit(8);
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use kettle_test_support::{code_only, production_source};

    /// The body of the first `fn {name}` in `code`, from its opening brace to
    /// the matching close.
    fn body<'a>(code: &'a str, name: &str) -> &'a str {
        let start = code
            .find(&format!("fn {name}"))
            .unwrap_or_else(|| panic!("fn {name} is missing"));
        let open = start + code[start..].find('{').expect("a body");
        let mut depth = 0;
        for (offset, byte) in code[open..].bytes().enumerate() {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &code[open + 1..open + offset];
                    }
                }
                _ => {}
            }
        }
        panic!("fn {name} is not closed")
    }

    /// The order `main` must keep, and what the worker must never do: print
    /// outside the framed writer, or read arguments, the environment or
    /// files. Matched against code alone: comments and strings are blanked,
    /// so mentioning a call cannot stand in for making it.
    #[test]
    fn worker_early_setup_precedes_all_reads() {
        let main = code_only(&production_source(include_str!("main.rs")));
        // The `test-faults` hooks, last in the file and built only for the
        // fault tests, are not production code.
        let worker = code_only(&production_source(
            include_str!("worker.rs")
                .split("#[cfg(feature = \"test-faults\")]\nmod faults")
                .next()
                .expect("the worker's code"),
        ));
        let early = code_only(&production_source(include_str!("early_unix.rs")));

        let unix_main = body(&main, "main()");
        let order = [
            "early_unix::sweep_fds_and_disable_dumping()",
            "std::panic::set_hook(",
            "early_unix::install_limits()",
            "worker::Watchdog::start(",
            "std::io::stdout().lock()",
            "worker::serve(",
        ];
        let positions: Vec<usize> = order
            .iter()
            .map(|call| {
                assert_eq!(unix_main.matches(call).count(), 1, "{call} once");
                unix_main.find(call).unwrap()
            })
            .collect();
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "main must run {order:?} in that order"
        );
        // The sweep is the first statement, and closing descriptors is the
        // sweep's own first act.
        assert!(
            unix_main
                .trim_start()
                .starts_with("if early_unix::sweep_fds_and_disable_dumping()")
        );
        assert!(
            body(&early, "sweep_fds_and_disable_dumping")
                .trim_start()
                .starts_with("close_above_stderr()?;")
        );

        // The Linux fallback sweep may list /proc/self/fd; nothing else
        // touches the filesystem.
        let early_without_listing = early.replacen("std::fs::read_dir(", "", 1);
        for (name, code) in [
            ("main", main.as_str()),
            ("worker", worker.as_str()),
            ("early_unix", early_without_listing.as_str()),
        ] {
            for forbidden in [
                "println!",
                "print!",
                "eprintln!",
                "eprint!",
                "dbg!",
                "std::env",
                "env::",
                "args()",
                "std::fs",
                "fs::",
                "File::",
                "OpenOptions",
            ] {
                assert!(!code.contains(forbidden), "{name} uses {forbidden}");
            }
        }
        // The watchdog, started before any job's sandbox, confines itself
        // first; a job confines the worker before rendering.
        assert!(
            body(&worker, "start(")
                .split("std::thread::Builder::new()")
                .nth(1)
                .is_some_and(
                    |thread| thread
                        .split(".spawn(move || {")
                        .nth(1)
                        .is_some_and(|body| body.trim_start().starts_with(
                            "let confined = kettle_media_native::sandbox::confine_thread_to_nothing().is_ok();"
                        ))
                )
        );
        let answer = body(&worker, "answer(");
        assert!(
            answer.find("confine(job, decoders.as_ref())").unwrap()
                < answer
                    .find("kettle_media_render::render_with_decoder(")
                    .unwrap()
        );
        // One writer owns stdout; stderr carries only the panic line.
        assert_eq!(main.matches("stdout()").count(), 1);
        assert_eq!(
            worker.matches("stdout()").count() + early.matches("stdout()").count(),
            0
        );
        assert_eq!(main.matches("stderr()").count(), 0);
        assert_eq!(worker.matches("stderr()").count(), 1);
        assert!(!early.contains("io::stderr()"));
        assert!(body(&worker, "report_panic").contains("std::io::stderr().write_all("));
        let source = production_source(include_str!("worker.rs"));
        assert!(body(&source, "report_panic").contains(r#"write_all(b"media worker panic\n")"#));
    }
}
