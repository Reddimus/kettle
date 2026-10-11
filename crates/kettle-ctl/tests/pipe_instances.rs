//! A Windows control endpoint never goes missing while its listener readies
//! an instance a client left.
//!
//! A client that opens the pending named-pipe instance and closes it before
//! `accept` reaches it leaves that instance unable to take another client
//! until it is disconnected. If the name had no instance meanwhile, a client
//! opening it then would read NotFound, which `connect` takes for a dead
//! server and gives up on at once: a discovery would pass over a live Kettle,
//! and a client would fail right after another succeeded. Its own binary, so
//! the clients' busy loop shares no process with the timing-bound unit
//! tests.

#![cfg(windows)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use kettle_ctl::transport::{CtlListener, connect};

/// The sequence a_connection_must_reach_the_process_its_entry_names hit on
/// CI: a client comes and goes before accept() runs, then the next client
/// connects while accept() handles the first one's leaving.
#[test]
fn a_client_that_left_before_accept_costs_the_next_nothing() {
    let endpoint = format!(r"\\.\pipe\kettle-ctl-left-{}", std::process::id());
    let listener = CtlListener::bind(&endpoint).expect("bind");
    drop(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&endpoint)
            .expect("the first client opens the pending instance"),
    );
    let server = std::thread::spawn(move || listener.accept().map(drop));
    let next = connect(&endpoint).expect("the next client connects");
    server
        .join()
        .expect("server")
        .expect("accept() serves the next client");
    drop(next);
}

#[test]
fn the_pipe_name_never_goes_missing_while_an_instance_is_readied() {
    let endpoint = format!(r"\\.\pipe\kettle-ctl-ready-{}", std::process::id());
    let listener = CtlListener::bind(&endpoint).expect("bind");
    let stop = Arc::new(AtomicBool::new(false));
    let server = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                // Each accepted connection is dropped at once.
                let _ = listener.accept();
            }
        })
    };
    // Clients that open the name and leave at once, as fast as they can:
    // many leave before accept() reaches their instance, so instances are
    // readied again and again while the others keep opening the name.
    let opened = Arc::new(AtomicU64::new(0));
    let missing = Arc::new(AtomicU64::new(0));
    let deadline = Instant::now() + Duration::from_secs(2);
    let clients: Vec<_> = (0..3)
        .map(|_| {
            let (endpoint, opened, missing) =
                (endpoint.clone(), Arc::clone(&opened), Arc::clone(&missing));
            std::thread::spawn(move || {
                while Instant::now() < deadline {
                    match std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&endpoint)
                    {
                        Ok(_) => {
                            opened.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            missing.fetch_add(1, Ordering::Relaxed);
                        }
                        // Busy: every instance is taken for the moment.
                        Err(_) => {}
                    }
                }
            })
        })
        .collect();
    for client in clients {
        client.join().expect("client");
    }
    stop.store(true, Ordering::Relaxed);
    // Wake accept() to see `stop`, unless the loop has seen it already; the
    // wakes are held until it ends.
    let mut wakes = Vec::new();
    let give_up = Instant::now() + Duration::from_secs(30);
    while !server.is_finished() {
        assert!(Instant::now() < give_up, "the accept loop never ended");
        if let Ok(wake) = connect(&endpoint) {
            wakes.push(wake);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    server.join().expect("server");
    drop(wakes);
    assert!(
        opened.load(Ordering::Relaxed) > 0,
        "no client ever opened the pipe"
    );
    assert_eq!(
        missing.load(Ordering::Relaxed),
        0,
        "the pipe name went missing while an instance was readied"
    );
}
