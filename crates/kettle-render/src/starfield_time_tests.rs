use super::{production_source, starfield_time};
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
    assert!(upload.contains("starfield_time(cfg.background_animation"));
}
