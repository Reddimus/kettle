//! A session's sound output on this machine's own device, muted so a test
//! run makes no sound. A machine with no output device, or one whose clock
//! never starts (a headless runner's), skips these, saying so, unless
//! `KETTLE_REQUIRE_AUDIO` is set, which makes that a failure.
#![cfg(all(target_os = "macos", feature = "avfoundation"))]

use std::time::{Duration, Instant};

use kettle_media::audio::CHANNELS;
use kettle_media::audio::SAMPLE_RATE;
use kettle_media_native::audio_out::{
    BUFFER_FRAMES, BUFFERS, MAX_LATENCY, Output, Unavailable, latency,
};

/// A muted output, or `None` on a machine with none.
fn output() -> Option<Output> {
    match Output::open(true) {
        Ok(output) => Some(output),
        Err(Unavailable) if std::env::var_os("KETTLE_REQUIRE_AUDIO").is_none() => {
            eprintln!("skipped: no audio output on this machine");
            None
        }
        Err(Unavailable) => panic!("no audio output, and KETTLE_REQUIRE_AUDIO is set"),
    }
}

/// Wait for a started queue's clock, filling it as it plays: a device
/// starts its clock in its own time, a virtual one late. One that has not
/// in two seconds is as good as no output.
fn clock_started(output: &mut Output) -> bool {
    let until = Instant::now() + Duration::from_secs(2);
    while output.queued().is_none() {
        if Instant::now() > until {
            assert!(
                std::env::var_os("KETTLE_REQUIRE_AUDIO").is_none(),
                "the audio output's clock never ran, and KETTLE_REQUIRE_AUDIO is set"
            );
            eprintln!("skipped: the audio output's clock never ran");
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
        fill(output, BUFFER_FRAMES);
    }
    true
}

/// Queue every free buffer with `frames` frames of silence; how many.
fn fill(output: &mut Output, frames: usize) -> usize {
    let silence = vec![0f32; frames * CHANNELS];
    let mut queued = 0;
    while let Some(index) = output.take_free() {
        output.enqueue(index, &silence).unwrap();
        queued += 1;
    }
    queued
}

/// The queue's clock starts with it, runs while it plays, muted or not,
/// holds while it is paused and runs on from there; each buffer comes back
/// free once played.
#[test]
fn the_output_s_clock_runs_while_it_plays() {
    let Some(mut output) = output() else {
        return;
    };
    assert_eq!(fill(&mut output, BUFFER_FRAMES), BUFFERS);
    assert!(output.take_free().is_none());
    assert_eq!(output.played(), None, "before it starts");
    output.start().unwrap();
    if !clock_started(&mut output) {
        return;
    }
    let mut last = 0.0;
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(30));
        fill(&mut output, BUFFER_FRAMES);
        let (played, _) = output.queued().expect("a running clock");
        assert!(played >= last, "{played} after {last}");
        last = played;
    }
    // A third of a second, give or take the device's start.
    assert!(last > 4_800.0, "{last} frames played");
    output.pause().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let (held, _) = output.queued().expect("a paused clock");
    std::thread::sleep(Duration::from_millis(100));
    let (still, _) = output.queued().expect("a paused clock");
    assert!(
        (still - held).abs() < 1.0,
        "{held} then {still} while paused"
    );
    output.start().unwrap();
    for _ in 0..5 {
        std::thread::sleep(Duration::from_millis(30));
        fill(&mut output, BUFFER_FRAMES);
    }
    let (resumed, _) = output.queued().expect("a running clock");
    assert!(resumed > still + 2_400.0, "{resumed} after {still}");
}

/// The output's device says how long after the queue plays a frame it is
/// heard, more than nothing and at most `MAX_LATENCY`; what has been heard
/// is what the queue played less that.
#[test]
fn what_is_heard_is_what_the_queue_played_less_its_latency() {
    let Some(mut output) = output() else {
        return;
    };
    let latency = latency().expect("a device that says its latency");
    assert!(
        latency > Duration::ZERO && latency <= MAX_LATENCY,
        "{latency:?}"
    );
    assert_eq!(output.played(), None, "before it starts");
    fill(&mut output, BUFFER_FRAMES);
    output.start().unwrap();
    if !clock_started(&mut output) {
        return;
    }
    for _ in 0..5 {
        std::thread::sleep(Duration::from_millis(30));
        fill(&mut output, BUFFER_FRAMES);
    }
    // Paused, so its clock holds between the two.
    output.pause().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let (queued, _) = output.queued().expect("a paused clock");
    let (heard, _) = output.played().expect("a paused clock");
    let late = latency.as_secs_f64() * f64::from(SAMPLE_RATE);
    assert!(
        (heard - (queued - late)).abs() < 1.0,
        "{heard} heard of {queued} played, {latency:?} late"
    );
}

/// A buffer is filled only within its size, by whole frames, and only one
/// of the queue's own.
#[test]
fn a_buffer_is_filled_only_within_its_size() {
    let Some(mut output) = output() else {
        return;
    };
    let index = output.take_free().unwrap();
    for samples in [
        vec![0f32; 0],
        vec![0f32; (BUFFER_FRAMES + 1) * CHANNELS],
        vec![0f32; CHANNELS + 1],
    ] {
        assert_eq!(
            output.enqueue(index, &samples),
            Err(Unavailable),
            "{}",
            samples.len()
        );
    }
    assert_eq!(output.enqueue(BUFFERS, &[0.0; CHANNELS]), Err(Unavailable));
    output.enqueue(index, &[0.0; CHANNELS]).unwrap();
}
