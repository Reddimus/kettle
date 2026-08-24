# macOS appearance gate

[`RELEASING.md`](RELEASING.md#macos-appearance-gate) requires a native run
before the release-cut pull request merges. Unit and image tests can prove the
material policy; they cannot prove what AppKit actually draws. This file records
each run, including what did not run and why.

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
