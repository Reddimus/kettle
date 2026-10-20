# macOS appearance gate

[`RELEASING.md`](RELEASING.md#macos-appearance-gate) requires a native run
before the release-cut pull request merges. Unit and image tests can prove the
material policy; they cannot prove what AppKit actually draws. This file records
each run, including what did not run and why.

## 5.4.0 cut - 2026-10-19

The release was prepared on Linux (Ubuntu, Intel Iris Xe, native Wayland).
A native macOS desktop was unavailable. The macOS appearance checks and
`just macos-update-smoke` were skipped; the Linux renderer and updater fixtures
do not establish AppKit appearance, Gatekeeper acceptance after a live update,
or native macOS media-player interaction.

The feature PR's required macOS build and regression checks passed in CI run
39341283460. Its separate macOS native fullscreen/caption job also passed.
Those automated checks do not replace the manual appearance gate.

On Linux, the strict repository gate, complete native gate, 24 simulated agent
card cases, media-viewer and paging cases, and audio/confinement smoke passed.
Native Wayland interaction verified the padded split and Theater geometry,
compositor fullscreen, and Escape restoration. Installed Codex 0.163.0
delivered all nine media types in a live run. Live Claude Code acceptance was
skipped because the installed client's weekly usage quota was exhausted;
fixtures establish Kettle's side only.

Not run: native macOS material/corner/opacity captures, Reduce Transparency,
Dock/Finder/app-switcher icon checks, manual media playback, the signed-bundle
updater smoke, and physical Intel or HiDPI macOS acceptance.

## 5.3.0 cut - 2026-10-17

macOS 27.0.1, M5 Max, 1920x1080 at 1x, system appearance Dark. The candidate
is the release workflow's macOS package from the `workflow_dispatch` dry run
39095632272 on `616f9797`: universal (x86_64, arm64, worker included),
Developer ID signed, notarized and stapled, accepted by `spctl`. The release
tree adds to it only #631's change to `kettle show`'s output order, the
changelog and this cut's version bump. 5.3 changes no window chrome (titlebar,
material, fullscreen, Dock or accent code) since 5.2.0's live run earlier the
same day.

Passed:

- `scripts/check-native-titlebar.py` against the candidate's binary: a light
  theme started under Dark draws its caption clear of the traffic lights at
  startup, after a resize and with the pointer over it; native fullscreen
  entered with the green button leaves on one toggle and restores 740x480; a
  window started in fullscreen leaves it on one toggle; a borderless window
  round-trips programmatic fullscreen back to 740x480.
- The new cards, live in the candidate: `kettle show --inline` printed a
  recognized card under the command for a PNG, an SVG, a Mermaid file, a PDF
  (`pdf 875x583 2/2` with `--page 2`), a HEIC, a tiled HEIC, an AVIF, a TIFF
  and a `.docx` file card (`File · 2.3 kB`), each drawn by the signed worker;
  through a pipe it went to the shelf alone with its note.
- `just macos-update-smoke` passed against the published v5.2.0 archive and
  against this candidate's archive.

Not run: the material, corner and opacity captures (unchanged window code;
5.2.0's run covers them), live Reduce Transparency, Dock, Finder and
app-switcher icons, and HiDPI or physical Intel hardware.

## 5.2.0 cut - 2026-10-17

macOS 27.0.1, M5 Max, 1920x1080 at 1x, system appearance Dark. The candidate
is the release workflow's own macOS package from the `workflow_dispatch` dry
run 39032628161 on `25c8d48e`: universal (x86_64, arm64, worker included),
Developer ID signed, notarized and stapled, accepted by `spctl` as "Notarized
Developer ID". The release tree differs from `25c8d48e` only by comments,
documentation, test files moved by #619 and this cut's version bump, so the
candidate runs the release's window code. This cut changes window chrome
(`app/chrome.rs`, windowing, the Dock menu and window accents), so the run was
live, with the console unlocked.

Passed:

- `scripts/check-native-titlebar.py` against the candidate's binary: a light
  theme started under Dark draws its caption clear of the traffic lights (no
  caption ink over them) at startup, after a resize and with the pointer over
  the caption; native fullscreen entered with the green button (pressed through
  Accessibility) leaves on one toggle and restores 740x480; a window started in
  fullscreen leaves it on one toggle; a borderless window resizes and
  round-trips programmatic fullscreen back to 740x480 with the terminal
  visible.
- Over a second, opaque saturated Kettle window filled with text, inspected
  through Computer Use captures: at the default 86% opacity with blur, the
  opaque native titlebar and the material reach both rounded top corners with
  no clear strip or seam, and the text behind is blurred; with blur off it shows
  through sharp; at `background-opacity = 1.0` and `window-blur = false` the
  theme fills every corner and nothing shows through.
- On the default blurred window, a control-API resize to 900x600, then a
  fullscreen round trip (1920x1080), restores the same surface and content
  rectangles exactly; the traffic lights, caption and first terminal row are
  where they were.
- `just macos-update-smoke` passed against the published v5.1.0 archive and
  against this candidate's archive.

Not run: live Reduce Transparency (a system setting this run does not change),
Dock, Finder and app-switcher icons (Computer Use captures filter the Dock),
and HiDPI or physical Intel hardware.

## 5.1.0 cut - 2026-10-15

macOS 27.0.1, M5 Max. Universal ad-hoc bundle built from `91e58265` (the
release tree before its version bump), with the Xcode 26 icon files from CI
run `38799607492`. Both architectures are present (`lipo`: x86_64, arm64).

**The live run did not happen.** The console session was locked for the whole
window this cut had: `loginwindow` held secure input and no app could become
active, so no window could become key. Captures of a window that is not key
show neither its content nor its theme-matched caption, and macOS will not
enter native fullscreen for an inactive app, so nothing from that attempt is
evidence.

What stands in for it, and why it is enough for this release only:

- The macOS window code is the code 5.0.1's live run passed. Since v5.0.1,
  `window_attributes` and `apply_post_create` are unchanged, the material,
  fullscreen, cursor-layer and display modules are unchanged, and the first
  frame on a new window still redraws, then requests a redraw, as before;
  only Wayland's path changed.
- CI's macOS 26 "macOS native fullscreen and caption" job passed on this tree
  with a live window: light startup caption beside the traffic lights, native
  and programmatic fullscreen round trips, and borderless resize.
- The same native titlebar check passed three times on this Mac earlier the
  same day, on the tree that added this release's window and session fixes.
- `just macos-update-smoke` passed against the published v5.0.1 archive.

Not run: the blur, opacity and borderless captures, light startup under Dark
by eye, live Reduce Transparency, Dock/Finder/app-switcher icons, and HiDPI or
physical Intel hardware. Run them on the next cut that changes window chrome.

## 5.0.1 cut - 2026-10-13

macOS 27.0.1, M5 Max, 1920x1080 at 1x. Universal ad-hoc bundle built from
`46ca35208efc` plus the fullscreen correction; source hash
`dcef2f2e6a4ee190`. Both architectures load (Intel through Rosetta). The icon
resources come from the cut's passing macOS 26 CI job. Signing and notarization
are checked on the official release candidate separately.

Native window inspection and control geometry checks passed:

- Default blur, blur off, opaque with/without blur, borderless, and light startup
  show readable captions and corners, with terminal content visible.
- All six fullscreen round trips restore the preceding surface and content
  rectangles exactly. Decorated resize used native zoom; borderless resize used
  the control API, since bound pointer drags did not resize the fixture.
- After the native green button enters fullscreen, one F11 restores the window.
  The earlier candidate required two toggles; this regression drove the fix.
- Startup fullscreen exits on one F11. AppKit chooses the restored size for this
  case; it need not equal a separately launched window's configured size.
- Live dark/light/dark configuration reloads retain a clear native caption.
- Detached windows have different accent colors. Current tearoff setup used the
  control API; it does not establish physical single-tab reattachment.

Native captures were inspected through Computer Use; renderer PNGs and geometry
receipts were retained separately. This run does not establish checkerboard blur
composition, physical reattachment, Dock/Finder/app-switcher appearance, HiDPI or
physical Intel behavior. Reduce Transparency, system appearance and Dock settings
were left unchanged. The macOS 26 CI suite checks native caption placement and
the native-button fullscreen regression.

## 4.5.0 cut - 2026-09-21

macOS 26.6.2, M5 Max, 1920x1080 at 1x. Universal bundle built from clean
`0aaaa34ac0a3`, with Xcode 26 icons from CI run `35570977894`. Its merge tree
matches that cut. Both architectures load; the Intel check uses Rosetta.
Later cut changes only synchronize test fixtures and record validation.

Native window captures and geometry checks passed:

- Blur on/off at 86% and 100% opacity; no clear titlebar seam.
- Decorated and borderless resize/fullscreen round trips; content restored exactly.
- Light-theme startup under Dark, dark-theme startup under Light, and live palette
  changes; titles stay beside the traffic lights.
- Live Reduce Transparency off/on/off; background and titlebar return to their
  original pixels. System appearance and accessibility settings were restored.
- AccessKit exposes the live terminal text through the native accessibility tree.

The real published 4.4.0 archive also passed `macos-update-smoke`, including
Gatekeeper verification after extraction and replacement.

Dock/menu and closed-but-pinned checks were skipped to preserve an existing
Kettle session; Dock capture is unavailable. Finder, app-switcher and magnified
icons were not checked. No native HiDPI or Intel hardware was available. This
bundle is ad-hoc signed; release signing and notarization are checked separately.

## 4.5.1 cut - 2026-09-22

Clipboard routing only changed; renderer, window chrome, material and icon
sources are unchanged. The visual results above remain applicable. The native
macOS image-paste CI route passed, and `just macos-update-smoke` passed against
published v4.5.0.

The rebuilt appearance suite was skipped: this host has Xcode 27, while the
release icon script requires Xcode 26.x. The macOS 26 CI icon job passed.
`just dock-menu-smoke` was skipped because the existing Kettle instance is
running; the recipe refuses to run alongside another instance. Dock icon
capture remains unavailable.

## 4.5.2 cut - 2026-09-26

Only working-directory and OSC 7 hostname logic changed. Renderer, window
chrome, material and icon sources are unchanged, so the 4.5.0 visual results
still apply. `just macos-update-smoke` passed against published v4.5.1, and
`dock-menu-smoke` passed on the merged fix.

The rebuilt appearance suite was skipped again: this host has Xcode 27, and the
release icon script requires Xcode 26.x. The macOS 26 CI icon job passed. Dock
icon capture remains unavailable.

## 4.6.0 cut - 2026-09-27

macOS 26.6.2 (25G83), M5 Max. Release build of the cut commit `a4c0a536`.

Window chrome, material and icon sources are unchanged since 4.5.0, so the
4.5.0 visual results still apply. The one visible change is the cursor-blink
timeout (#347), which adds a "Stop blinking after" row to Settings →
Appearance. Captured from the cut's release build with `kettle ctl
screenshot`, the row sits under "Cursor blink" and reads `10s` by default.
With `cursor-blink = false` and `cursor-blink-timeout = 0` it is dimmed and
reads `never`. The blink itself was measured live: a focused idle window stops
drawing after the timeout, as recorded in PERFORMANCE.md.

`just gauntlet-strict` passed on the cut, and so did 15 of the 17 macOS native
gates: `icons-check-required`, `package-templates`, `update-manifest-test`,
`release-assets-test`, `package-manifest-test`, `online-installer-test`,
`gpu-render-smoke`, `cli-smoke`, `touchpad-scroll-smoke`,
`split-exit-resize-smoke`, `text-presentation-smoke`,
`line-edit-chords-smoke`, `macos-compare-score-self-test`,
`macos-standing-self-test` and `agent-cli-smoke`. `just macos-update-smoke`
passed against published v4.5.2; `kettle-update` is unchanged since then.

`icns-smoke` was skipped again: this host has Xcode 27, and the release icon
script requires Xcode 26.x. The macOS 26 CI icon job covers it.
`dock-menu-smoke` could not drive System Events, because macOS denied Apple
Events to this session's host process (-1743); granting it Automation
permission was out of scope. Dock icon capture remains unavailable.

## 4.7.0 cut - 2026-09-28

macOS 26.6.2 (25G83), M5 Max. Release build of the cut commit `54f883fb`.

Window chrome, material and icon sources are unchanged since 4.5.0, so the
4.5.0 visual results still apply. The visible changes were captured from the
cut's release build with `kettle ctl screenshot`:

- A confirm bar draws its destructive button bold: "Close" is bold beside a
  regular "Cancel", and the brackets stay aligned.
- A disabled Settings row is readable. With `cursor-blink = false`, "Stop
  blinking after 10s" is dimmed but legible.
- The status-bar clock shows local time, with no "UTC".
- `window-width = 100` and `window-height = 24` open a 100x24 grid exactly.

The cursor-blink change (#378) and the earlier first shell (#379) were
measured live; their pull requests have the numbers.

`just gauntlet-strict` passed on the cut, and so did 16 of the 17 macOS native
gates: `icons-check-required`, `package-templates`, `update-manifest-test`,
`release-assets-test`, `package-manifest-test`, `online-installer-test`,
`gpu-render-smoke`, `cli-smoke`, `touchpad-scroll-smoke`,
`split-exit-resize-smoke`, `text-presentation-smoke`,
`line-edit-chords-smoke`, `dock-menu-smoke`, `macos-compare-score-self-test`,
`macos-standing-self-test` and `agent-cli-smoke`. `dock-menu-smoke` ran this
time; at the 4.6.0 cut macOS denied it Apple Events. `just macos-update-smoke`
passed against published v4.6.0; since then `kettle-update` has changed only
in comments.

`icns-smoke` was skipped again: this host has Xcode 27, and the release icon
script requires Xcode 26.x. The macOS 26 CI icon job covers it.

## 4.8.0 cut - 2026-09-28

macOS 26.6.2 (25G83), M5 Max. Release build of the cut commit `829f51cb`.

Window chrome, material and icon sources are unchanged since 4.5.0, so the
4.5.0 visual results still apply. The visible changes were captured from the
cut's release build with `kettle ctl screenshot`:

- The search bar's labels sit centered in their buttons and the editor. With
  Next focused, its dark label reads clearly on the accent, and the Wrap,
  Case and Enter toggles keep their names in place. Every step of the search
  bar was also recorded and measured at 40 to 200 columns and 10 to 18 pt, and
  on Linux; #391 has the numbers.
- The right-click menu shows Split Left and Split Up among the splits, and
  Close Tab, New Window, Close Window and Set Window Title… after New Tab.
- A grid line holding a paragraph separator between Latin and Hebrew renders,
  and the window stays up.

`just gauntlet-strict` passed on the cut, and so did 16 of the 17 macOS native
gates: `icons-check-required`, `package-templates`, `update-manifest-test`,
`release-assets-test`, `package-manifest-test`, `online-installer-test`,
`gpu-render-smoke`, `cli-smoke`, `touchpad-scroll-smoke`,
`split-exit-resize-smoke`, `text-presentation-smoke`,
`line-edit-chords-smoke`, `dock-menu-smoke`, `macos-compare-score-self-test`,
`macos-standing-self-test` and `agent-cli-smoke`. `just macos-update-smoke`
passed against published v4.7.0.

`icns-smoke` was skipped again: this host has Xcode 27, and the release icon
script requires Xcode 26.x. The macOS 26 CI icon job covers it.
`package-templates` skipped its release-asset check because v4.8.0 is not
published yet; the post-release check runs it with `--require-release`.

## 4.9.0 cut - 2026-10-02

macOS 26.6.2 (25G83), M5 Max. Release build of the cut commit `e69245d1`.

Window chrome, material and icon sources are unchanged since 4.5.0, so the
4.5.0 visual results still apply. This release changes how frames reach the
screen: mapped uploads, shared pipelines, the shaping of trailing blanks, and
on macOS a Core Animation layer that blinks the cursor. The cut was therefore
compared with the published 4.8.0 pixel for pixel. Both builds rendered one
fixed screen of text: the 16, 256 and truecolor palettes, every underline
style, bold, italic, reverse and padded rows, wide, emoji, combining and
right-to-left text, box drawing, braille and ligatures. Their `kettle ctl
screenshot` frames were identical opaque, at 86% opacity with blur, and split
in two.

The visible changes were captured from the cut's release build:

- Settings shows values in one case and with spaced units (`On`, `Block`,
  `13 pt`, `10 s`), underlines the open category, and spaces the category
  names evenly. In a 64-column window the categories and the key hints end in
  an ellipsis instead of running past the panel.
- With `macos-cursor-blink-layer` on, the window server blinks the cursor once
  the window is idle. `just cursor-blink-layer-smoke --pixels` matched the
  layer's on and off phases to Kettle's own frames for block, bar and
  underline cursors, opaque and at 86% opacity with blur, at `09ceae2d`; the
  cut changes no crate source since then. Its wakeup and CPU budgets were
  relaxed, because the host carried a sustained load of 7 to 14 from
  long-running jobs, and 4.9.0 makes no new measured performance claim. On the
  cut itself the pixel comparison did not finish under that load. One attempt
  timed out starting its control server, and two ran out of the smoke's
  10-second budget for fitting each capture inside one blink phase.

`just gauntlet-strict` passed on the cut, and so did 16 of the 17 macOS native
gates: `icons-check-required`, `package-templates`, `update-manifest-test`,
`release-assets-test`, `package-manifest-test`, `online-installer-test`,
`gpu-render-smoke`, `cli-smoke`, `touchpad-scroll-smoke`,
`split-exit-resize-smoke`, `text-presentation-smoke`,
`line-edit-chords-smoke`, `dock-menu-smoke`, `macos-compare-score-self-test`,
`macos-standing-self-test` and `agent-cli-smoke`. `just macos-update-smoke`
passed against published v4.8.0.

`icns-smoke` was skipped again: this host has Xcode 27, and the release icon
script requires Xcode 26.x. The macOS 26 CI icon job covers it.
`package-templates` skipped its release-asset check because v4.9.0 is not
published yet; the post-release check runs it with `--require-release`.

## 5.0.0 cut - 2026-10-11

macOS 27.0.1 (26A434), M5 Max. Release build of the cut's release commit
`158153a9`, built as `58b5bd76`: the same tree, before the cut was rebased onto
the merged changelog pull request.

Window chrome, material and icon sources are unchanged since 4.5.0, so the
4.5.0 visual results still apply. 5.0 adds surfaces of its own (inline media
cards, preview lanes beside a pane, and an Agents category in Settings) rather
than changing how the existing ones draw. The cut was compared with the
installed 4.9.0 (`ea0e36fd`) pixel for pixel, on the fixture the 4.9.0 cut
used: one fixed screen of text with the 16, 256 and truecolor palettes, every
underline style, bold, italic, reverse and padded rows, wide, emoji, combining
and right-to-left text, box drawing, braille and ligatures. Their `kettle ctl
screenshot` frames were identical opaque and at 86% opacity with blur.

Split in two, 180 pixels differed, and all of them belong to the two panes'
scrollbar thumbs. 5.0 runs a pane's scrollbar along its terminal, clear of the
pane's titlebar, so that a preview lane can sit beside the terminal; 4.9.0 ran
it the whole height of the pane. Only split panes have titlebars, so only they
show the change. The text is identical.

Settings was captured from both builds, wide and in a 64-column window. In
the wide window 5.0 adds the Agents category, and the panel grows to fit it.
The 64-column captures are identical: the categories and the key hints end in
an ellipsis before Agents would show, as they do in 4.9.0.

`just gauntlet-strict` passed on the release commit. The 17 macOS native gates
passed on the first release commit, `7ed542a5`: `icons-check-required`,
`package-templates`, `update-manifest-test`, `release-assets-test`,
`package-manifest-test`, `online-installer-test`, `gpu-render-smoke`,
`cli-smoke`, `touchpad-scroll-smoke`, `split-exit-resize-smoke`,
`text-presentation-smoke`, `line-edit-chords-smoke`, `dock-menu-smoke`,
`macos-compare-score-self-test`, `macos-standing-self-test`,
`macos-avsync-self-test` and `agent-cli-smoke`. That commit left
`fuzz/Cargo.lock` naming the media crates at 4.9.0, which gauntlet-strict
refused, so `scripts/release.sh` now refreshes it and the release commit was
made again. The release commit differs from `7ed542a5` only in that lockfile,
the release script, its tests and `RELEASING.md`.

`just macos-update-smoke` cannot pass against published v4.9.0. Since 5.0
the updater requires the media worker in every macOS archive, and the 4.9.0
archive predates the worker. A 4.9.0 install updating to 5.0.0 runs 4.9.0's
own updater, which replaces the whole bundle, so installs are unaffected. As
[`RELEASING.md`](RELEASING.md) directs at such a boundary, `release.yml` was
dispatched on main `a9fda8f9` (run `38182915046`) for a signed, notarized
candidate archive in the 5.0 shape, and the live updater test passed against
it, its sidecar checked. Neither the updater nor the macOS packaging changed
between that commit and the cut.

`icns-smoke` was skipped again: this host has Xcode 27, and the release icon
script requires Xcode 26.x. The macOS 26 CI icon job covers it.
`package-templates` skipped its release-asset check because v5.0.0 is not
published yet; the post-release check runs it with `--require-release`.

## 4.3.0 cut — 2026-09-04

Host: macOS 26.6.2 (25G83), Apple silicon, system appearance **Dark**. Bundle: a
universal `kettle.app` built from the clean cut commit `5c12f96f`, replicating
`release.yml`'s `Build (macOS universal)` and `Package (macOS .app bundle)`
steps exactly — both `--target` release builds, `lipo`,
`scripts/compile-macos-app-icon.sh` through Xcode 26.6, and the PlistBuddy
version patch. `lipo -archs` reports `x86_64 arm64` and
`CFBundleShortVersionString` is `4.3.0`. The plist was patched through a copy,
so the cut worktree stayed clean at that SHA; verified after the run.

This release adds a macOS keybinding default and changes no rendering,
material, window chrome or icon code. The window and icon checks below are
therefore run as policy; the chord row is the one the diff actually puts at
risk.

Window frames were captured with `screencapture -l<windowid>`, which reads only
kettle's own window layer. No full-screen capture was taken, so nothing outside
kettle was recorded at any point.

### Passed

| Check | Result |
|---|---|
| Default 86% opacity, native blur on | Zero clear pixels inside the opaque span at every sampled row. The titlebar at y=8 is a uniform `(34,36,39,255)` from four pixels inside the left end of the span to four inside the right. The span widens from `10..805` at y=0 to `1..814` at y=8 and `0..815` by y=16 — the corner radius, not a gap. |
| Alpha on, blur off | Byte-identical titlebar measurements to the row above: uniform `(34,36,39,255)`, zero clear pixels. AppKit supplied its standard titlebar backdrop rather than exposing a clear desktop strip. The terminal row at y=60 reads `(29,33,44,219)`; alpha 219 is the configured 86%. |
| Opaque surface, blur left on | Same uniform titlebar to both corners, zero clear pixels, no titlebar-only seam. Terminal row `(26,27,38,255)`, fully opaque. |
| Opaque surface, blur off | Identical to the row above. The active theme reaches both rounded top corners with no clear or mismatched strip. |
| `borderless = true` | No titlebar; the span is the full `0..815` from y=0. Rows 0..30 are kettle's own tab bar, so they are legitimately non-uniform. The first terminal row below it, at y=60, is a uniform `(29,33,44,219)` across the full width — alpha 219 is the configured 86%, so the terminal stays visible through the documented sharp-alpha fallback instead of being covered by a material view. |
| Full-screen round trip | `ui_geometry`'s `content` rect is byte-identical before and after: `{"height": 463.75, "width": 816.0, "x": 0.0, "y": 24.25}` → fullscreen `{"height": 1055.75, "width": 1920.0, ...}` → back to the original values exactly. |
| Light theme at **startup** under a Dark system ([#251](https://github.com/Reddimus/kettle/issues/251)) | Measured through the accessibility API rather than by eye. Traffic lights sit at x=560, 583 and 606; the title element starts at **x=634**, value `~ — kettle`. The last button ends near x=622, so the title begins beside the cluster rather than across it. This is the startup path, not the runtime toggle. |
| NSWindow background follows a live palette change | Started on Catppuccin Latte and captured: titlebar `(255,255,255,255)`, content `(239,241,245,255)`. After `perform_action next_theme`: titlebar `(34,36,39,255)`, content `(36,39,58,255)`. The NSWindow background tracks the palette at runtime, not only at startup. |
| **`Cmd+Opt+Arrow` split focus (new in this release)** | Driven as real keystrokes through the window server against this bundle's own binary, in a deterministic 2x2 split, asserting the expected target pane for every direction from every pane: **16/16**. `Ctrl+Cmd+Arrow` scored **16/16** in the same run, so the pre-existing chord did not regress. `Cmd+]` / `Cmd+[` were confirmed inert, which is deliberate: logical bracket bindings conflict with keyboard layouts and zoom (see [Key design choices](ARCHITECTURE.md#key-design-choices)). |

### Not run

| Check | Why |
|---|---|
| Reduce Transparency toggle | **Not attempted.** It is a system accessibility setting, and changing system settings is outside what this environment does. Unchanged from the 4.2.0 cut. |
| Dock icon screenshots, running and closed-but-pinned | The Dock is filtered out of automation captures at the allowlist level. The Dock *menu* remains reachable through the accessibility API and `just dock-menu-smoke` passed in `full-native-gates`, but the drawn icon still cannot be photographed here. |
| Signed, notarized, stapled bundle | No Developer ID or App Store Connect secrets exist on this machine; the bundle above is unsigned. `release.yml`'s `macos-signing` environment performs signing, notarization, stapling and the Gatekeeper assessment, and the published archive is verified after the tag. |
| Non-US keyboard layout, live | Switching the active input source is a system setting. Layout behaviour was measured instead by calling `UCKeyTranslate` through `TISInputSource` for each installed layout, read-only, which is what settled the `Cmd+[` / `Cmd+]` decision. |

## 4.2.0 cut — 2026-09-02

Host: macOS 26.6.2 (25G83), Apple silicon, system appearance **Dark**. Bundle: a
universal `kettle.app` built from the clean cut commit `c57b0328`, replicating
`release.yml`'s `Build (macOS universal)` and `Package (macOS .app bundle)`
steps exactly — both `--target` release builds, `lipo`,
`scripts/compile-macos-app-icon.sh` through Xcode 26.6, and the PlistBuddy
version patch. `lipo -archs` reports `x86_64 arm64` and
`CFBundleShortVersionString` is `4.2.0`. The plist was patched through a copy,
so the cut worktree stayed clean at that SHA; verified after the run.

This release adds a macOS Dock context menu. It touches the application
delegate and `NSApplication.windowsMenu`, but no rendering, material, window
chrome or icon code — so the window and icon checks below are run as policy,
while the Dock row is the one the diff actually puts at risk.

Window frames were captured with `screencapture -l<windowid>`, which reads only
kettle's own window layer. No full-screen capture was taken, so nothing outside
kettle was recorded at any point.

### Passed

| Check | Result |
|---|---|
| Default 86% opacity, native blur on | Zero clear pixels inside the opaque span on every sampled row. The titlebar material is a uniform `(33,34,36,255)` from four pixels inside the left end of the span to four pixels inside the right end, at both y=8 and y=16. The span narrows to `26..789` at y=0 and widens to `3..812` by y=16 — that is the corner radius itself, not a gap. |
| Alpha on, blur off | Byte-identical measurements to the row above: uniform `(33,34,36,255)` to both corners, zero clear pixels. AppKit supplied its standard titlebar backdrop rather than exposing a clear desktop strip. |
| Opaque surface, blur left on | Same uniform material to both corners, zero clear pixels, and no titlebar-only seam. |
| Opaque surface, blur off | Identical. The active theme reaches both rounded top corners with no clear or mismatched strip. |
| `borderless = true` | No titlebar; the span is the full `0..815` from y=0. Rows 0..30 are kettle's own tab bar; the first terminal row below it, sampled at y=60, is a uniform `(29,33,44,219)` across 810 of 816 pixels. Alpha 219 is the configured 86%, so the terminal remains visible through the documented sharp-alpha fallback instead of being covered by a material view. |
| Full-screen round trip | `ui_geometry`'s `content` rect is byte-identical before and after: `{"height": 447.5, "width": 816.0, "x": 0.0, "y": 40.5}` → fullscreen `{"height": 2127.5, "width": 3456.0, ...}` → back to the original values exactly. |
| Light theme at **startup** under a Dark system ([#251](https://github.com/Reddimus/kettle/issues/251)) | Measured through the accessibility API rather than by eye. Traffic lights occupy x=668..730 (close 668, minimize 691, full-screen 714, each 16 wide); the title element starts at **x=742**, width 69, value `~ — kettle`. The title therefore begins 12 px to the right of the last button and sits beside the cluster, not across it. This is the startup path, not the runtime toggle. |
| NSWindow background follows a live palette change | Driven with `perform_action next_theme` and re-captured each time. The titlebar and content both read `(255,255,255,255)` on the light theme at startup and `(33,34,36,255)` after switching to a dark one, so the NSWindow background tracks the palette at runtime and not only at startup. |
| **Dock context menu (new in this release)** | Read from the shipping bundle's own Dock tile through the accessibility API, selecting the tile by `AXIsApplicationRunning` so the pinned-but-not-running `kettle.app` tile cannot be mistaken for it. The menu enumerates `~ — kettle`, separator, `New Window`, `New Tab`, separator, `Options`, `Show All Windows`, `Hide`, `Hide Others`, `Quit`, `Force Quit`. Both the kettle-supplied rows and the AppKit window-title list are present; before this release the same enumeration returned only the system section. `just dock-menu-smoke` additionally clicks New Window and reads the window count 1 → 2 back over the control plane. |
| Icon geometry | `AppIcon.icns` 256 px rendering: rim L21 R21 T23 B19, horizontal centre offset 0.0 px — the inset face is parallel to the system mask with clear rim space on every side. `Assets.car` carries one `AppIcon` set spanning 32, 64, 128, 256, 512 and 1024 px across 7 `Icon Image` renditions plus a MultiSized Image and dark/light vector assets, so Finder, the Dock item and the app switcher all draw from one compiled asset and cannot disagree. |

### Not run

**Live blur compositing.** `screencapture -l<windowid>` reads kettle's own layer
and does not composite what is behind the window, so the blur-on and blur-off
scenarios produce identical bytes by construction — directly visible above,
where the default-86-blur-on and alpha-on-blur-off rows are byte-for-byte the
same. What the measurements establish is that no clear strip or seam exists and
that the material reaches both corners in every configuration, not that the blur
is visibly compositing. The 4.0.1 run has the working method for that question
(a second kettle instance as an opaque backdrop, judged from a composited
screenshot); it was not repeated here because no rendering code changed.

**Toggling Reduce Transparency live.** Not attempted. It is a system
accessibility setting, and this session does not change system settings. The
portable policy tests still cover the Reduce Transparency state; what stays
unproven is that the material disappears and returns *live*.

**Dock magnification, and the running / closed-but-pinned Dock icon
appearance.** Magnification renders only under the pointer, and the Dock is
still filtered out of automation screen captures at the allowlist level, so no
image of the drawn tile could be obtained. The 256 px asset the Dock draws was
measured directly above. Note the distinction from the row that did pass: the
Dock *menu* is reachable through accessibility and was read from the live tile;
the Dock *icon pixels* remain uncapturable.

**The app switcher.** Cmd-Tab must be held for its window to stay up. The icon
it shows comes from the same `Assets.car` measured above, which contains a
single AppIcon set for every size.

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
