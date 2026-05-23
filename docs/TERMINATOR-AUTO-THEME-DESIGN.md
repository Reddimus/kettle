# Terminator `auto_theme.py` — auto-detect sunrise/sunset design

> Status: design only (cycle 632). Cycle 616 shipped the *manual*
> half of `plugins/auto_theme.py`: a `toggle_light_dark` chord plus
> `light-theme` / `dark-theme` config keys. The *auto* half — system
> dark-mode detection + sunrise/sunset scheduling — needs platform-
> specific portal queries and is multi-cycle. Same shape as the other
> Bucket D design docs.

## What it is

Terminator's auto_theme has three modes:

  - `Light`  — always use the configured `light_theme`
  - `Dark`   — always use the configured `dark_theme`
  - `Auto`   — follow the GTK system theme (libhandy's `StyleManager`
               on Linux; falls back to GTK theme-name string match)

End-state UX in kettle:

- The user sets `theme-mode = auto` in `~/.config/kettle/config`.
- kettle queries the OS preference once at launch + listens for
  changes (DBus signal on Linux/Wayland; CGEvent on macOS; registry
  notification on Windows).
- The current theme tracks the OS preference live: switching
  GNOME's "dark style" toggle in Settings flips kettle to its
  `dark-theme` immediately, no restart.
- Plus an optional `theme-schedule = sunrise/sunset` (or hour:min
  numeric) that flips the theme on a wall-clock schedule
  independent of the OS preference.

## Why multi-cycle

Two layers, each non-trivial:

### Layer 1 — OS preference detection

**Linux** (Wayland + X11):
  - DBus portal `org.freedesktop.appearance.ColorScheme` (newer)
  - DBus signal `org.freedesktop.portal.Settings.SettingChanged`
    on key `color-scheme` (0=no-pref, 1=dark, 2=light)
  - Fallback: GTK `gtk-theme-name` settings string match (older
    distros without the portal).

**macOS**:
  - `defaults read -g AppleInterfaceStyle` returns "Dark" or
    nothing (light is implicit).
  - `NSDistributedNotificationCenter` `AppleInterfaceThemeChangedNotification`
    fires on the user-pref toggle.

**Windows**:
  - Registry `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\
    Personalize\AppsUseLightTheme` (0=dark, 1=light)
  - `RegNotifyChangeKeyValue` on that key for live changes.

Decision: use the `dark-light` crate (~30 stars, well-maintained,
cross-platform). It handles all three platforms with a single
`dark_light::detect()` call + a `subscribe()` for change events.
Sound choice for v1; lift to direct portal queries only if
dark-light fails on some user's setup.

### Layer 2 — Sunrise/sunset scheduling

For `theme-schedule = sunrise/sunset` to work, kettle needs the
user's lat/long. Three options:

  - **Explicit config**: `theme-schedule-lat = 37.7749` +
    `theme-schedule-long = -122.4194`. Privacy-friendly; no
    network or location-services prompt.
  - **IP geolocation**: query an IP-geolocation service. Privacy-
    questionable; defer to a follow-up.
  - **OS location services**: macOS CoreLocation, Linux GeoClue2.
    Privacy-prompt-y; defer to a follow-up.

Decision: v1 ships explicit-lat/long-only. The math (sunrise/sunset
from lat/long + date) is well-known — use the `sunrise` crate
(~10 stars, lightweight) or roll our own (NREL Solar Position
Algorithm is ~50 lines).

Plus a clock-time schedule shorthand: `theme-schedule = 18:00 dark,
06:00 light` (HH:MM in local time) as a no-geolocation alternative.

## Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│ kettle_config::ThemeMode (new enum)                                  │
│                                                                      │
│  pub enum ThemeMode { Explicit, Light, Dark, Auto }                  │
│                                                                      │
│  pub schedule: Option<ThemeSchedule>                                 │
│                                                                      │
│  pub enum ThemeSchedule {                                            │
│      SunriseSunset { lat: f64, long: f64 },                          │
│      Clock { dark_at: (u8, u8), light_at: (u8, u8) },                │
│  }                                                                   │
└──────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ kettle_ui::theme_watcher (NEW module)                                │
│                                                                      │
│  1. ThemeMode::Auto:                                                 │
│     spawn task: dark_light::subscribe(|new_mode| { send_event })     │
│     initial value: dark_light::detect()                              │
│                                                                      │
│  2. Schedule::Clock:                                                 │
│     spawn task: every 60s, check now vs (dark_at, light_at)          │
│     fire ThemeModeEvent::AutoUpdated if crossing a boundary          │
│                                                                      │
│  3. Schedule::SunriseSunset:                                         │
│     compute today's sunrise + sunset from lat/long at midnight       │
│     spawn task: at next boundary, fire event + recompute             │
│                                                                      │
│  Events feed into the App's existing reload-theme path               │
│  (cycle 616 dispatch + Action::ToggleLightDark machinery).           │
└──────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│ kettle_ui::app::App                                                  │
│                                                                      │
│  on ThemeModeEvent::AutoUpdated(is_dark):                            │
│      target = if is_dark { &cfg.dark_theme } else { &cfg.light_theme }│
│      apply_theme(target)  ← same path as ToggleLightDark             │
└──────────────────────────────────────────────────────────────────────┘
```

The watcher task runs off the main render thread; events come back
via the existing winit event-loop user-event queue.

## Sub-cycle roadmap

| Sub-cycle | What ships | Test coverage |
|-----------|-----------|---------------|
| 1 | `ThemeMode` enum + `theme-mode` config key | Drift guard on parser arms |
| 2 | `dark-light` crate dep + initial-detect at launch | Manual e2e on Linux+GNOME |
| 3 | DBus / NSDistributedNotificationCenter / Registry subscribe via dark-light::subscribe | Manual e2e on each platform |
| 4 | `ThemeSchedule::Clock` (HH:MM dark_at + light_at) + 60s tick task | Pure unit test on the boundary-crossing logic |
| 5 | `ThemeSchedule::SunriseSunset` (lat/long) + sunrise crate | Pure unit tests on solar-position calc for known dates |
| 6 | App-side event handler — reuses cycle-616 apply_theme | Drift guard on the event-routing |
| 7 | Audit doc + CONFIG.md + CHANGELOG | doc-only |

Estimated test growth: +10-12 (the pure boundary-crossing + solar
calc paths are nicely unit-testable; the platform subscribe paths
need manual e2e).

## What WON'T ship in v1

- **IP geolocation**. Privacy posture: kettle never makes network
  requests for theme purposes. Users supply lat/long explicitly.
- **OS location services**. macOS CoreLocation / Linux GeoClue2
  prompts are user-hostile for a terminal app. Defer indefinitely.
- **Per-pane theme**. kettle's theme is window-wide. Per-pane theme
  would compound with this design; out of scope.

## Acceptance test

```
# In ~/.config/kettle/config:
light-theme = TokyoNight Day
dark-theme = TokyoNight Night
theme-mode = auto

# On GNOME:
$ kettle &
# Settings → Appearance → toggle Light/Dark
# verify: kettle's theme flips live, no restart

# Alternatively:
# In config:
theme-schedule = 18:00 dark, 06:00 light
# verify: at 18:00 local, theme switches to TokyoNight Night
#         at 06:00 local, theme switches to TokyoNight Day

# Or sunrise/sunset:
theme-schedule = sunrise/sunset
theme-schedule-lat = 37.7749
theme-schedule-long = -122.4194
# verify: at today's sunrise time in SF, theme switches to light
#         at today's sunset time, theme switches to dark
```

## Risks + mitigations

- **Risk:** dark-light crate doesn't compile on some target (e.g.
  WSL2, Wayland-only distros without portal). **Mitigation:** the
  cycle-616 manual toggle remains the fallback. Detection is
  opt-in via `theme-mode = auto`.
- **Risk:** DBus / NSDistributedNotificationCenter subscribe blocks
  on first launch. **Mitigation:** subscribe in a spawned task;
  the initial detect runs synchronously at launch but with a
  100 ms timeout (fall back to Light on timeout).
- **Risk:** schedule task drifts if the system sleeps. **Mitigation:**
  recompute the next boundary from wall-clock on resume, not from
  a counted-down sleep.
- **Risk:** lat/long config typo → bogus sunrise time. **Mitigation:**
  parse + validate ranges (lat ∈ [-90, 90], long ∈ [-180, 180]) at
  --check-config time; flag bad values like the cycle-309 trigger
  pattern validation.
