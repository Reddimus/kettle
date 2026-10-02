//! Local wall-clock time for the status-bar clock and the theme schedule.
//!
//! `std::time` has no time-zone support, so the offset comes from the C
//! library: `localtime_r(3)` fills `tm_gmtoff`, the local offset east of UTC
//! in seconds, from `TZ` or the system zone file. Windows builds fall back to
//! UTC.

use std::time::{SystemTime, UNIX_EPOCH};

/// The local `(hour, minute, second)` right now.
pub(crate) fn now_local() -> (u8, u8, u8) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    time_of_day(secs, local_utc_offset(secs))
}

/// The wall-clock `(hour, minute, second)` of `unix_secs` in a zone
/// `utc_offset_secs` east of UTC.
pub(crate) fn time_of_day(unix_secs: i64, utc_offset_secs: i64) -> (u8, u8, u8) {
    let day = unix_secs.saturating_add(utc_offset_secs).rem_euclid(86_400);
    (
        (day / 3600) as u8,
        (day % 3600 / 60) as u8,
        (day % 60) as u8,
    )
}

/// The local zone's offset east of UTC at `unix_secs`, or 0 if the C library
/// cannot convert it.
#[cfg(unix)]
fn local_utc_offset(unix_secs: i64) -> i64 {
    // `time_t` is 64-bit on every Unix target Kettle builds for.
    let t = unix_secs as libc::time_t;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::zeroed();
    // SAFETY: `t` is a valid `time_t`, and `tm` is writable storage for one
    // `libc::tm`. `localtime_r` is the reentrant form: it writes only through
    // the pointer we pass and returns null on failure, in which case `tm` is
    // not read.
    let filled = unsafe { libc::localtime_r(&t, tm.as_mut_ptr()) };
    if filled.is_null() {
        return 0;
    }
    // SAFETY: `localtime_r` returned non-null, so it filled `tm`.
    let tm = unsafe { tm.assume_init() };
    // `tm_gmtoff` is a C `long`, 32-bit on some Unix targets.
    #[allow(clippy::useless_conversion)]
    let offset = i64::from(tm.tm_gmtoff);
    offset
}

#[cfg(not(unix))]
fn local_utc_offset(_unix_secs: i64) -> i64 {
    0
}

#[cfg(test)]
mod tests {
    use super::time_of_day;

    // 2026-09-27 01:00:00 UTC.
    const ONE_AM_UTC: i64 = 1_790_470_800;

    #[test]
    fn west_of_utc_is_the_previous_evening() {
        assert_eq!(time_of_day(ONE_AM_UTC, -7 * 3600), (18, 0, 0));
    }

    #[test]
    fn east_of_utc_and_half_hour_zones_shift_forward() {
        assert_eq!(time_of_day(ONE_AM_UTC, 5 * 3600 + 30 * 60), (6, 30, 0));
        assert_eq!(time_of_day(ONE_AM_UTC, 0), (1, 0, 0));
    }

    #[test]
    fn the_epoch_and_negative_days_stay_in_range() {
        assert_eq!(time_of_day(0, -3600), (23, 0, 0));
        assert_eq!(time_of_day(59, 0), (0, 0, 59));
    }

    #[test]
    fn a_clock_schedule_follows_local_time_not_utc() {
        use kettle_config::{ThemeSchedule, schedule_decision_clock};
        // `19:00 dark, 07:00 light`, checked at 20:00 UTC from UTC-7, where it
        // is 13:00: light. Read as UTC, the same instant would be dark.
        let schedule = ThemeSchedule::Clock {
            dark_at: (19, 0),
            light_at: (7, 0),
        };
        let eight_pm_utc = ONE_AM_UTC + 19 * 3600;
        let (h, m, _) = time_of_day(eight_pm_utc, -7 * 3600);
        assert!(!schedule_decision_clock((h, m), schedule));
        let (h, m, _) = time_of_day(eight_pm_utc, 0);
        assert!(schedule_decision_clock((h, m), schedule));
    }

    #[test]
    fn the_schedule_and_status_clock_read_local_time() {
        let app = kettle_test_support::production_source(include_str!("app.rs"));
        let schedule = app
            .split_once("fn theme_schedule_is_dark(")
            .expect("theme_schedule_is_dark")
            .1
            .split_once("ThemeSchedule::SunriseSunset")
            .expect("sunrise arm")
            .0;
        assert!(schedule.contains("wall_clock::now_local()"));
        let status = app
            .split_once("fn build_status_bar(")
            .expect("build_status_bar")
            .1
            .split_once("\n    }\n")
            .expect("end of build_status_bar")
            .0;
        assert!(status.contains("wall_clock::now_local()"));
        assert!(!status.contains(" UTC "), "the clock shows local time");
    }

    #[cfg(unix)]
    #[test]
    fn the_local_offset_is_a_real_zone_offset() {
        let offset = super::local_utc_offset(ONE_AM_UTC);
        // Real zones run from UTC-12 to UTC+14.
        assert!((-12 * 3600..=14 * 3600).contains(&offset), "{offset}");
    }
}
