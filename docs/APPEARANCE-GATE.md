# macOS appearance gate

[`RELEASING.md`](RELEASING.md#macos-appearance-gate) requires a native run
before the release-cut pull request merges. Unit and image tests can prove the
material policy; they cannot prove what AppKit actually draws. This file records
each run, including what did not run and why.

## 4.1.0 cut — 2026-08-28

Host: macOS 26.6.2, Apple silicon, system appearance **Dark**. Bundle: a
universal ad-hoc-signed `kettle.app` built from the clean cut commit
`122ec152`, replicating `release.yml`'s `Build (macOS universal)` and
`Package (macOS .app bundle)` steps exactly — both `--target` release builds,
`lipo`, `scripts/compile-macos-app-icon.sh` through Xcode 26.6, and the
PlistBuddy version patch. `lipo -archs` reports `x86_64 arm64` and
`CFBundleShortVersionString` is `4.1.0`. The plist was patched through a copy,
so the cut worktree stayed clean at that SHA; verified after the run.

This release changes keyboard handling only — no rendering, material, window
chrome or icon code is touched — so the gate is run as policy rather than
because the diff puts any of it at risk.

Window frames were captured with `screencapture -l<windowid>`, which reads only
kettle's own window layer. No full-screen capture was taken, so nothing outside
kettle was recorded at any point.

### Passed

| Check | Result |
|---|---|
| Default 86% opacity, native blur on | Zero clear pixels inside the opaque span on every sampled row. The titlebar material is a uniform `(29,32,35,255)` from four pixels inside the left end of the span to four pixels inside the right end, at both y=8 and y=16. The narrowing span (`10..805` at y=0 widening to `0..815` by y=16) is the corner radius itself, not a gap. |
| Alpha on, blur off | Byte-identical measurements to the row above: uniform `(29,32,35,255)` to both corners, zero clear pixels. AppKit supplied its standard titlebar backdrop rather than exposing a clear desktop strip. |
| Opaque surface, blur left on | Same uniform material to both corners, zero clear pixels, and no titlebar-only seam: the only edge is the titlebar/grid boundary where the terminal content begins. |
| Opaque surface, blur off | Identical. The active theme reaches both rounded top corners with no clear or mismatched strip. |
| `borderless = true` | No titlebar; terminal content starts at y=0 and the first uniform content row reads `(29,33,44,219)`. Alpha 219 is the configured 86%, so the terminal remains visible through the documented sharp-alpha fallback instead of being covered by a material view. |
| Full-screen round trip | `ui_geometry`'s `content` rect is byte-identical before and after: `{"height": 463.75, "width": 816.0, "x": 0.0, "y": 24.25}` → fullscreen `{"height": 1055.75, "width": 1920.0, ...}` → back to the original values exactly. |
| Light theme at **startup** under a Dark system ([#251](https://github.com/Reddimus/kettle/issues/251)) | Measured through the accessibility API rather than by eye. Traffic lights occupy x=560..622 (close 560, minimize 583, full-screen 606, each 16 wide); the title element starts at **x=634**, width 69. The title therefore begins 12 px to the right of the last button and sits beside the cluster, not across it. This is the startup path, not the runtime toggle. |
| Icon geometry | `AppIcon.icns` 256 px rendering: rim L21 R21 T23 B19, horizontal centre offset 0.0 px — the inset face is parallel to the system mask with clear rim space on every side. `Assets.car` carries the full ladder (32, 64, 128, 256, 512, 1024 across 11 renditions), so Finder, the Dock item and the app switcher all draw from one compiled asset and cannot disagree. |

### Not run

**Live blur compositing.** `screencapture -l<windowid>` reads kettle's own layer
and does not composite what is behind the window, so the blur-on and blur-off
scenarios produce identical bytes by construction. What the measurements above
establish is that no clear strip or seam exists and that the material reaches
both corners in every configuration — not that the blur is visibly compositing.
The 4.0.1 run has the working method for that question (a second kettle
instance as an opaque backdrop, judged from a composited screenshot); it was not
repeated here because no rendering code changed in this release.

**Toggling Reduce Transparency live.** Re-tested rather than assumed:
`defaults write com.apple.universalaccess reduceTransparency -bool true` returns
`Could not write domain com.apple.universalaccess; exiting`. TCC still refuses,
and the System Settings switch needs foreground pointer input. The portable
policy tests still cover the Reduce Transparency state; what stays unproven is
that the material disappears and returns *live*.

**Dock magnification.** Magnification renders only under the pointer, and this
session has keyboard event injection but no pointer control. The 256 px asset
the magnified Dock draws was measured directly above.

**The app switcher.** Cmd-Tab must be held for its window to stay up. The icon
it shows comes from the same `Assets.car` measured above, which contains a
single AppIcon set for every size.

## 4.0.1 cut — 2026-08-25

Host: macOS 26.6.2 (Darwin 25G83), Apple silicon. Bundle: a universal
ad-hoc-signed `kettle.app` built from the exact clean cut
`2234a26587da5fa7d4b6e43af0a88c16a32bcf68`, replicating `release.yml`'s
`Build (macOS universal)` and `Package (macOS .app bundle)` steps: both
`--target` release builds, `lipo`, `scripts/compile-macos-app-icon.sh`, and the
PlistBuddy version patch. `lipo -archs` reports `x86_64 arm64` and the bundle's
`CFBundleShortVersionString` is `4.0.1`. The plist was patched through a copy so
the cut worktree stayed clean at that SHA, verified after the run.

Window frames were captured with `screencapture -l<windowid>`, which reads only
kettle's own window layer. An earlier full-screen capture was taken, found to
contain unrelated application content, and deleted unread; nothing outside
kettle was analysed or kept.

### Passed

| Check | Result |
|---|---|
| Default 86% opacity and native blur | Across every top row, the material is `(31,35,42,255)` from four pixels inside the left end of the opaque span to four pixels inside the right end, with zero clear pixels inside the span. The narrowing spans (`26..789` at y=0 through `0..815` at y=32) are the corner radius itself, not a gap. |
| Alpha on, blur off | Same measurements: uniform `(31,35,42,255)` to both corners, zero clear pixels. AppKit supplied a backdrop rather than exposing a clear strip. |
| Opaque surface, blur left on | Identical, with the single titlebar/grid edge at y=62. No titlebar-only material seam. |
| `background-opacity = 1.0`, `window-blur = false` | The theme reached both rounded top corners with no clear or mismatched strip, confirmed both in the frame samples and composited over a controlled backdrop. |
| Material over a saturated backdrop | The window's own layer reads identically whatever sits behind it, so translucency was confirmed compositionally instead: over an opaque `#f0b000` surface the dark material still reached both rounded corners, with the blur's warmth visible in the titlebar against the neutral grid. |
| Resize and full-screen round trip | Resized to 1100×700, `content` became `1100×659.5` at `y = 40.5`; full screen gave `3456×2127.5`; leaving full screen returned to exactly `1100×659.5` at `y = 40.5`. |
| `borderless = true` full-screen round trip | `816×479.5` → `3456×2127.5` → exactly `816×479.5`, and the window returned to its 408×260 frame. In full screen the terminal stayed visible through the sharp-alpha fallback with an on-screen sentinel legible and no material view over it. |
| Runtime light/dark switching | Five `next_theme` switches on one open window moved the NSWindow titlebar `(225,225,225)` → `(31,35,42)` across four dark themes → `(255,255,255)`, so the window background tracked the palette across the light/dark boundary rather than only the grid. |
| Start on a light theme with macOS Dark | With the system in Dark, Alabaster **started** white — titlebar and grid both `(255,255,255)` — and the complete title sat beside clean traffic lights, not across them. This is the [#251](https://github.com/Reddimus/kettle/issues/251) shape, checked at startup rather than through the toggle. |
| `AppIcon.icns` at 256 px | The outer corner is clipped by the system mask, the inset face keeps clear rim space (left-edge alpha stays 0 through x=16 before ramping), and the `>_` mark is centred and legible. |
| Both icon resource paths present | `Assets.car` (500,744 bytes) and the loose `AppIcon.icns` fallback are both in `Contents/Resources`, so Finder, the Dock item and the app switcher cannot disagree for lack of artwork. |

### Not run

**Live Reduce Transparency toggle.** Toggling it means changing a system
accessibility setting, which this session does not do unattended. The setting
was confirmed off (`reduceTransparency = 0`) and left untouched. Not covered by
any other check here: run it by hand before trusting the material under Reduce
Transparency.

**Dock magnification, and the running / closed-but-pinned Dock items.**
Magnification is likewise a system setting. The Dock also never reached a
capture: screenshots in this session are composited at the allowlist level, and
the Dock is not one of the granted applications, so it is filtered out. The
icon evidence above is the bundle's own resources, not what the Dock drew.

**App-switcher icon.** The global Command-Tab switcher cannot be held open for
an application-scoped capture, unchanged from the 4.0.0 run.

**`Assets.car` rendered appearance.** Only its presence and size were checked
this cycle; the 4.0.0 run rendered both 256 px paths and compared them.

No system setting was changed during this run. The appearance stayed Dark,
Reduce Transparency stayed off, and the cut worktree was clean at
`2234a265` afterwards.

## 4.0.0 pre-release — 2026-08-24

Host: macOS 26.6.2 (Darwin 25G83), Apple silicon. Bundle: a universal
ad-hoc-signed `kettle.app` built from exact clean cut
`67282a152b5b28402f842db8875d451101753459` using Rust 1.97.1 and the
release workflow's Xcode 26.6 icon, dual-target build, `lipo`, plist and
resource steps. `lipo -archs` reports `x86_64 arm64`.

### Passed

| Check | Result |
|---|---|
| Default 86% opacity and native blur | One material reached both rounded top corners with no clear strip or seam. Traffic lights and title remained in their native positions. |
| Resize and full-screen round trip | After resizing to 1100×700, `ui_geometry.content` returned exactly to `1100×675.75` at `y = 24.25`. |
| `borderless = true` full-screen round trip | The terminal remained visible through the sharp-alpha fallback and restored the same exact geometry. |
| Alpha on, blur off | AppKit supplied its opaque titlebar backdrop; no clear desktop strip appeared. |
| Opaque surface, blur on | No titlebar-only material seam appeared. |
| `background-opacity = 1.0`, `window-blur = false` | TokyoNight reached both rounded top corners without a clear or mismatched strip. |
| Runtime light/dark switching | Four deterministic switches alternated central mean RGB `(35,37,49)` and `(255,255,255)` repeatably while caption and corners stayed intact. |
| Start on a light theme with macOS Dark | Alabaster started white with the complete title beside clean traffic lights. |
| macOS Light with dark Kettle | TokyoNight retained a clean dark caption, surface and rounded corners. |
| Toggle Reduce Transparency live | The same open window changed from mean RGB `(35,37,49)` to opaque `(29,31,43)`, then returned exactly to `(35,37,49)` without relaunch. |
| Finder icon | The exact cut bundle retained the system mask, parallel inset face, clear rim and centered legible terminal mark. |
| Both 256 px resource paths | `Assets.car` rendered a blue rim over a dark face and `AppIcon.icns` rendered the inverse; both retained matching geometry and a centered mark. |

### Partial or not run

**Running and pinned Dock icons.** A native capture showed the exact-cut running
item and the existing pinned `/Applications/kettle.app` item agreeing visually.
The existing pinned application was already running and was preserved, so an
exact-cut closed-but-pinned state was not captured.

**Dock magnification.** Dock accessibility inspection timed out and the
automation interface has no pointer-hover primitive. Magnification remained
off and untouched.

**App-switcher icon.** App-scoped automation cannot keep the global Command-Tab
switcher open for capture.

System appearance was restored to Dark, Reduce Transparency to off, Dock
auto-hide to on, and Dock magnification remained off. The cut worktree was
still clean at the exact SHA after the run.

## 3.3.0 (2026-08-24)

### Not run

**Native macOS appearance gate.** No native appearance run was performed for
3.3.0 before the release was published. This was a procedural miss against the
release checklist, not a passing result. The release changed only the updater's
end-of-life notification for retired Windows clients and release metadata and
documentation. It did not change native-window, titlebar/material, renderer, or
icon code, so the release received a narrow, one-release waiver from repeating
the native appearance run. This entry records the omission; it does not
retroactively satisfy the gate or extend the waiver to later releases.

## 3.2.1 pre-release completion — 2026-08-23

Host: macOS 26 (Darwin 25.6), Apple silicon. Bundle: the installed, signed
`/Applications/kettle.app` at 3.2.1. The run used foreground Computer Use
against System Settings and restored the user's original dark appearance,
Reduce Transparency off, and Dock magnification off when it finished.

### Passed

| Check | Result |
|---|---|
| System appearance light with a dark Kettle theme | With macOS Appearance set to **Light** and Kettle still on **TokyoNight Night**, the live window kept a clean dark caption and surface. The title remained beside the traffic lights and the rounded top corners had no clear strip or seam. |
| Toggle Reduce Transparency live | With `background-opacity = 0.86` and `window-blur = true`, Accessibility > Display changed **Reduce transparency** from off to on and back while the same Kettle window stayed open. A fixed `700 x 400` crop of the terminal surface changed from mean RGB `(35,37,49)` to `(29,31,43)` when reduction was on, then returned exactly to `(35,37,49)` when switched off. The window needed no relaunch and kept clean corners throughout. |

### Still not capturable

**Dock magnification.** The System Settings slider was driven from `0` to `1`
and back successfully. The automation screenshot for Finder exposes the desktop
but filters the Dock out, and attempting a coordinate action on that desktop is
rejected as `noWindowsAvailable`. That compositor/app boundary prevents a
screen-derived judgement of the magnified icon; the setting itself is no longer
the blocker.

**The app-switcher icon.** An app-targeted `Cmd+Tab` was sent to the live Kettle
window, but Computer Use cannot invoke global shortcuts and its app-scoped
screenshot did not expose the switcher. This is now specifically blocked by
system-UI capture and global-shortcut isolation, not by missing Automation
permission.

## 3.2.1 — 2026-08-24

Host: macOS 26 (Darwin 25.6), Apple silicon, system appearance **dark**
throughout.

Bundle: a universal `kettle.app` at 3.2.1 assembled from the release job's own
steps — `compile-macos-app-icon.sh`, the same `Info.plist` with its version
patched, a `lipo` of the `aarch64-apple-darwin` and `x86_64-apple-darwin`
release binaries — then ad-hoc signed. `lipo -archs` reports `x86_64 arm64`.

Windows were driven through `kettle ctl` (`focus_window`, `toggle_fullscreen`,
`toggle_light_dark`, `ui_geometry`). Every judgement below comes from a screen
capture, and the traffic-light rows come from classifying one pixel row across
the titlebar.

### Passed

| Check | Result |
|---|---|
| 86% opacity, blur on: one material to both rounded top corners | No clear strip, no seam. Lights clean, title beside them. |
| Full-screen round trip | Content rect `1600 x 1159.5` at `y = 40.5` before and after, byte-identical. |
| `borderless = true` full-screen round trip | Terminal stayed visible through the sharp-alpha fallback, not covered by a material view. Geometry identical across the round trip. |
| Alpha on, blur off | AppKit drew its standard opaque titlebar backdrop. No clear desktop strip. |
| Opaque surface, blur on | No titlebar-only material seam; caption matches the theme. |
| `background-opacity = 1.0`, `window-blur = false` | Theme reached both rounded top corners, no mismatched strip. |
| Live light/dark switch | Four consecutive `toggle_light_dark` steps: Alabaster `(226,226,226)` → TokyoNight `(40,38,35)` → Alabaster `(255,255,255)` → TokyoNight `(40,38,35)`. Drives the appearance in both directions, repeatably. |
| **Start on a light theme** | New row this release, and the one 3.2.0 lacked. `theme = Alabaster` from launch: caption present, traffic lights clean, full `~ — kettle` beside them. This is [#251](https://github.com/Reddimus/kettle/issues/251), fixed. |
| Dock icons, running and pinned | Both render the system mask with a blue rim over a dark inset face, clear rim space, `>_` centered and legible. The running build and the installed bundle agree. |
| Finder icon at 128 px | Matches the Dock rendering. |
| Both 256 px appearances | `Assets.car` draws a blue rim over a dark face; the `AppIcon.icns` deployment-target fallback inverts them. Both keep the system mask, a parallel inset face, clear rim space, and a centered legible mark. |

### Not run

**Toggling Reduce Transparency live.** Unchanged from 3.2.0: TCC refuses
`defaults write com.apple.universalaccess`, and the System Settings switch needs
foreground pointer input that the automation policy blocks.

**Dock magnification.** It only renders under the pointer, and global pointer
control is refused.

**The app-switcher icon.** ⌘-Tab is foreground input, refused for the same
reason. The Dock and Finder renderings agree, which is the same asset, but the
switcher itself was not captured.

**System appearance light with a dark theme.** The inverse of the #251 case. By
the mechanism it is safe — the fix moves every macOS appearance application to
after AppKit builds the caption, in either direction — but flipping the whole
desktop's appearance was out of scope for this session, so it is reasoning
rather than a run.

## 3.2.0 — 2026-08-23

Host: macOS 26 (Darwin 25.6), Apple silicon, 1920x1080 at 1x, system appearance
dark.

Bundle: a universal `kettle.app` assembled from the release job's own steps —
`compile-macos-app-icon.sh` through Xcode 26.6, the same `Info.plist`, and a
`lipo` of `aarch64-apple-darwin` and `x86_64-apple-darwin` release binaries —
then ad-hoc signed. Same tree as the cut, version strings aside. Signing and
notarization are covered by the release workflow and are not what this gate is
for.

Windows were driven through `kettle ctl`, and every judgement below comes from a
screen capture rather than from source.

### Passed

| Check | Result |
|---|---|
| 86% opacity, blur on: one material to both rounded top corners | No clear strip, no seam. Traffic lights in native positions. |
| Resize, then a full-screen round trip | Content rect returned to `1100 x 675.75` at `y = 24.25`, byte-identical to before. Lights, drag region, first row and pointer targets unmoved. |
| `borderless = true` full-screen round trip | Terminal stayed visible through the sharp-alpha fallback. Geometry restored exactly. |
| Alpha on, blur off | AppKit drew its standard titlebar backdrop. No clear desktop strip. |
| Opaque surface, blur on | No titlebar-only material seam. |
| `background-opacity = 1.0`, `window-blur = false` | Theme reached both rounded top corners, no mismatched strip. |
| Live light/dark switch | `ToggleLightDark` on a `light-theme`/`dark-theme` pair took the titlebar from `(29,29,29)` to `(255,255,255)` and back. This is the runtime `set_theme` path, not just the creation-time hint. |
| Dock icons, running and pinned | Same system mask, inset face parallel with clear rim space, `>_` centered and legible. The running build and the installed bundle agree. |
| Finder icon | Matches the Dock rendering. |
| Both 256 px appearances | `Assets.car` draws a blue rim over a dark face; the `AppIcon.icns` deployment-target fallback inverts them. Both keep the system mask, a parallel inset face, clear rim space, and a centered legible mark. |

### Failed at the gate, fixed after the release

**Light themes draw the window title through the traffic lights**
([#251](https://github.com/Reddimus/kettle/issues/251)). The title landed at the
far left of the titlebar, over the red and yellow buttons, with its leading
characters clipped outside the window. Reproduced on `Alabaster`, `3024 Day` and
`Adwaita`; not on `TokyoNight`. The shipped 3.1.1 bundle did the same, so it was
not a regression, and it survived a full-screen re-layout, so it was not a stale
frame.

3.2.0 shipped with it: cosmetic, not new, and the release carried a crash fix,
an installer downgrade fix and a session data-loss fix worth more than holding
for it. It is fixed on `main`.

The gate write-up guessed the cause was AppKit's appearance switch, outside code
this repo owns. Reading the live `NSThemeFrame` settled it, and the guess was
wrong. The two cases are structurally different:

| | dark theme | light theme |
|---|---|---|
| caption | `NSTitlebarContainerView` 0,600 800×32 | absent |
| buttons | inside the container | loose on the frame view, x = 9 / 32 / 55 |
| title | inside the container | `NSTextField` x = **-10**, w = 92 |

`(72 - 92) / 2 = -10`: with no caption to centre the title in, AppKit centres it
over the 72-point traffic-light cluster. The trigger is kettle's own doing —
`native_material` adds an `NSVisualEffectView` to AppKit's frame view, and doing
that after winit has applied a creation-time appearance override stops the
caption from ever being built. Any foreign subview does it, not just an effect
view; a plain `NSView` reproduces it. Nothing recovers afterwards: removing the
view again leaves the caption gone.

The fix is to reach the window with no override and apply the theme hint once
the window is key, which is the path the live light/dark switch above already
exercised — that row passing while startup failed is exactly the clue.

Verified live on the same desktop, one pixel row across the titlebar:

```
before  ..  RRRRRRRRR .   YY  YYY  YYY..   GGGGGGGGGGGG      title over the buttons
after   ....  RRRRRRRR      YYYYYYYYYY     GGGGGGGGGGGG      title beside them
```

### Not run

**Toggling Reduce Transparency live.** This session could not change the
setting: `defaults write com.apple.universalaccess` is refused by TCC, and the
System Settings switch needs foreground pointer input, which the automation
policy here blocks. The portable policy tests still cover the Reduce
Transparency state; what stays unproven is that the material disappears and
returns *live*, without a relaunch.

**Dock magnification.** Magnification only renders under the pointer, and
pointer control was blocked for the same reason. The 256 px asset the magnified
Dock draws was checked directly and the mark is centered and legible at that
size.

**The app switcher.** Cmd-Tab has to be held for its window to stay up, which is
foreground keyboard input. The icon it shows comes from the same bundle
resources as the Dock and Finder renderings above, both of which agree.
