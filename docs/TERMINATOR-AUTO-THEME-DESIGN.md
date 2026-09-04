# Terminator `auto_theme.py` — auto-detect sunrise/sunset design

> Status: partially shipped. An earlier change shipped the *manual* half of
> `plugins/auto_theme.py`: a `toggle_light_dark` chord plus
> `light-theme` / `dark-theme` config keys. Later work added clock
> and sunrise/sunset scheduling. v2.25.1 wires OS appearance following
> through winit's current window theme and `ThemeChanged` event; no
> separate `dark-light` watcher task is required for the direct
> light/dark theme-pair case.

## What it is

Terminator's auto_theme has three modes:

  - `Light`  — always use the configured `light_theme`
  - `Dark`   — always use the configured `dark_theme`
  - `Auto`   — follow the GTK system theme (libhandy's `StyleManager`
               on Linux; falls back to GTK theme-name string match)

End-state UX in kettle:

- The user sets `theme-mode = auto` in `~/.config/kettle/config`.
- kettle queries winit's current window theme once at launch when the
  platform reports one + listens for live `ThemeChanged` events.
- The current theme tracks the OS preference live: switching
  OS appearance toggle flips kettle to its `dark-theme` immediately,
  no restart, on platforms/compositors that emit the winit event.
- Plus an optional `theme-schedule = sunrise/sunset` (or hour:min
  numeric) that flips the theme on a wall-clock schedule and takes
  precedence over OS appearance changes.

## Why two layers

Two layers, each non-trivial:

### Layer 1 — OS preference detection

Implemented through winit 0.30:

- `Window::theme()` supplies the initial value when supported.
- `WindowEvent::ThemeChanged(Theme)` supplies live changes.
- No polling thread, registry watcher, DBus subscription, or
  additional dependency is needed in kettle's app layer.

Platform caveat: winit reports `None` or no event on some Linux
compositor/window-system combinations. In that case `light-theme` /
`dark-theme`, `toggle_light_dark`, and `theme-schedule` remain the
fallbacks.

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
│ kettle_ui::app::App                                                  │
│                                                                      │
│  1. ThemeMode::Auto with no theme-schedule:                          │
│     - on startup, read Window::theme() when available                 │
│     - on WindowEvent::ThemeChanged(theme), resolve the theme pair     │
│                                                                      │
│  2. Schedule::Clock / SunriseSunset:                                 │
│     - poll the configured schedule from the existing wait/redraw loop │
│     - when present, schedule takes precedence over OS following       │
│                                                                      │
│  Both routes reuse resolve_theme_for_mode + apply_theme_name.         │
└──────────────────────────────────────────────────────────────────────┘
```

No watcher task is needed for OS appearance following; the event is already
delivered on the main winit event loop.

## Phase roadmap

| Phase | What ships | Test coverage |
|-----------|-----------|---------------|
| 1 | `ThemeMode` enum + `theme-mode` config key | Drift guard on parser arms |
| 2 | Initial OS preference via winit `Window::theme()` | Source drift guard + manual e2e where supported |
| 3 | Live OS updates via winit `WindowEvent::ThemeChanged` | Source drift guard + manual e2e on each platform/compositor that emits it |
| 4 | `ThemeSchedule::Clock` (HH:MM dark_at + light_at) + app-loop poll | Pure unit test on the boundary-crossing logic |
| 5 | `ThemeSchedule::SunriseSunset` (lat/long) | Pure unit tests on solar-position calc for known dates |
| 6 | Shared App-side apply helper for schedule + OS following | Drift guard on the event-routing |
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

- **Risk:** winit returns no initial theme or emits no theme-change event on
  some Linux compositor/window-system combinations. **Mitigation:** the
  `toggle_light_dark` manual toggle and `theme-schedule` remain fallbacks. Detection is
  opt-in via `theme-mode = auto`.
- **Risk:** schedule task drifts if the system sleeps. **Mitigation:**
  recompute the next boundary from wall-clock on resume, not from
  a counted-down sleep.
- **Risk:** lat/long config typo → bogus sunrise time. **Mitigation:**
  parse + validate ranges (lat ∈ [-90, 90], long ∈ [-180, 180]) at
  --check-config time; flag bad values the same way `parse_trigger`
  rejects a malformed keybind.
