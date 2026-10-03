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
//! This build renders nothing yet: every job is answered `WorkerUnavailable`.

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
    use kettle_test_support::production_source;

    /// The order `main` must keep, and what the worker must never do: print
    /// outside the framed writer, or read arguments, the environment or files.
    #[test]
    fn worker_early_setup_precedes_all_reads() {
        let main = production_source(include_str!("main.rs"));
        let body = main
            .split("fn main() {")
            .nth(1)
            .expect("the Unix main is present");
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
            .map(|call| body.find(call).unwrap_or_else(|| panic!("{call} missing")))
            .collect();
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "main must run {order:?} in that order"
        );
        // The sweep is the first statement.
        assert!(
            body.trim_start()
                .starts_with("if early_unix::sweep_fds_and_disable_dumping()")
        );

        let worker = production_source(include_str!("worker.rs"));
        let early = production_source(include_str!("early_unix.rs"));
        for (name, source) in [("main", &main), ("worker", &worker), ("early_unix", &early)] {
            for forbidden in [
                "println!",
                "print!",
                "eprintln!",
                "eprint!",
                "dbg!",
                "std::env",
                "env::args",
                "env::var",
                "std::fs",
                "File::",
                "OpenOptions",
            ] {
                assert!(!source.contains(forbidden), "{name} uses {forbidden}");
            }
        }
        // One writer owns stdout; stderr carries only the panic line.
        assert_eq!(main.matches("io::stdout()").count(), 1);
        assert_eq!(
            worker.matches("io::stdout()").count() + early.matches("io::stdout()").count(),
            0
        );
        assert_eq!(
            main.matches("io::stderr()").count() + early.matches("io::stderr()").count(),
            0
        );
        assert_eq!(worker.matches("io::stderr()").count(), 1);
        let panic_report = worker
            .split("fn report_panic() {")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("report_panic is present");
        assert!(panic_report.contains("io::stderr()"));
        assert!(panic_report.contains("b\"media worker panic\\n\""));
    }
}
