# macOS appearance gate

[`RELEASING.md`](RELEASING.md#macos-appearance-gate) requires a native run
before the release-cut pull request merges. Unit and image tests can prove the
material policy; they cannot prove what AppKit actually draws. This file records
each run, including what did not run and why.

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

### Failed

**Light themes draw the window title through the traffic lights**
([#251](https://github.com/Reddimus/kettle/issues/251)). The title lands at the
far left of the titlebar, over the red and yellow buttons, with its leading
characters clipped outside the window. Reproduced on `Alabaster`, `3024 Day` and
`Adwaita`; not on `TokyoNight`. The shipped 3.1.1 bundle does the same, so it is
not a regression, and it survives a full-screen re-layout, so it is not a stale
frame.

Releasing with it: it is cosmetic, it is not new, and 3.2.0 carries a crash fix,
an installer downgrade fix and a session data-loss fix worth more than holding
for it.

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
