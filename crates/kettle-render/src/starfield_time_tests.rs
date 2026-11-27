use super::{background_clock, production_source, starfield_time};
use kettle_config::BackgroundAnimation;
use std::time::Duration;

#[test]
fn animation_off_freezes_the_starfield() {
    let later = Duration::from_secs(90);
    assert_eq!(starfield_time(BackgroundAnimation::Off, later), 0.0);
    assert_eq!(starfield_time(BackgroundAnimation::Always, later), 90.0);
    assert_eq!(
        starfield_time(BackgroundAnimation::WhenFocused, later),
        90.0
    );
}

#[test]
fn the_starfield_uniform_takes_its_time_from_starfield_time() {
    let production = production_source();
    let upload = production
        .split_once("self.starfield.upload(")
        .expect("starfield upload")
        .1
        .split_once(");")
        .expect("end of upload")
        .0;
    assert!(upload.contains("starfield_time(cfg.background_animation, clock)"));
    assert!(
        production.contains(
            "let clock = background_clock(self.background_held, self.starfield_started);"
        ),
        "the uniform's clock is the one reduced motion holds"
    );
}

/// Held by reduced motion, an animated background's clock stops where it
/// was, whatever the time; one that started since the hold shows its first
/// frame.
#[test]
fn a_held_background_clock_stops_where_it_was() {
    let started = std::time::Instant::now();
    let held = started + Duration::from_secs(3);
    assert_eq!(
        background_clock(Some(held), started),
        Duration::from_secs(3)
    );
    assert_eq!(background_clock(Some(started), held), Duration::ZERO);
    assert!(background_clock(None, started) < Duration::from_secs(3));
}
