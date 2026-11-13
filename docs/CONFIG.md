# Configuration reference

Kettle uses a simple **`key = value` grammar**: one entry per line, the
first `=` splits key and value, surrounding whitespace is trimmed, only
full-line `#` comments are allowed (a `#` inside a value is part of the value,
so hex colors work), and some keys may repeat.

**Type notes.** `bool` keys accept the standard aliases (case-insensitive):
`true` / `yes` / `on` / `1` / `enabled` / `enable` / `y` for true,
and `false` / `no` / `off` / `0` / `disabled` / `disable` / `n` for false.
Unrecognized values keep the current setting and surface in `--check-config`.
Numeric keys with documented ranges
(`font-size`, `background-opacity`, `unfocused-split-opacity`,
`scroll-multiplier`, `minimum-contrast`, `cursor-blink-interval`,
`cursor-blink-timeout`) clamp at parse-time *and* flag out-of-range values via
`--check-config` and the in-window config notice.

Config path: kettle first probes `$XDG_CONFIG_HOME/kettle/config` (honored on
every OS). Failing that, the fallback is per-OS: on Unix/macOS
`~/.config/kettle/config`, and on Windows `%APPDATA%\kettle\config`. On Windows a
stray `HOME` (Git Bash / MSYS / WSL-interop all export one) is **intentionally
ignored** so a Start-menu launch and a shell launch read the same config — set
`XDG_CONFIG_HOME` if you genuinely want `~/.config` on Windows. Always run
`kettle --config-path` for the authoritative resolved location, or
`kettle --check-config` to validate it (resolved settings + any unrecognized
keys). Default and `--profile` configs are loaded only through directory chains
that another local principal cannot modify; Kettle-owned directories left too
permissive by an older release are repaired first. The file is **watched and
reloaded live** only under the same trust policy, and a refused reload keeps the
last known-good settings active. `--config FILE` is the deliberate exception:
supplying an exact path is an explicit trust grant for project-local or shared
configuration, while the regular-file and 1 MiB size checks still apply.

### Syntax

One `key = value` per line. The first `=` splits the pair, surrounding
whitespace is trimmed, and a `#` at the start of a line comments the whole
line out (a `#` inside a value is part of the value, so hex colours work).

Keys may be written with `-` or `_`; the two are the same key, so
`scroll-on-output` and `scroll_on_output` both work.

**One matched pair of surrounding quotes is stripped from a value.** This lets
quoted colors and numbers parse as expected instead of treating the quote as
part of the value. Only the
outermost pair goes, and only when both ends are the same character — so
`"a'` is left alone and inner quotes survive for values that legitimately
contain them, such as a shell command. If you need a value that really does
begin and end with a quote, add a second pair.

### Colour values

Anywhere a key takes a colour, all of these work:

| Form | Example |
|---|---|
| `#rrggbb` | `#7aa2f7` |
| `#rgb` (each digit doubled) | `#7af` |
| bare hex | `7aa2f7` |
| `0xRRGGBB` | `0x7AA2F7` |
| `rgb:R/G/B` (X11/xterm, 1–4 hex digits per channel) | `rgb:7a/a2/f7` |
| a named colour | `teal`, `orange`, `dodgerblue` |

Names are the 148 CSS Color Level 4 colours — the same list X11's `rgb.txt`
uses — matched case-insensitively, so `DodgerBlue` and `dodgerblue` are the
same. Two are kettle's own long-standing values rather than CSS's, because
configs were written against them: `green` is `#008000` and `gray`/`grey` is
`#bebebe`.

## Keys

| Key | Type | Default | Notes |
|---|---|---|---|
| `theme` | string | `TokyoNight Night` | Any bundled theme (`kettle --list-themes`). A `theme` line without `theme-mode` keeps a fixed theme for compatibility. Picker and Settings picks write here in `explicit` mode; other modes save those picks to the active `light-theme` or `dark-theme` side |
| `language` | `auto`\|`en`\|`es` | `auto` | The language of Kettle's own text: Settings, menus (the macOS menu bar's application menu and the Dock menu among them), prompts, notifications and screen-reader names. `auto` follows the operating system's locale; a Spanish locale of any region selects Spanish, anything else English. Read at startup, so a change applies when Kettle restarts; a config reload keeps the running language. Config keys and values, logs, the CLI (its help, listings, completions and default config, byte for byte) and control-protocol JSON stay English. Spanish uses neutral Latin American; per-message review status is tracked in [`es.review.toml`](../crates/kettle-i18n/locales/es.review.toml) |
| `font-family` | string | `JetBrainsMono Nerd Font` | Bundled; falls back to system fonts |
| `font-family-bold` / `-italic` / `-bold-italic` | string | — | Per-style family overrides (fall back to `font-family`) |
| `font-size` | float | `13` macOS; `14` elsewhere | Logical pixels, `5`–`72`. macOS matches its other terminals at 13. Linux, the BSDs and Windows start larger because their terminals size text in points at 96 DPI (GNOME Terminal's 11 pt is about 14.7 px, Windows Terminal's 12 pt is 16 px). With the bundled font a 14 px cell is 8.4×17.5 px, so a maximized 1366 px wide screen fits 159 columns, and a 1280-logical-pixel display fits 149 (139 at 15 px). Reset font size (`Cmd+0`/`Ctrl+0`) returns to this value |
| `cell-width` / `cell-height` | float 0.5–3.0 | `1.0` | Multiplier applied to measured terminal cell width / height. Values are clamped at parse time and reload live with font metric changes |
| `text-renderer` | enum | `grid` | `grid` \| `legacy`. `grid` pins every glyph to its terminal cell (`col × cell_w`) so fallback-font glyphs, emoji, symbols, and ligatures cannot drift away from selection, cursor, or mouse hit testing. `legacy` restores the older continuous layout for diagnosing renderer regressions; grapheme-mode rows still follow their grid-owned cluster positions. Both modes fit emoji presentation to the cell height and width and draw grapheme-mode ZWJ and skin-tone sequences as one glyph or one replacement |
| `builtin-box-drawing` | bool | `true` | Kettle draws box drawing (U+2500–U+257F), block elements (U+2580–U+259F), the Powerline separators U+E0B0–U+E0BF, the triangles ◢◣◤◥ and braille (U+2800–U+28FF) itself, from each cell's pixel rectangle, instead of taking them from the font. Lines are whole device pixels sized from the font's underline thickness (heavy lines twice that), so they join across cells and rows without seams or brighter overlaps; arcs, diagonals and Powerline curves are antialiased; shades are 25/50/75% coverage, blended like text antialiasing; separators fill the full cell height. They take the cell's text colour, including bold, dim, selection and cursor colours. A cell carrying combining marks, or two columns wide, keeps the font. `false` draws every one of them with the font. Reloads live |
| `background` / `foreground` | color | from theme | Hex/`#rgb`/`rgb:`/X11 name |
| `cursor-color` | color | from theme | The block cursor color. `cursor-bg-color` and `cursor_bg_color` are aliases |
| `cursor-fg-color` | color | from theme | The glyph color under a focused block cursor. The block is filled with `cursor-color` and its glyph is recolored to this value |
| `selection-background` / `selection-foreground` | color | from theme | Active fills meet 3:1 against the terminal background: a theme colour short of that becomes a stronger tint of the same hue. A selected cell with a program-set background keeps that fill where it also meets 3:1 there, and gets one worked out against its own background where it does not. Unfocused windows use a neutral grey fill at least 2:1 against the background and at least 1.3:1 quieter than the focused fill. Text and decorations meet 4.5:1 against the fill. In a translucent window, the macOS and Windows default, the ratios are measured against the background as it shows over a light or dark desktop (see [Selection and history controls](#selection-and-history-controls)). A readable user `selection-foreground` takes precedence, then readable cell ink; otherwise Kettle chooses the highest-contrast theme selection foreground, theme foreground, theme background, white or black |
| `palette` | `N=#RRGGBB` | from theme | Repeatable, `N` = 0..15 |
| `search-foreground` / `search-background` | color | from theme | Current search-match + quick-select highlight. The background defaults to the theme's yellow `palette[3]`. Search text follows the selection contrast rule: a readable explicit foreground has priority, then readable cell ink, then the strongest fallback. Quick-select labels default to the theme background |
| `scrollback` | int / `infinite` | `10000` | Line-count history cap; `0`, `infinite` or `unlimited` = effectively unbounded before the byte cap is applied |
| `scrollback-bytes` (`scrollback-byte-limit`, `scrollback-memory`) | bytes with optional `K`/`M`/`G`, `KiB`/`MiB`/`GiB`, or `0` | `64000000` | Per-pane scrollback memory budget (64 MB by default; the 10,000-line cap still applies). Includes the active screen, protects visible rows, and trims oldest history by reducing the effective line cap. `0` disables the byte cap and uses `scrollback` only. The budget is an **estimate over the inline grid**: it does not walk each cell's optional heap storage (combining marks, underline color, hyperlink), because that would mean touching every cell on the PTY reader's path. Those are separately bounded — combining marks are capped per cell, and hyperlink text is shared across the cells of one link — so actual usage exceeds the configured value by a bounded factor rather than an unbounded one |
| `window-padding-x` / `window-padding-y` | float | `12` | Inner padding (px), `0` or more |
| `window-width` / `window-height` | int cells | unset | Initial fresh-window terminal grid size. Width is clamped `[20, 400]`, height `[8, 200]`. Unset, a fresh window opens at a `160x45` baseline (about 164 columns at the default font and padding) fitted to 90 % × 85 % of the monitor. On a 1366 px wide laptop that fit leaves about 154 columns at macOS's 13 px font, past the 144 Claude Code's fullscreen diff panel needs to auto-open (see [TERMINAL-CLIENT-COMPATIBILITY.md](TERMINAL-CLIENT-COMPATIBILITY.md#claude-code-diff-panel)); at the 14 px default elsewhere it leaves 143, and the panel opens once the window is maximized (159 columns). A 1280×720 or 1280×800 logical display gets 134 columns fresh and 149 maximized. If only one dimension is set, the other comes from that same `160x45` baseline, and an explicit value is honoured as typed rather than fitted. The font is measured before the window opens, so a set dimension opens at exactly that many cells. An unset dimension keeps the `160x45` baseline's pixel size, converted in logical pixels with an 8×16 px cell at macOS's 13 px font, scaled to about 8.6×17.2 px for the 14 px default elsewhere, so a HiDPI display gets the same grid and the default window (1304×744 px on macOS and 1403×800 px elsewhere, plus any titlebar band) holds about 164x44 cells at the default font and padding. Applied only as the startup seed; restored session geometry and explicit new-window geometry take precedence. For an ~800×600 px window, set `window-width = 98` / `window-height = 35` on macOS, or `92` / `33` at the 14 px default elsewhere (798×602 px before any titlebar band) |
| `window-position-x` / `window-position-y` | int px | unset | Initial fresh-window position in physical pixels. Negative coordinates are valid for monitors left/above the primary display. Applied only as the startup seed; restored sessions and explicit new-window placement take precedence |
| `background-opacity` | float | `0.86` macOS/Windows; `0.99` elsewhere | 0..1. Solid backgrounds request an alpha surface below `1.0`; transparent backgrounds use `background-opacity × background-darkness`; image backgrounds always allow decoded image alpha; starfield stays opaque because its shader covers the surface. macOS and Windows default to visible native material; other targets stay at 99% whether compositor blur is available or not. A lower value applies with or without blur, so GNOME and X11 show the transparency you set. Set this to `1.0` and `window-blur = false` for a fully opaque window. On macOS, the titlebar tab strip is opaque theme chrome; `macos-titlebar-style = native` keeps AppKit's opaque caption following the selected light or dark appearance |
| `window-blur` | bool | `true` | Ask the window system to blur content behind Kettle. macOS keeps material behind the content and an opaque theme-matched titlebar band, follows the selected light or dark appearance, and becomes opaque when Reduce Transparency is enabled. Windows requests the system backdrop and palette-matches its DWM caption; without that backdrop (Windows 10, for one) the window keeps the configured opacity without blur. Other targets default to 99% opacity. Linux enables blur only when the active Wayland compositor advertises KWin's blur protocol; X11 (KDE Plasma's included) and other Wayland sessions, GNOME's included, keep the configured opacity without blur. Wherever blur is unavailable, Settings says so under the switch. A newly opened window is required when changing from an opaque startup surface |
| `cursor-style` | `block`\|`underline`\|`bar` (`beam`) | `block` | `beam` is an alias for `bar` |
| `cursor-unfocused-style` | `hollow`\|`hidden`\|`unchanged` | `hollow` | Steady cursor in unfocused windows and split panes: hollow block, hidden, or the terminal-selected shape. Respects DEC cursor hiding and cursor colour. Applies live |
| `cursor-style-blink` (`cursor-blink`, `cursor_blink`) | bool | `true` | Cursor blinks while the window is focused. The short alias `cursor-blink` is the spelling the right-click Preferences submenu writes back |
| `bell` | `off`\|`visual`\|`attention`\|`both` | `both` | Visual flash and/or window-attention (taskbar/dock urgency) on `BEL` |
| `bell-flash-intensity` (`bell_flash_intensity`) | float 0–1 | `0.03` | Peak strength of the visual-bell flash as a step of CIE L\* lightness (a fraction of the 0–100 scale): `0.03` lifts a dark theme's background by 3 L\* and dips a light theme's by 3 L\* on the frame the bell rings, then eases out to nothing over 300 ms. Only the pane that rang flashes, under its text. Defined perceptually rather than as an alpha because the renderer blends in linear light, where one alpha reads very differently per theme (a `0.10` alpha would be a +22 L\* jump on TokyoNight). The most frequent bell in practice is an empty Tab completion, which does not warrant more than a barely-there cue; raise this (`0.06`–`0.10`) for a stronger flash, `1` paints the foreground solid, or set `0` to drop the flash while keeping window attention and the tab/titlebar bell marks. Full-pane flashes are also the part of a terminal most likely to affect a photosensitive user |
| `osc52-notify` | bool | `true` | Show a dismissible notice after a successful OSC 52 write, at most once every five seconds per pane. Counts Unicode code points after sanitising internal markers and applying the size limit. `false` silences successful-write notices; denied reads and writes each still show one policy notice per pane. See [Pasting](../README.md#pasting) |
| `osc52` (`clipboard`) | `off`\|`copy`\|`paste`\|`both` | `copy` | OSC 52 clipboard policy. `copy` allows programs to set the clipboard but **not** read it (a remote read is a clipboard-exfiltration risk); `paste`/`both` enable read. Target `c` uses the regular clipboard; target `p`/`s` uses Linux PRIMARY without cross-target fallback (platforms without a separate selection use their one clipboard). DA1 advertises clipboard extension `52` only when writes are enabled and the platform clipboard is available; live reload updates that advertisement for existing panes |
| `macos-option-as-alt` | `none`\|`left`\|`right`\|`both` | `none` | Selects which macOS Option key behaves as terminal Alt **for keys that produce text**. `none` preserves normal macOS composition on both sides, so Option-produced symbols and accented characters reach the PTY without a Meta/ESC prefix. Keys that compose no character — Return, Backspace, Delete, the arrows, Home/End, Page Up/Down, Insert and the F-keys — always carry `Alt` to the PTY, on every setting and from either side, because there is no composition for the policy to protect: `⌥⌫` is `ESC DEL` (readline's `backward-kill-word`), `⌥←`/`⌥→` are word-wise motions, and `⌥↩` is Alt+Return, the newline chord in Codex, Claude Code and zsh's emacs keymap. kitty draws the same line. A selected side uses the unmodified key character and keeps `Alt` for keybinds, legacy xterm encoding, Kitty keyboard encoding, and modifier parameters. Ctrl+Option and Cmd+Option chords keep `Alt` in every mode, matching macOS's suppression of Option composition for those chords — they keep it for keybind matching and Kitty encoding; a Cmd-bearing chord has no legacy PTY encoding and is not written at all (see [`TERMINAL-CLIENT-COMPATIBILITY.md`](TERMINAL-CLIENT-COMPATIBILITY.md)). Applies to existing windows on live reload. The key remains parseable but is reported inert on non-macOS platforms so one shared config works everywhere |
| `modify-other-keys` (`modify_other_keys`) | `auto`\|`always`\|`off` | `auto` | Controls only Kettle's modified-Enter fallback before an application queries or sets a keyboard protocol. `auto` recognizes Codex, Claude Code, Gemini, and OpenCode. On Unix/macOS it requires noncanonical input and a fresh foreground process-group match to either the direct launch identity or the background process snapshot. On Windows it requires a running command with one unambiguous shell-child branch containing a recognized composer, or a direct composer launch; helper forks below that composer are supported. Nested shells, readline/libedit programs, SSH/WSL transports, wrappers, and snapshots without a recognized composer receive plain Enter, preventing the literal `;2;13~` suffix an unsolicited xterm sequence can leave behind. Use `always` for an unrecognized or unobservable client, including one inside SSH/WSL; legacy `enter` is its alias. `off` removes the fallback. All modes still honor application-selected xterm levels and Kitty CSI-u, which take precedence. Reloads live, and GUI, control-plane `send_keys`, and broadcast input evaluate the policy separately for each target pane |
| `keybind-yield` (`keybind_yield`) | `auto`\|`off` | `auto` | Which default chords a program in the pane gets while it owns the keyboard: its screen is the alternate screen or has mouse reporting on, or it negotiated the kitty keyboard protocol or xterm `modifyOtherKeys` while shell integration does not show the shell at its prompt (a fish prompt's own kitty flags do not count). `auto` gives such a program `Shift+Arrow` (split resize) always, so Codex's `Shift+Left` (answer a queued question) and Neovim's `Shift+Arrow` motions work. It also gives it jump to prompt (`Ctrl+Up/Down`, `Cmd+Up/Down` on macOS), scrolling (`Shift+PageUp/PageDown`, `Ctrl+Shift+Up/Down`, `Ctrl+Home/End`), `Shift+Home/End`, tab switching (`Ctrl+PageUp/PageDown`, `Ctrl+Shift+PageUp/PageDown`, `Alt+1`-`9`, and `Cmd+1`-`9` on macOS), but only when Kettle's action would do nothing (no prompt to jump to, nothing to scroll, one tab, no selection to extend) and the view is not scrolled into history. Pane focus (`Ctrl+Shift+N/P`, and `Cmd+Opt+Arrow`/`Ctrl+Cmd+Arrow` on macOS) goes to it the same way when focus has nowhere to go (a zoomed split, one pane, or no pane on that side) and its keyboard protocol sends the chord as itself. Copy, every other chord, and every chord while broadcast input is on stay Kettle's, as does a chord you bind yourself, even to its default action. `off` keeps every default chord Kettle's, as in 4.8. See [Keys a program shares with Kettle](TERMINAL-CLIENT-COMPATIBILITY.md#keys-a-program-shares-with-kettle) |
| `paste-images` (`paste-image`) | bool | `on` | Turn a clipboard bitmap into an owner-only temporary PNG and paste its quoted path. Kettle keeps at most 64 files and 256 MiB of final PNG data per process, rejects source buffers above 256 MiB, removes partial writes, and deletes successful files at exit. `off` disables bitmap materialization without changing ordinary file paste. Linux bitmap paste requires X11/XWayland; native Wayland text paste works without it. See [Architecture](ARCHITECTURE.md#private-clipboard-images-and-video-receipts) for the descriptor, identity, and crash-cleanup model |
| `paste-image-preview` (`paste_image_preview`) | bool | `on` | After the initiating pane accepts its own Kettle-created bitmap path, show a pane-local thumbnail receipt. It expands for four seconds, then contracts until its 30-second lifetime ends. Hover pauses that timer, but a two-minute hard limit always removes the receipt. The newest media paste replaces the previous receipt, and later keyboard, paste, or control input dismisses it because the command line may have changed. The card reports the image dimensions and never claims the client attached or opened it. Click the body to preview the retained PNG in Kettle or `×` to dismiss it. The preview follows `media-open-mode` and leaves the pasted command line unchanged. Remote panes warn that the path is local. `off` avoids creating or retaining preview pixels without disabling bitmap-to-path paste. Arbitrary paths are never previewed |
| `paste-files` | bool | `on` | Paste an explicit clipboard file list as shell-quoted paths. WSL panes receive translated `/mnt/…` paths. `off` makes copied files a no-op; drag and drop remains available. Linux clipboard file lists require X11/XWayland; native Wayland uses text only |
| `paste-video-preview` (`paste_video_preview`) | bool | `on` | Show the same pane-local receipt for an explicitly copied or dropped video path after that pane accepts the paste. The first video gets a bounded poster; a batch reports its count. On macOS and Linux the poster comes from the sandboxed media worker, through the decoders a shown video uses (AVFoundation on macOS, then your ffmpeg); on Linux, a video no decoder can read falls back to its integrity-checked Freedesktop thumbnail-cache entry, which the worker reads. Windows uses the Shell thumbnail API. Without a poster the receipt keeps its generic one. Kettle itself never decodes video, scans terminal text, or opens a video from the card; clicking its body or `×` only dismisses the receipt. Two background threads feed an eight-job queue. A hidden check validates the file with a two-second deadline and one fresh-child retry after a deadline failure (on macOS and Linux it decodes nothing; on Windows it also asks the Shell for the poster); if a child cannot be reaped, that queue thread retires. One 20-second deadline from the paste bounds the checks and the poster, and pending receipt state expires two seconds after it. Preview eligibility requires a trusted non-link regular file and parent chain. Sources that fail the platform trust checks keep normal path paste but get no receipt; on Unix this includes group-writable and hard-linked files. The check holds the source open and revalidates it before it answers (on Windows, around the Shell call); on macOS and Linux a poster is kept only when a second check finds the same file with the same sampled contents. A video is decoded only inside the worker's sandbox; a cached thumbnail is an image, so like any image it renders in the bounded worker even where the sandbox cannot be applied. `off` leaves path paste unchanged and skips poster work |
| `record` | `off`\|`on` | `off` | Arm the session recorder at launch, writing an asciicast trace of the window session. **Off by default.** Recording captures on-screen output verbatim — review a trace before sharing it. `--record PATH` / `--record-dir DIR` / `KETTLE_RECORD*` override this for a single launch. The window title carries `[REC]` while active. See [RECORDING.md](RECORDING.md) |
| `record-dir` | path | `<config-dir>/recordings` | Directory that `record = on` writes per-session traces into (collision-safe `kettle-session-*.cast` files; mode `0600` and directory mode `0700` on Unix, or a protected current-user file DACL on Windows). Ignored when recording is off or an explicit `--record PATH` is given |
| `record-raw-input` | bool | `off` | Capture RAW typed characters (**including passwords**) instead of redacted key tokens while recording. A separate, explicit opt-in from `record`; off by default. The title shows `[REC RAW]` while active |
| `record-max-bytes` | bytes with optional `K`/`M`/`G`, `KiB`/`MiB`/`GiB` | `536870912` (512 MiB) | Largest single cast; the session stops at an event boundary before it and the title shows `[REC LIMIT]`. Minimum `1024`; anything lower (zero included) is rejected rather than read as "unlimited", because a cast that cannot hold its own header would stop the recorder from ever starting |
| `record-max-files` | positive integer | `50` | Kettle-owned casts kept in the managed recording directory; oldest unlocked pruned first |
| `record-max-directory-bytes` | bytes with optional `K`/`M`/`G`, `KiB`/`MiB`/`GiB` | `5368709120` (5 GiB) | Total budget for Kettle-owned casts in that directory, pruned oldest-first. Minimum `1048576` (1 MiB): retention deletes to fit, so a bare `500` meaning 500 MB would wipe the directory, and is rejected instead |
| `tab-bar` | `off`\|`auto`\|`always` | `auto` | When the tab bar is shown (`auto` = only with >1 tab, except the macOS and Wayland client titlebar strips which always show a title) |
| `macos-titlebar-style` | `tabs`\|`native` | `tabs` | macOS only; ignored elsewhere. `tabs` puts a top tab strip in a transparent native titlebar with a full-size content view and hidden native title. The strip follows the titlebar height (AppKit measurement, at least 28 pt), clears the traffic lights (measured, or 78 pt), and centres a single tab as the window title. It stays visible with `tab-bar = auto`, including fullscreen. Empty strip space drags the window; double-click follows macOS's zoom/minimize/do-nothing preference (zoom when unset). `tab-bar = off`, a position other than `top`, or `borderless = true` retains the previous native/borderless behavior. Config reload updates decorated windows; `native` restores the separate AppKit caption |
| `linux-titlebar-style` | `tabs`\|`native` | `tabs` | Linux Wayland only. On desktops whose compositor offers no server-side decorations (GNOME and other Mutter-based desktops, read from `XDG_CURRENT_DESKTOP`), Kettle draws a 46 logical pixel header with a single centred title or multiple tabs, followed by the shell dropdown, new-tab button and minimize, maximize/restore, close buttons. Other Wayland desktops (KDE, wlroots compositors) keep the frame winit negotiates with the compositor, and X11 keeps the window manager frame. `native` uses winit's existing titleless Adwaita CSD. As on macOS, `tab-bar = off`, a position other than `top`, or `borderless = true` retains native/borderless behavior. Reload updates existing windows. Empty header space drags; double-click within 400 ms toggles maximize; right-click opens the compositor window menu; middle-click does nothing. A 6 logical pixel inner edge band resizes with directional cursors; maximized and fullscreen windows have no resize band. The header has square outer corners. GNOME's `org.gnome.desktop.wm.preferences button-layout` is read through the existing desktop Settings portal, including left/right placement and omitted buttons; when unavailable or malformed, the default right-side minimize, maximize/restore, close order is used. The strip follows the current terminal-derived chrome colors; 28-point controls share the New Tab hover/pressed fills. Unfocused titles and control glyphs dim with at least 3:1 contrast (4.5:1 when focused) |
| `tab-bar-position` (`tab-position`) | `top`\|`bottom`\|`left`\|`right`\|`hidden` | `top` | Where the tab bar sits. `left`/`right` render a **vertical** tab strip (its width is `tab-bar-width`); when its tabs don't fit the window's height it scrolls with `⌃ ⌄` buttons and the mouse wheel, keeping the active tab in view. `hidden` forces the bar off regardless of `tab-bar` |
| `tab-bar-width` | float 40–600 px | `180` | Width of the vertical tab strip when `tab-bar-position = left`/`right`. Clamped `[40, 600]`; ignored for `top`/`bottom` bars |
| `tab-min-width` | float 40–600 px | `120` | Minimum width of a horizontal tab segment. Outside a titlebar, tabs divide and fill the bar evenly. Titlebar tabs cap their width to leave empty space for native window dragging. Once tabs would shrink below this minimum, the bar overflows and (with `scroll-tabbar`) scrolls. Clamped `[40, 600]` |
| `scroll-tabbar` | bool | `true` | When horizontal tabs would shrink below `tab-min-width`, keep them at that width and scroll the bar with `‹ ›` arrow buttons + the mouse wheel (active tab kept in view). `false` lets them keep shrinking to fit instead. A side bar's rows can't shrink, so it always scrolls |
| `unfocused-split-opacity` | float 0.1–1 | `0.7` | Dim level of unfocused split panes |
| `scroll-multiplier` (`mouse-scroll-multiplier`) | float 0.1–50 | `1.0` | Mouse-wheel scroll-speed multiplier (1.0 ≈ 3 lines/notch). Applied with **sub-notch precision**: precision touchpads and high-resolution wheels report a fraction of a notch per event, and the remainder is carried across events instead of being rounded away, so slow gestures scroll smoothly and proportionally. Small multipliers stay usable for the same reason — at `0.1` a notch is 0.3 lines, which accumulates over three notches rather than vanishing |
| `disable-mousewheel-zoom` | bool | `false` | When `true`, Ctrl+wheel does NOT change the font size. Useful for users who accidentally scroll-zoom on a laptop touchpad. The keyboard IncreaseFontSize / DecreaseFontSize / ResetFontSize chords still work |
| `smart-copy` | bool | `true` | `true` preserves the existing clipboard when there is no selection. `false` replaces it with empty text on every Ctrl+Shift+C. This is separate from `copy-on-select`, which controls automatic copying after selection |
| `menu-item` (repeatable, `menu-item = LABEL = CMD`) | string | none | Add a row to the right-click context menu that writes `CMD\n` to the focused pane's PTY when clicked. Repeatable — each `menu-item = …` line appends one row. Use `menu-item = Clear screen = clear`, `menu-item = Open editor = $EDITOR ~/.bashrc`, etc. For richer behavior (Lua callbacks, conditional rows) use `kettle.add_menu_item(label, callback)` from `init.lua` — see [`docs/examples/init.lua`](examples/init.lua) |
| `handle-size` | int -1–50 px | `-1` | Split-divider stroke width. `-1` = use the theme default (1 px). Higher values give a chunkier divider — useful on high-DPI displays where 1 px is hard to see |
| `geometry-hinting` | bool | `false` | When `true`, request an approximate 8×16 logical-pixel resize increment. This is not recomputed from the active font metrics, so it does not guarantee integral terminal rows/columns and does not track font zoom. X11 honors the hint; Wayland support varies by compositor; macOS ignores it, and Kettle does not currently apply a native Windows increment |
| `focus` | `click`\|`sloppy`\|`system` | `click` | Focus-follows-mouse policy. `click` (default) — focus on click. `sloppy` — focus on cursor movement; pane under the cursor becomes focused without clicking. `system` — kettle treats this as `click` (winit doesn't expose the OS-level focus policy, so the OS-managed mode falls back to explicit-click behavior) |
| `minimum-contrast` | float 0–21 | `0.0` | WCAG 2.0 minimum contrast ratio of cell text against its background; `0` = off. `4.5` ≈ WCAG AA, `7.0` ≈ AAA. Foreground is lifted toward white/black as needed |
| `window-title-format` (`title-format`) | string | `{title} — kettle` | OS title template: `{title}` uses the pane title, cwd basename, or `kettle`; `{cwd}` abbreviates home; `{tab}` is 1-based. `{{`/`}}` escape braces. Native titlebars omit Private Use/Nerd Font glyphs; tabs and pane titles retain them |
| `tab-format` (`tab-title-format`) | string | `{title}` | Per-tab label template — placeholders `{n}` (1-based tab index), `{title}` (focused pane title). Horizontal tabs show a close button on the active or hovered tab and, on macOS, a Cmd+1 through Cmd+9 hint when space permits |
| `scrollbar` | `never`\|`auto`\|`always` | `auto` | Per-pane overlay scrollbar. `auto` shows it whenever the pane has scrollback history — dim at rest, brighter while scrolled back or while the pointer hovers/drags it; `always` also draws the empty gutter with no history. Click or drag the thumb to scroll |
| `scrollbar-width` | float 2–40 logical px | `6` | Visible overlay-thumb width. Kettle scales it for display DPI and keeps a separate invisible hit strip of at least 12 logical px. Dragging preserves the pointer's position inside the thumb |
| `split-divider-color` | color | theme `palette[8]` | Pane border/divider color for *inactive* panes |
| `focused-split-color` (`split-divider-color-focused`) | color | theme `palette[4]` | Border color for the *focused* pane — the "here am I" accent. Pane borders are drawn only when a tab has more than one pane, or while broadcast input is on. While **broadcast mode** is on (`Ctrl+Cmd+B` on macOS, `Ctrl+Shift+G` on Windows, or `Super+G` elsewhere), this is temporarily overridden by theme `palette[3]` (yellow) to signal the active state; the configured color is restored when broadcast turns off |
| `cursor-blink-interval` | int ms | `530` | Cursor blink half-period |
| `cursor-blink-timeout` | int s | `10` | Stop blinking after this many seconds without typing, leaving the cursor visible. Typing, pasting, refocusing, or changing a setting restarts the blink; output does not, as in kitty and Alacritty. `0` blinks for as long as the window is focused. Clamped to `0`-`3600`. The default matches GTK's cursor blink timeout, which Terminator follows. An idle window that stops drawing lets macOS release the GPU memory it holds while drawing. On macOS an idle window stops drawing its blink sooner, at an on-to-off edge after one quiet half-period (see `macos-cursor-blink-layer`), and the timeout still stops the blink on its visible phase |
| `macos-cursor-blink-layer` | bool | `true` | macOS only. Once a focused window has drawn nothing but its cursor blink for one half-period, the window server blinks the cursor in a Core Animation layer: Kettle presents no frame and does not wake until something else changes, such as typing, output, focus, a resize or a setting, which returns the blink to Kettle. macOS then releases the GPU driver memory a drawing window holds. Each blink looks the same either way. An inverted glyph that overhangs a block cursor in a translucent window, and any failure to set up the layer, keep the redrawn blink. `false` redraws the window at every blink. Applies on live reload. The key is parsed but reported inert on other platforms, so one shared config works everywhere |
| `tab-silence-threshold-ms` (`tab-silence-threshold`) | int ms | `10000` | An inactive tab whose unseen output went quiet for this long transitions from the filled `Output` dot to the `Silent` ring (on a top or bottom tab bar; vertical tabs show a dim dot until 6.1). Clamped `[1000, 600_000]` |
| `command-notify-threshold-ms` (`command-notify-threshold`) | int ms | `5000` | Minimum command duration before Kettle fires a desktop notification when an OSC 133 D (CommandEnd) event arrives **while the window is unfocused**. `0` disables. Requires shell integration because the shell emits the event. Clamped `[0, 86_400_000]` |
| `copy-on-select` | bool | `true` | Auto-copy the selection to the clipboard on release |
| `copy-trim-whitespace` | bool | `true` | Drop trailing whitespace at the end of each copied line, whether the program printed it or it is cell padding. Spaces inside a line are kept. `false` copies the selection exactly as displayed, including rectangular block widths. Applies to manual copy, copy on selection and vi-mode yank; live in Settings |
| `update-policy` | `off`\|`notify`\|`auto` | `auto` | Stable-channel behavior after the first-launch privacy skip: no automatic request, a brief top-center update toast, or an authenticated background install used on the next launch. **`auto` is the default** (kettle keeps itself current, oh-my-zsh style); set `off` to opt out. Official installer ownership is required for installation. See [UPDATES.md](UPDATES.md) |
| `update-check-interval-hours` | int hours | `24` | How often the background check may contact the release feed. Default 24 (daily); floored at 1. `update-policy = off` disables checking regardless |
| `update-check` (`check-for-updates`) | bool | compatibility alias | Legacy setting mapped to `notify` (`true`) or `off` (`false`). `update-policy` wins regardless of line order. `kettle --check-update` always performs a one-shot check |
| `restore-session` (`restore_session`) | `auto`\|`true`\|`false` | `auto` | Reopen the previous session (windows, tabs, splits, working dirs). On macOS, `auto` follows the system setting "Close windows when quitting an application": restore when unchecked. Kettle reads `NSQuitAlwaysKeepsWindows` in the app domain first, then the global domain; an absent value means keep windows. On Linux, `auto` restores, as GNOME's Ptyxis does by default; on Windows it means `false`. `true` always enables restore; `false` opens fresh. Legacy boolean spellings (`on`/`off`, `yes`/`no`, etc.) still work. The session is saved only in restore mode (or with `--restore`), so a fresh window never clobbers a saved layout. `--restore` enables restore for one launch; `--layout NAME` restores a named workspace independently. Recent primary-screen output also survives under the bounded private output settings below; layout recovery prompts and restore limits still apply |
| `restore-scrollback-lines` (`restore_scrollback_lines`) | int 0–10000 | `1000` | Recent primary-screen and scrollback rows saved per pane in restore mode, bounded to 8 MiB across the session. `0` disables output persistence and removes the old content file. Each row is capped at 16 KiB of UTF-8 text and SGR. Oversized numeric values clamp to 10000 and produce a config diagnostic. See [SESSION-RESTORE.md](SESSION-RESTORE.md) |
| `restore-scrollback-remote` (`restore_scrollback_remote`) | bool | `false` | Include detected remote/SSH/container panes in saved output. Off by default because server output may contain sensitive data that should not be copied to local disk without opting in. Layout and working-directory restore are unchanged |
| `agent-server` | `off`\|`read-only`\|`full` | `off` | The agent control server mode. **Off by default.** When enabled, kettle starts a local-IPC control server that an AI agent / `kettle ctl` / `kettle mcp` can use to read the screen and drive panes (`read-only` reads / lists / subscribes; `full` also sends text + runs commands). Security: local-only — a Unix domain socket (mode `0600`) or a Windows named pipe (current-user DACL); no TCP. `--agent-server <mode>` is the per-launch override. `agent-server` applies at launch; a reload does not change it. See [docs/AGENT.md](AGENT.md) |
| `agent-display` (alias `agent_display`) | bool | `false` | Agent previews: let agents show media in kettle without granting them screen reads or input. On its own it starts the control server for display requests only, which refuse every read (`display_only`) and mutation (`read_only`); `agent-server = full` already includes display. Agents show media with `show` (or `kettle show`), which lands on the media shelf of the pane they run in. Turning it on (here or in Settings → Agents) applies at once, also for open connections; turning it off applies at the next launch. `--agent-display on\|off` is the per-launch override; `--agent-server off` alone also turns display off, and a reload never overrides either flag. See [docs/AGENT.md](AGENT.md) |
| `agent-display-codex` (alias `agent_display_codex`) | bool | `false` | Codex previews: the zsh or fish a new pane starts defines `codex` as the function `kettle agent-setup --print` prints: zsh through a Kettle-owned, read-only `.zshenv` and a borrowed `ZDOTDIR` it puts back, fish through code run after your configuration (`fish -C`). Needs `agent-display`. Your own `codex` function (defined or autoloadable) still wins, and `kettle agent-setup --status` reports it. Aliases are not checked; fish abbreviations do not suppress Kettle's function. Other shells use the printed function; panes already open keep what they started with. See [Wrapping your own claude or codex function](AGENT.md#wrapping-your-own-claude-or-codex-function). |
| `agent-display-agy` (alias `agent_display_agy`) | bool | `false` | agy previews: enables the explicitly installed native display plugin and its visual guidance. Install with `kettle agent-setup --agent agy --install`; start a new agy session afterwards. Explicit artifact fallback requires a signed macOS agy TUI with foreground terminal input. MCP display also supports Linux. Delivery uses the media shelf; native agy hooks cannot print Kettle inline cards. Needs `agent-display`. |
| `add-kettle-to-path` (alias `add_kettle_to_path`) | `auto`\|`off` | `auto` | macOS and Linux: a new pane whose `PATH`, after the configured `env`, has no executable `kettle` gets the running Kettle's folder appended, after every entry, so `kettle show`, `kettle mcp` and `kettle agent-setup` work there; it is never put in front of your commands, and never a translocated copy's folder (run Kettle from `/Applications` first). Only absolute entries are looked in, and an empty `PATH` (which means the working directory) is left as it is. `off` leaves the `PATH` as configured. Panes already open keep theirs. |
| `video-preview-hover` (alias `video_preview_hover`) | bool | `off` | Loop a video's silent preview while the pointer rests on its lane, for at most four seconds, from frames the lane already holds; it never starts decoding. Held back by `reduce-motion`. Applies live. |
| `reduce-motion` (alias `reduce_motion`) | `auto`\|`on`\|`off` | `auto` | Whether Kettle avoids motion it would start on its own; today that is the video hover preview. `auto` follows macOS Reduce Motion or the Linux Settings portal and avoids motion if that preference is unavailable; `on` always avoids it; `off` never. A preview the user starts plays either way. Applies live. |
| `media-open-mode` (alias `media_open_mode`) | `overlay`\|`lane` | `overlay` | How manual previews open. `overlay` centers within 90% of the owning split without resizing its terminal. Esc or a backdrop click closes it; Pin moves the same preview into a lane. Applies to newly opened previews. |
| `preview-lane-side` (alias `preview_lane_side`) | `bottom`\|`right` | `bottom` | Where a pane's preview lane opens: `bottom` keeps the terminal's columns, `right` keeps its rows. A lane takes about 40% of the pane and leaves the terminal at least 20 columns and 5 rows. Applies to lanes opened after it changes. See **Media** under keybind actions. |
| `agent-display-claude-code` (alias `agent_display_claude_code`) | bool | `false` | Claude Code previews: Claude Code started in a new pane gets Kettle's own plugin (the display server and the hook that prints inline cards) through `CLAUDE_CODE_PLUGIN_DIRS`. Needs `agent-display`. Kettle writes the plugin into its own data directory, checks it before every new pane, and offers nothing while Claude Code's managed policy forbids plugins from the environment; it changes none of Claude Code's settings. On Linux, Kettle also offers a `claude` launch function for cards. Your own `claude` function (defined or autoloadable) still wins, and `kettle agent-setup --status` reports it. Aliases are not checked; fish abbreviations do not suppress Kettle's function. See [Wrapping your own claude or codex function](AGENT.md#wrapping-your-own-claude-or-codex-function). Panes already open keep what they started with. See [Kettle's Claude Code plugin](AGENT.md#kettles-claude-code-plugin) |
| `agent-badge` | string | `"[agent] "` | The per-pane titlebar prefix shown while an agent connection has the pane attached. Set to any glyph you like (`agent-badge = 🤖 `); empty disables it |
| `scroll-on-keystroke` (`scroll-on-input`) | bool | `true` | Jump back to the bottom when the user types while scrolled back |
| `scroll-on-output` | bool | `false` | Jump back to the bottom when new output arrives while scrolled back. Off by default so a chatty background job does not interrupt reading old output |
| `mouse-hide-while-typing` (`mouse-hide`) | bool | `true` | Hide the OS mouse cursor while typing and show it again on mouse movement |
| `word-delimiters` (`selection-word-chars`, `semantic-escape-chars`) | string | engine default | Characters that delimit a word for double-click selection. Leave empty for the engine default. For example, `()[]{}` makes `/` part of a word so paths and URLs select as one unit |
| `font-feature` | string | — | OpenType feature(s), repeatable / comma-list. Forms: `liga`, `+calt`, `-liga`, `liga off`, `ss01`, `cv01=2`, `zero 1`. Applied on top of the ligature toggle |
| `command` / `shell` | string | `$SHELL` | Program to launch |
| `ssh-host` | `name=user@host` | — | Repeatable; named target for the `Ctrl+Shift+S` SSH launcher |
| `keybind` | `trigger=action` | built in map | Repeatable |
| `accent-color` | `auto` \| `theme` \| color | **`auto`** | The UI-chrome accent — active-tab strip, focused-pane border, per-pane titlebars, drag ghost, settings/menu highlights. **`auto` (the default) is Peacock behavior, per *window***: each window claims a distinct hue from the theme's accent pool, seeded by the working directory (same project → same starting hue, stable across launches) and live-deduped against every other kettle window — including other kettle processes — so two open windows never share a hue while the pool has a free one. A theme switch keeps each window's pool slot. **`theme`** (also `off`/`none`) opts out: every window uses the theme's signature accent (the default TokyoNight Night's blue `#7aa2f7`, matching the app icon; Catppuccin Mocha instead uses its mauve `#cba6f7`; `palette[4]` for themes without an `accent`). A `#rrggbb`/`#rgb`/`0xRRGGBB`/X11 color pins one color for every window (skips the dedupe). CLI `--accent COLOR` wins over the config. `palette[3]` broadcast yellow and the cursor are not affected by design |
| `status-bar` (`statusbar`) | `off\|top\|bottom` | `off` | Show a thin strip at the configured edge with `HH:MM:SS` in local time, the theme, and the focused pane title. Disabled by default. Aliases: `none` / `false` = off, `on` / `true` = bottom |
| `trigger` | regex \[`:: cmd args`\] | — | Repeatable. Triggers run only while the window is unfocused: a match against any of its panes' output then requests attention. `^` and `$` anchor to each row of output. A 2 second throttle coalesces storms. With ` :: cmd args`, Kettle spawns the argv directly without a shell; `{0}` and `{1}` substitute capture values but cannot add arguments |
| `resize-overlay` (`resize_overlay`) | `always`\|`never`\|`after-first` | `after-first` | Show a centered `cols×rows` chip while resizing. `after-first` skips the initial window placement; `never` disables it |
| `theme-mode` (`theme_mode`) | `explicit`\|`light`\|`dark`\|`auto` (`system`/`follow-system`) | `auto` (or `explicit` when `theme` is set without a mode) | How the active theme is picked. `explicit` uses `theme`; `light`/`dark` force `light-theme`/`dark-theme`; `auto` follows the OS light/dark preference (XDG Settings portal on Linux, native appearance on macOS/Windows); no preference or unavailable detection selects dark. If `theme-schedule` is set, the schedule owns the switch instead |
| `light-theme` / `dark-theme` | string | `Kettle Day` / `TokyoNight Night` | The two themes `theme-mode` switches between. Any bundled theme name (`kettle --list-themes`). Kettle Day is TokyoNight Day with darker text: foreground 7:1 and ANSI colours 1–7 and 9–15 at least 4.5:1 against its background. `TokyoNight Day` itself keeps upstream's colours |
| `theme-schedule` | string | — | Scheduled light/dark switch for `theme-mode = auto`; when present, it takes precedence over OS appearance following. Either two `HH:MM <role>` entries in local time (`role` = `dark`/`light`), comma-separated — e.g. `19:00 dark,07:00 light` — or `auto` (aliases `sunrise/sunset`, `solar`) for sunrise/sunset (needs `theme-schedule-lat`/`-long`) |
| `theme-schedule-lat` / `theme-schedule-long` | float | — | Latitude `[-90, 90]` / longitude `[-180, 180]` for `theme-schedule = auto` sunrise/sunset. Out-of-range values are discarded (the schedule stays unset) |
| `grapheme-clustering` | bool | `true` | Start new terminals with Unicode grapheme cells (mode 2027): joined emoji, skin tones, flags and VS16 sequences share a cluster. Indic spacing marks and conjuncts, plus Thai/Lao spacing vowels, reserve two cells; registered VS15 text sequences narrow default emoji to one. Unattached skin-tone modifiers occupy two cells. `false` keeps scalar cells. Applications can toggle mode 2027; RIS and DECSTR restore the startup default. Reload affects only terminals created afterwards |
| `bidi` | bool | `true` | Show normal-screen lines in Unicode Bidirectional Algorithm display order with a left-to-right paragraph ("implicit" mode, as in VTE and Konsole). Hebrew and Arabic read right to left, brackets inside them are mirrored, and Arabic letters take their joined forms. Each line is laid out on its own, and bidi control characters are ignored. Stored text, copy, search, the PTY, mouse reports and screen readers stay in logical order. Full-screen programs (the alternate screen) keep logical order, which VTE does not. `false` shows every line in logical order. Reloads live |
| `allow-bold` | bool | `true` | When `false`, suppress the SGR bold attribute. Useful with fonts that have no bold face |
| `bold-is-bright` | bool | `false` | When `true`, bold text using a palette 0–7 color is remapped to the bright 8–15 variant (xterm convention) |
| `clear-select-on-copy` | bool | `false` | When `true`, the selection highlight is cleared right after a copy (some users prefer the selection to disappear once captured) |
| `invert-search` | bool | `false` | Flip the search bar's default step direction: `Enter` searches backward and `Shift+Enter` forward. Previous, Next, `Shift+F3`, and `F3` keep their literal directions |
| `search-wrap` | bool | `true` | Wrap from the last match to the first and back. When `false`, navigation stops at the buffer ends |
| `vim-menu-nav` | bool | `true` | Vim-style navigation in kettle's menus/overlays. List overlays (context menu, new-tab dropdown, settings) take `j`/`k` (wrapping), `g`/`G`, `Ctrl+d`/`Ctrl+u` (half page); in the context menu / new-tab dropdown `h` = back/close and `l` = drill in/activate, while in the settings panel `h`/`l` step the highlighted row's value (same as `←`/`→`); confirm dialogs take `y`/`n`; text-input overlays with a selection (palette, search, layout picker) use `Ctrl+j`/`Ctrl+k` (or `Ctrl+n`/`Ctrl+p`) so letters keep typing. Menu mnemonics skip the nav letters while enabled. Text-input overlays keep normal typing. `false` restores plain arrow-key navigation |
| `backspace-binding` | `ascii-del`\|`control-h`\|`escape-sequence`\|`auto` | `ascii-del` | Startup byte(s) the Backspace key sends; an application can override them with DEC mode 67 until terminal reset. `ascii-del` = `0x7f` (the modern default); `control-h` = `0x08` for hosts expecting the old binding |
| `delete-binding` | `ascii-del`\|`control-h`\|`escape-sequence`\|`auto` | `escape-sequence` | Byte(s) the Delete key sends. `escape-sequence` = the standard `CSI 3~` |
| `login-shell` | bool | `false` | Launch the shell a `command` names as a login shell (`-l`). On macOS and Linux your default shell already starts as a login shell, with `-` and its name as `argv[0]`, so it needs no flag; on Windows the default shells (pwsh, powershell, cmd) take no `-l`, so `login-shell` leaves them alone. Ignored for `wsl.exe` (where `-l` means "list distros" — see the WSL note below) |
| `shell-integration` (alias `shell_integration`) | `auto`\|`off` | `auto` | New macOS/Linux panes automatically load integration for the default bash, zsh or fish, or a `command` naming just that shell. User startup runs once before Kettle's hooks; each bootstrap clears its borrowed environment when it runs. Bash uses `--posix`/`ENV` while preserving login state; macOS system Bash 3.2 is launched untouched and needs manual hooks. Zsh uses a private `ZDOTDIR`; fish removes its injected `XDG_DATA_DIRS` entry while preserving user edits. Bash startup-only flags also receive hooks. Scripts, other arguments, SSH, sudo and other wrappers are left alone. `off` disables automatic hooks; legacy `on`/`true` and `false` still work. Manual snippets still work. See [SHELL-INTEGRATION.md](SHELL-INTEGRATION.md). |
| `completion-overlay` | `auto`\|`off` | `auto` | Present shell-owned candidates in a detached card aligned with the command's first editable column and never at the cursor. Kettle prefers the lane above the command and flips the card below its final wrapped row when that shows more results. The card stays inside the active pane's terminal grid and never covers the command, pane title, or tab bar. If neither lane fits or the pane is narrower than 20 columns, no card is drawn. Wrapped input falls back to the first grid column. `auto` replaces only stock Fish or PowerShell bindings, never custom bindings, and stays off inside tmux/screen. Bash and Zsh provide cooperative helpers. The shell still owns matching and edits. Changes apply to new shells so an existing wrapper never becomes invisible mid-session |
| `term` | string | `xterm-256color` | The `TERM` value exported to the PTY |
| `colorterm` | string | `truecolor` | The `COLORTERM` value exported to the PTY (advertises 24-bit color) |
| `env` | `KEY=VALUE` | — | Repeatable; exports user environment variables to every spawned pane. Names must use portable env syntax (`[A-Za-z_][A-Za-z0-9_]*`), empty values are allowed, and later duplicates win. Kettle still owns terminal identity vars through `term`, `colorterm`, `TERM_PROGRAM`, and `TERM_PROGRAM_VERSION`, and sets `KETTLE_PANE_ID` and `KETTLE_PID` after these entries so they cannot be overridden; user vars are also appended to `WSLENV` for Windows → WSL launches |

SSH pane titles show the destination after skipping option operands, including
`-p`, `-J`, and `-i` (for example, `ssh -p 2222 alice@server` shows `ssh alice@server`).

### Scrollback search

`Ctrl+Shift+F` opens a responsive search bar at the bottom of the
window. It shows the current match position and exact total beside the query (for example, `3/122`), **No matches** when empty, and **Counting…** while its bounded count scan is incomplete. Its controls are Editor, Previous, Next, Wrap, Case, Regex, Invert, and Close;
`search-wrap`, `search-case-sensitive`, and `invert-search` are also available
in **Settings → Search**. Changes made from either surface persist to the config.

Patterns use Rust regex syntax through `regex-automata`'s meta engine and are
limited to **4096 UTF-8 bytes**. A pattern that **fails to parse** is retried as
a literal, so searching for `call(x` or a bare `(` finds that text on screen
instead of reporting an error. Regex is on by default; click **Regex** or press
`Alt+R` on Linux and Windows, `Cmd+Alt+R` on macOS
(`toggle_search_regex`, rebindable) to switch to literal text. Hover or focus the
Regex control to see its currently bound shortcut. Windows AltGr composition
continues to type into the query.
This toggle is local to the open window's search and does not change a config key.
In regex mode a pattern that **does** parse keeps regex meaning, so `a|b` and
`^row` work as expected. Literal mode finds punctuation such as `call(x)` and
`arr[0]` exactly. Clicking **Case** cycles Smart, Match and Ignore. Engine construction is bounded to a
**512 KiB NFA**, **256 KiB one-pass state**, **256 KiB hybrid cache**, and
**40 KiB DFA**. A syntactically valid expression that exceeds an applicable
ceiling shows **Pattern too complex**. Kettle asks the engine for its implicit
whole-match capture only (`WhichCaptures::Implicit`); parentheses still group
normally, but subgroup capture values are neither built nor exposed because
search only needs the whole grid span.

Highlights require a consuming match, so zero-width results such as bare `^`,
`$`, or `\b` are suppressed in the engine's single leftmost-first pass. Rust
alternative priority still applies: a nullable alternative that wins with an
empty match can shadow a later consuming alternative at the same position. The
editor supports grapheme-aware selection, caret movement, deletion, Home/End,
platform copy/cut/paste shortcuts, and horizontal scrolling. Control characters
in inserted text are rejected; pasted tabs and newlines normalize to spaces.

Typing scans up to 1000 physical lines around the viewport immediately. If that
bounded pass has no match, a 500 ms idle retry walks the remaining history in
1000-line chunks without blocking the event loop. The 1000-line range is a
navigation bound, not permission for one long synchronous call: one exact core
work slice runs per event-loop turn and yields only between complete hard
logical lines. Its continuation resumes on a later turn without showing
Results limited.

Chunk progress is kept while a PTY keeps producing output. Because rows can
shift during that pass, Kettle verifies a non-navigation scan again from a fresh
viewport anchor once output has been quiet for 500 ms, and only then reports a
definitive boundary or miss. If output interrupts an explicit Previous/Next
operation, that operation stays **Results limited** until the user retries it.
Silently starting a quiet retry in the default direction would verify a
different navigation request.

Visible highlighting scans the viewport plus 100 physical lines on each side.
One regex-engine invocation receives at most **64 KiB of UTF-8**. One aggregate
bounded call has the same **64 KiB** text ceiling and may inspect at most
**262,144 terminal cells** across at most **256 complete logical-line
haystacks**. Reaching an aggregate work ceiling produces the exact resumable
yield described above.

A single soft-wrapped logical haystack is separately bounded to **256 physical
rows**, **64 KiB of UTF-8**, and **262,144 inspected cells**. If that capacity
ends inside the logical line, it is an immediate accuracy barrier: Kettle does
not skip the uninspected cells, returns no continuation beyond them, and reports
**Results limited** instead of claiming a definitive first, last, wrap, or
no-match state. One nearby projection retains at most **65,536 match spans**.
Signed grid coordinates keep negative scrollback rows valid. A separate bounded scan retains at most 65,536 spans for the exact total and
current ordinal. It advances one engine call and at most 512 matches per turn,
after the typing pause, and restarts on output, query changes or reflow. It shows
**Results limited** when a capacity barrier prevents an exact count. Navigation
and visible highlights have independent scanner progress. Search state belongs to the OS window, while
the last query is remembered per pane within that window.

`Enter` follows the configured default direction and `Shift+Enter` reverses it;
`F3` / `Shift+F3` are always Next / Previous, and so are `Cmd+G` / `Shift+Cmd+G`
(`Ctrl+G` / `Ctrl+Shift+G` on Linux). The `start_search` shortcut pressed again
returns to the query and selects it. Clicking a button runs it without taking
focus from the query. A button acts when released over it, so moving off before
releasing cancels, and Space or Enter on a focused toggle or Close acts once
however long it is held. Wrap and Invert keep the current match; Case searches
again from it. `Escape` closes the bar without snapping the viewport away from
the selected result. Search input is Kettle UI chrome and is not sent to tmux,
AstroNvim, Codex CLI, Claude Code CLI, or any other program in the PTY.

The query edits like a native text field: word movement and deletion with
`Option` on macOS or `Ctrl` elsewhere, `Cmd+Left` / `Cmd+Right` and
`Cmd+Backspace` / `Cmd+Delete` for the line ends on macOS, and undo with
`Cmd+Z` / `Shift+Cmd+Z` (`Ctrl+Z` / `Ctrl+Shift+Z` on Linux). Typing, or a
held Backspace, undoes in one step. On macOS, `Ctrl+A` / `E` / `B` / `F` /
`D` / `H` work as in its text fields, and `Ctrl+K` deletes to the end when
`vim-menu-nav` is off; with it on (the default), `Ctrl+J` / `K` / `N` / `P`
step through matches. `Shift+Left` / `Shift+Right` and `Shift+Home` /
`Shift+End` select in the query, so the split resize and select-to-top or
bottom bound to them work once the bar closes or a button has focus.

Kettle's other shortcuts keep working while the bar is open: a new tab, font
zoom, tab switching (with `Alt+1`…`9` on Linux too), split focus, vertical split
resize, scrolling, and the rest. The bar follows focus to another pane.
Shortcuts that type into the terminal do nothing while the bar is open: `text:`
bindings, `send_newline`, `paste_primary`, and the pane-number inserts. Paste
goes into the query, and Copy takes the query's selection if it has one,
otherwise the text selected in the grid. On Linux, `Ctrl+Shift+A` and
`Ctrl+Shift+X` keep their split and zoom meanings rather than selecting or
cutting the query.

### Auto light/dark theme switching

With neither `theme` nor `theme-mode` set, Kettle follows the system appearance
using `Kettle Day` for light and `TokyoNight Night` for dark. No preference
or unavailable detection selects the dark side. Kettle Day is a bundled theme
derived from TokyoNight Day: it keeps that theme's background, selection
background and background-tone slots (0 and 8) but darkens its text colours,
selected text included, along their own hue, so body text reaches 7:1 and the
ANSI colours prompts use reach 4.5:1 (WCAG AA). `TokyoNight Day` and every
other bundled theme keep upstream's colours, and `palette = N=#hex` and
`foreground` lines still override any of them. Existing configs that set
`theme` without `theme-mode` keep their fixed theme, including named `palette`
presets. An explicit mode always wins, regardless of line order.

Settings → Appearance → Appearance offers **Follow system**, **Light**, **Dark**
and **Fixed theme**, mapped to `theme-mode = auto | light | dark | explicit`.
Fixed theme keeps the palette currently on screen. The theme picker header says
which appearance you are choosing; picks in the picker or Settings save to the
active `light-theme` or `dark-theme` while keeping the mode. A preview's own
lightness does not change the destination. Fixed mode saves picks to `theme`.

Linux reads and subscribes to the XDG Settings portal's
`org.freedesktop.appearance color-scheme` off the UI thread, with bounded calls
and retries. macOS material and titlebar appearance match the active palette,
including a custom light palette chosen for the system's dark side. The native
titlebar strip uses the theme background, independently of wallpaper chrome. Live
changes repaint all windows and update terminal colour-scheme reports.

Set `theme-mode = auto` (or `system` / `follow-system`) to enable following in
an existing fixed-theme config, and optionally customize the pair:

```ini
theme-mode  = auto
light-theme = Kettle Day
dark-theme  = TokyoNight Night
```

Add `theme-schedule` when you want kettle to ignore OS appearance changes and
switch by time instead:

```ini
theme-mode     = auto
light-theme    = Kettle Day
dark-theme     = TokyoNight Night
# dark at 19:00, light at 07:00
theme-schedule = 19:00 dark,07:00 light
```

…or compute sunrise/sunset from your location using the current UTC calendar date:

```ini
theme-mode          = auto
light-theme         = Kettle Day
dark-theme          = TokyoNight Night
theme-schedule      = auto
# e.g. Long Beach, CA
theme-schedule-lat  = 33.77
theme-schedule-long = -118.19
```

### Launching WSL / Ubuntu as your shell (Windows)

On Windows you can point kettle at a WSL distribution instead of
PowerShell or `cmd.exe` by setting `command` to the `wsl.exe` launcher:

```ini
# Open your default WSL distro
command = wsl.exe

# Or pick a specific distro
command = wsl.exe -d Ubuntu

# Start in your Linux home directory (not the Windows cwd)
command = wsl.exe -d Ubuntu --cd ~
```

kettle runs `wsl.exe` over ConPTY just like any other shell, so colors,
resizing, UTF-8, and mouse reporting all work. Run `wsl -l -v` in
PowerShell to see your installed distro names.

> **Note:** `login-shell = true` is ignored for `wsl.exe` — passing `-l`
> to `wsl` means "list distributions", which would exit immediately
> instead of opening a shell. To get a Linux *login* shell, ask for it
> inside the distro: `command = wsl.exe -d Ubuntu -- bash -l`.

### Quick terminal

`kettle quick-terminal` toggles a dedicated borderless window in the running
instance over the private activation channel, or starts Kettle directly in that
window. The first toggle creates it; later toggles reuse its tabs, splits and
running programs. Normal windows keep their own workspace. The action
`toggle_quick_terminal` is also in the command palette, the macOS View menu,
and the keybind map, for example `keybind = ctrl+grave=toggle_quick_terminal`.

| Key | Type | Default | Notes |
|---|---|---|---|
| `quick-terminal-position` | `top` \| `bottom` \| `left` \| `right` \| `center` | `top` | Screen edge, or centered on the screen |
| `quick-terminal-size` | integer percent, 1-100 | `40` | Top/bottom use this fraction of screen height and full width; left/right use this fraction of width and full height; center uses this fraction of both dimensions |
| `quick-terminal-hotkey` | modified key chord, or `none` | unset | Examples: `ctrl+grave`, `cmd+escape`. Settings has a key-capture field; bare Delete/Backspace clears it and bare Esc cancels capture |
| `quick-terminal-animation` | bool | `true` | Slide into view in 150 ms. System Reduce Motion or `reduce-motion = on` makes it instant, as does `false` |
| `quick-terminal-autohide` | bool | `true` | Hide when another application gains focus. Switching to another Kettle window keeps it visible |

Each show selects the screen under the pointer (the active screen on macOS),
requests placement above normal windows and focuses the terminal. Toggling it
again hides it and returns focus to the previous app or Kettle window. When you
switch apps yourself, autohide keeps focus in the app you selected. Hidden tabs
and splits keep running; the quick workspace is not saved into normal session
restore. Position and size changes apply on the next show. Hotkey changes
register on reload.

macOS registers global shortcuts through Carbon `RegisterEventHotKey`, without
Accessibility permission. X11 uses passive root-window `XGrabKey` registrations,
including CapsLock and NumLock variants. An unavailable or already-taken chord
shows a translated notice. macOS excludes the window from its Window menu and
window cycling; Windows skips its taskbar entry and Alt-Tab and X11 requests skip-taskbar
and skip-pager hints.

On **Wayland and Windows**, set a desktop shortcut that runs
`kettle quick-terminal`. A configured `quick-terminal-hotkey` logs once that it
is not registered there. Wayland compositors control window placement,
stacking, visibility and activation; screen-edge placement and sliding require
compositor window rules where the compositor does not honor these requests.
Desktop launchers should supply `XDG_ACTIVATION_TOKEN` (for a custom `.desktop`
launcher, set `StartupNotify=true`); Kettle forwards this bounded token to the
running instance to raise and focus the quick window. Without a desktop token,
Wayland activation remains subject to the compositor's focus policy. Hiding
requests minimization on Wayland. No extra global-shortcut service or
Accessibility permission is required.

### Extended keys

Both kebab-case and underscore spellings are accepted for every key in this
table. For example, `show-titlebar` and `show_titlebar` are equivalent.

| Key | Type | Default | Notes |
|---|---|---|---|
| `window-state` | enum | `normal` | Launch state: `normal` \| `maximise` (`maximize`) \| `fullscreen` \| `hidden`. Honored by winit's `with_maximized` / `with_fullscreen` / `set_visible(false)` |
| `window-width` / `window-height` | int cells | unset | Fresh-window startup grid size. Width is clamped `[20, 400]`, height `[8, 200]`; unset, a fresh window opens at a `160x45` baseline fitted to 90 % × 85 % of the monitor (about 164 columns on a 1080p monitor at the default font and padding; on a 1366 px wide laptop, 154 at macOS's 13 px font and 143 at the 14 px default elsewhere), and if one side is omitted the other comes from that baseline. Logical pixels, so HiDPI gets the same grid. Session restore and explicit new-window geometry override this seed |
| `window-position-x` / `window-position-y` | int px | unset | Fresh-window startup position in physical pixels. Negative values support multi-monitor layouts. Session restore and explicit new-window placement override this seed |
| `gpu-power-preference` | enum | `auto` | GPU preference at startup: `auto` (surface/platform preference — **default**) \| `low` (low-power / usually integrated) \| `high` (high-performance / often discrete). Used when no specific GPU is pinned (`gpu-device-id` below). Auto uses deterministic native backend order: DX12 first on Windows, Metal on macOS, Vulkan elsewhere. Explicit `low`/`high` ranks the requested physical GPU class before backend, so a lower-ranked backend cannot make the opposite GPU class win; equal-class ties preserve the platform-preferred adapter. On a dual-GPU laptop `high` may wake the discrete GPU; use it only when you want that render headroom. Also surfaced in Settings → Graphics |
| `gpu-device-id` / `gpu-vendor-id` | hex u32 | `0` / `0` | Pin a *specific* adapter by its PCI device + vendor id (e.g. `0x2191` / `0x10de`). Both ids at `0` plus an empty `gpu-name` means use `gpu-power-preference`. Easiest set via Settings → Graphics → GPU device; detected software adapters with zero PCI ids remain pinnable by `gpu-name`. The resolver falls back to the power-preference policy if the pin is absent (eGPU unplugged, driver swap) so a stale pin never fails startup. **Applies on next launch** |
| `gpu-name` | string | — | Display name of the pinned GPU; also a fallback match if the `(vendor,device)` pair no longer enumerates. Written by the settings picker |
| `gpu-backend` | enum | `auto` | Select the graphics backend independently of a GPU pin: `auto` \| `dx12` \| `vulkan` \| `metal` \| `gl`. `auto` uses DX12 → Vulkan → GL on Windows, Metal first on macOS, and Vulkan → GL elsewhere. An unavailable explicit backend emits a warning and falls back to native order so a portable config still starts. **Applies on next launch** |
| `gpu-force-software` | bool | `false` | Force wgpu's software/fallback adapter (slow; for debugging GPU-driver issues). **Applies on next launch.** You rarely need it for resilience: Kettle auto-recovers from a lost graphics device — a driver TDR/reset, a Remote Desktop console↔session transition, or VRAM exhaustion — by rebuilding the renderer on a backoff. Recovery tries another backend on the same GPU, the surface-preferred GPU, another physical hardware GPU, then software rendering, while panes and their PTYs keep running throughout. When recovery lands on software, the window title shows a "software rendering (GPU unavailable)" notice |
| `borderless` | bool | `false` | Hide OS chrome (`winit::WindowAttributes::with_decorations(false)`). Useful for tiling WMs |
| `always-on-top` | bool | `false` | Keep window above others (`winit::Window::set_window_level(AlwaysOnTop)`) |
| `hide-on-lose-focus` | bool | `false` | Quake-style auto-hide. Wayland defers to compositor; Linux X11 + macOS + Windows hide directly |
| `show-titlebar` | bool | `true` | Per-pane titlebar; renders only when a tab has >1 pane (a single-pane tab uses the OS window title instead). Also the grab handle for dragging a pane elsewhere in its tab — with it off, use `move_split:{up,down,left,right}` |
| `title-at-bottom` | bool | `false` | Per-pane titlebar position |
| `title-hide-sizetext` | bool | `false` | Hide the `WxH` size annotation in the titlebar |
| `icon-bell` | bool | `true` | Render a bell glyph in the titlebar when the pane ringed BEL |
| `title-transmit-bg-color` / `-fg-color` | color | `focused-split-color` → window accent / theme `cursor-text` | Focused-pane (broadcast-source) titlebar colors; unset values follow the active theme cascade |
| `title-receive-bg-color` / `-fg-color` | color | window accent / theme `cursor-text` | Broadcast-group-member titlebar colors; unset values follow the active theme cascade |
| `title-inactive-bg-color` / `-fg-color` | color | theme `palette[8]` / theme foreground | Idle-pane titlebar colors; unset values follow the active theme cascade |
| `background-type` | enum | `solid` | `solid` \| `transparent` \| `image` \| `starfield` (a zero-config procedural GPU starfield that needs no `background-image`; a fixed built-in example whose look is not tunable). Surfaced in Settings → Background. See **[BACKGROUNDS.md](BACKGROUNDS.md)** |
| `background-image` | path | — | Wallpaper image (for `background-type = image`). Supports PNG/JPEG/WebP/BMP/GIF, **animated GIF / APNG / animated WebP** (plays as a moving background — see `background-animation`). Tilde expansion supported. Editable inline in Settings → Background. Curated sources in **[BACKGROUNDS.md](BACKGROUNDS.md)** |
| `chrome-background` | enum | `theme` | When a wallpaper (`image` or `starfield`) is set, the opaque fill of the window chrome strips (tab bar, status bar) so the background never bleeds through them: `theme` (the theme's chrome color — default) \| `auto` (the background's average color, kept readable under the tab text; black over the starfield) \| `black` \| `white`. Without a wallpaper, strips use a slightly darker theme background instead of the ANSI bright-black colour, in every mode |
| `background-animation` | enum | `always` | How an animated background (a `starfield` or an animated `background-image`) plays: `always` (`on`/`true`, the default; animate even when unfocused, but still freeze when the window is minimized/occluded) \| `when-focused` (`focused`, animate only while focused, zero idle otherwise — battery-friendly) \| `off` (`static`/`false`, freeze on first frame). Surfaced in Settings → Background |
| `background-image-mode` | enum | `stretch_and_fill` | `stretch_and_fill` \| `tile` \| `center` \| `scale` (aspect-preserving fit) |
| `background-image-align-horiz` | enum | `center` | `left` \| `center` \| `right` (applies to `center` + `scale` modes) |
| `background-image-align-vert` | enum | `middle` | `top` \| `middle` \| `bottom` |
| `background-blur` | bool | `false` | CPU-side 3-pass separable box blur at decode (approximates Gaussian) |
| `background-darkness` | float 0..1 | `0.5` | Opacity of the terminal background over a transparent, image, or starfield backdrop. `0.0` is fully see-through; `1.0` hides the backdrop. Only applies when `background-type` is not `solid` |
| `inactive-color-offset` / `inactive-bg-color-offset` | float 0..1 | `1.0` / `1.0` | How far an unfocused pane recedes. Both are read with `unfocused-split-opacity`, and the strongest value wins. Kettle composites one overlay rather than reshaping each glyph |
| `exit-behavior` | `close`\|`close-on-success`\|`hold` | `close-on-success` | Close successful processes; retain failed processes and their output with an exit-code banner along the bottom of the pane, under the output. Enter or Restart runs the original command with its launch directory and environment in the same pane. Escape or Close dismisses the pane; the close chord also works. `close` closes every exit; `hold` retains every exit. Reload applies to future exits |
| `exit-action` | enum | unset | Compatibility policy: explicit `close`, `hold`, or `restart` (same argv and current working directory in a new tab). `exit-behavior` takes precedence when both are present. Duplicate exit notifications are coalesced |
| `broadcast-default` | `all`\|`group`\|`off` | `group` | Scope enabled by the broadcast chord. `group` targets the active tab, `all` targets every pane in the window, and `off` prevents the chord from enabling broadcast. Kettle waits for the chord instead of starting a window in an input-mirroring state |
| `split-to-group` | bool | `false` | A new split joins the broadcast group of the pane it came from, so splitting a grouped pane widens the group instead of quietly dropping out of it |
| `autoclean-groups` | bool | `true` | Forget a broadcast group after its last pane closes. This prevents a stale group target from claiming a later pane that reuses the same name |
| `always-split-with-profile` | bool | `false` | Repeat the parent pane's direct launch command in a new split instead of opening a shell. Ordinary shell panes are cloned either way |
| `force-no-bell` | bool | `false` | Silence every bell path regardless of `bell`: visual flash, window attention, and the tab activity mark |
| `visible-bell` / `urgent-bell` | bool / bool | `—` | Legacy aliases for the unified `bell` key. Together they map to `both`; individually they map to `visual` or `attention`. An explicit `bell = …` setting wins regardless of file order |
| `log-strip-ansi` | bool | `false` | Strip ANSI escape sequences from the per-pane session log (`Action::ToggleSessionLog`) before writing. `true` → log is plain-text (CSI / OSC / single-char ESC all stripped); `false` → raw stream is preserved (`cat`-replayable in a terminal) |
| `light-theme` / `dark-theme` | theme name | `Kettle Day` / `TokyoNight Night` | Themes swapped by `Action::ToggleLightDark`. Lookup is case insensitive |
| `search-case-sensitive` | enum | `smart` | `smart` ignores case until the query contains uppercase. `always` / `sensitive` forces matching case; `never` / `insensitive` always ignores it. `case-sensitive = true/false` remains accepted as an alias |
| `link-single-click` | bool | `false` | Single-click opens URLs (default needs `Ctrl`/`Cmd`+click). Hover shows the destination and the current opening gesture; the pill clears on pointer exit, scrolling, typing or focus loss |
| `link-underline` (alias `link_underline`) | `hover`\|`always` | `hover` | When detected URLs and paths are underlined. `hover` draws no line at rest and the solid accent line while the pointer is over the link; `always` also draws a thin, dim line at rest. Explicit OSC 8 links keep their dotted resting line in both modes, since their text need not look like a link. Hover, the destination pill, the opening gesture and quick-select hints are the same in both modes. Applies live |
| `disable-mouse-paste` | bool | `false` | Block middle-click paste |
| `paste-protection` | enum | `auto` | `auto` confirms text containing CR/LF, C0 controls other than TAB, DEL or C1 controls when any writable recipient has not enabled bracketed paste, and text carrying a bracketed-paste end marker (`ESC [ 201 ~`) in any mode. `always` confirms every multi-line paste, including bracketed mode, and the same marker. `off` never confirms. The dialog reports line and control counts with a visible-control preview. It offers Paste, Paste as one line, Paste Without Controls (only when controls exist; removes whole escape sequences and controls while preserving TAB/CR/LF), and Cancel (focused initially). All modes remove embedded ESC bytes in bracketed paste. See [Pasting](../README.md#pasting) |
| `clipboard-paste-protection` | bool | — | Retired legacy key; accepted and ignored. Use `paste-protection` for the current policy |
| `putty-paste-style` | bool | `false` | Right-click pastes instead of opening the context menu. By default it uses the same PRIMARY-first source as middle-click paste on Linux and falls back to the regular clipboard on platforms without PRIMARY |
| `putty-paste-style-source-clipboard` | bool | `false` | When `putty-paste-style = true`, source right-click paste from the regular system clipboard instead of Linux PRIMARY |
| `close-button-on-tab` | bool | `true` | Show a close button on tabs (horizontal: active and hovered tabs only; vertical: every tab) |
| `new-tab-after-current-tab` | bool | `false` | Insert vs append behavior when creating a new tab |
| `detachable-tabs` | bool | `true` | Allow cross-window tab tear-off / re-dock and the `move_tab_to_new_window` / `move_tab_to_window` actions. `false` keeps in-window tab switching and reordering but disables detach |
| `ask-before-closing` | enum | `busy-or-multiple` | Close-confirmation policy: `always`, `busy-or-multiple`, `multiple-terminals`, or `never`. `busy-or-multiple` asks before closing more than one pane, and before closing a single pane (its tab, its window, or the pane itself) while a program other than an idle shell owns its terminal: a job such as `sleep` or an editor started from the shell, or the pane's own program when it was started as something other than an interactive shell (a profile or `-e` command such as `vim`, `ssh` or a script); an idle shell at its prompt closes without asking (not on Windows, where a single pane never asks). `multiple-terminals`, the default before, asks only when more than one pane would close. Applies to the close-window, close-tab, and close-pane **actions** (`Ctrl+Shift+W`, `Ctrl+Shift+Q`, the tab-bar `✕`, middle-clicking a tab); panes sitting idle at an integrated-shell prompt do not count as work to lose. The **titlebar `✕` and `Alt+F4`** are OS window-destroy requests rather than Kettle commands, so a window holding several panes closes at once under every setting except `always`. A window holding one pane asks under `busy-or-multiple` when a program is running in it, as it does for the actions; under `multiple-terminals` and `never` it closes at once. Set `always` if you want them to ask for every window. When one pane would close, the question names the program that owns its terminal (its name only, never its arguments). A pipeline is named after its first command, so `cat f | less` reads `cat`. Shells, programs Kettle cannot read, and Windows keep the general wording. Also on the right-click **Preferences ▸** menu as a radio group |
| `lua-sandbox` | enum | `safe` | Lua plugin trust level. `restricted` blocks `kettle.send_text` and `kettle.exec_action`. `safe` also removes command and file APIs from the Lua standard library, but a plugin can still run a command by typing it into the pane. `trusted` restores the command and file APIs (`os.execute`, `io.open`, `io.popen`, `loadfile`, `dofile`); the `debug` library and native C-module loading stay unavailable at every level because the VM is always mlua's safe state. Read any plugin before enabling it. See [Lua plugins](PLUGINS.md) |

Close confirmations appear in a centered dialog with Cancel focused. Enter
activates the focused button, Space activates on release, and Esc cancels. Tab,
Shift+Tab, Left, or Right moves focus between buttons. Clicking outside the
dialog leaves the question open. A button activates when the pointer is released
inside the button where the press began. In short windows, use the wheel,
Up/Down, Page Up/Page Down, or Home/End to scroll the dialog.

The automatically discovered `<config-dir>/init.lua` uses the same trusted
directory and leaf checks as the default config because even `safe` Lua can type
commands into the shell. A dotfile-manager link remains supported only when the
link itself has trusted ownership and one name, and its resolved target passes
the same checks. `--lua-script FILE` is the explicit-path escape hatch for
project or shared scripts; it retains the 4 MiB bounded read and is a deliberate
trust grant, just as `--config FILE` is for configuration.

In `safe` and `restricted` mode, `require` keeps preloaded modules but searches
only absolute `package.path` templates. Imported files must pass the same
directory and leaf checks and the 4 MiB read cap. Relative templates, including
launch-directory discovery, are ignored; use an absolute module path.

`kettle --gpu-info` honors the GPU settings above, including `--config` and
`--profile`, without opening a window. It reports the requested backend policy,
the active backend, and whether an explicit backend fallback occurred.
Screenshots honor the loaded configuration through the same policy. The CI
offscreen GPU self-test deliberately uses `Config::default()` so a developer's
machine configuration cannot make the repository gate nondeterministic.

When a fatal GPU error occurs, Kettle writes fault-only recovery records under
`%LOCALAPPDATA%\kettle\diagnostics\` on Windows or
`$XDG_CACHE_HOME/kettle/diagnostics/` (normally
`~/.cache/kettle/diagnostics/`) on Unix. The JSONL schema records Kettle and
adapter versions, fault type, recovery escalation, and outcome. It never records
terminal text, commands, environment variables, or working directories. Each
incident is capped at 256 KiB and the newest ten are retained.

### Extended key status

Kettle accepts several legacy keys so older configuration files do not fail on
unknown names. Some already map to current behavior; others remain parsed but
inert so the compatibility boundary is explicit.

#### Effectively wired — kettle's behavior already matches the documented setting

| Key | Why it's a "no-op" but works |
|---|---|
| `sticky` (X11 _NET_WM_STATE_STICKY) | **macOS is fully wired** — `sticky = true` calls `app.rs::set_visible_on_all_spaces`, which sets `NSWindowCollectionBehavior::CanJoinAllSpaces \| Stationary` on the underlying NSWindow, pinning the window to every Space. **X11 / Wayland are not wired** — winit 0.30 exposes no portable "stick to all workspaces" API (the X11 `_NET_WM_STATE_STICKY` hint has no winit binding and Wayland has no equivalent), so on those platforms the key is logged (not silently dropped) and `always-on-top` is the closest available cross-platform substitute |

#### Won't implement

| Key | Rationale |
|---|---|
| `cursor-color-default` | Kettle uses one setting: set `cursor-color` to override it or remove the line to return to the theme |
| `http-proxy` | Parsed but not consumed. Update traffic is process-wide and does not inherit a per-profile terminal proxy |
| `audible-bell` | Kettle ships no audio bell; use `bell = visual`, `attention`, or `both` |

#### Genuine future work — parsed for forward-compat

The remaining keys parse cleanly but are not yet wired. A future implementation
moves a key into the main table without changing its parser arm.

| Key | Intended behavior | Why it is future work |
|---|---|---|
| `extra-styling` | Render bold/italic with styled-font features even when palette lacks variants | Render glyph-attribute change |
| `enabled-plugins` | Terminator plugin-enable list | Kettle loads `<config-dir>/init.lua` instead; use `lua-sandbox` to set its trust level |
| `hide-from-taskbar` | Suppress from OS taskbar | winit Windows-only natively; cross-platform requires per-platform extensions |
| `title-font` / `title-use-system-font` / `use-system-font` / `use-theme-colors` | Per-pane titlebar font + theme-color overrides | Multi-cycle per-pane font system |

### Selection and history controls

The theme's selection colour is used as the active fill when it has at least
3:1 contrast against the terminal background. Otherwise Kettle lightens or
darkens it, keeping its hue and saturation, until it does. When the window
loses focus the fill turns a neutral grey at least 2:1 from the background
and at least 1.3:1 quieter than the focused fill, as a selection in an
inactive macOS window does, so the selection changes colour as well as
strength. A selected cell that a program gave its own background keeps the
same fill where it also has that contrast against the cell's background, so
one selection across a status line or a powerline prompt stays one colour.
Where it does not, as where the program's background matches the fill, that
cell gets a fill worked out the same way against its own background. An
unfocused split's fills are worked out against its dimmed backgrounds. Focus
changes repaint both selection and cursor without changing terminal state.

With `background-opacity` below `1.0` (the macOS and Windows default is
`0.86`) the terminal background is blended with whatever is behind the
window. Kettle cannot see the desktop, so it measures the selection against
the background as it would show over white (for a selection lighter than the
background) or black (for a darker one), the backdrop that leaves the least
contrast. The focused fill strengthens toward 3:1 there, moving away from the
background on the side it already sits, only as far as its text keeps 4.5:1
and stays light on a dark theme or dark on a light one, so over a bright page
it can stop short of 3:1. The unfocused grey aims for 2:1 against the same
backdrop but stays 1.3:1 quieter than the focused fill, so it stops short
too: TokyoNight Night at `0.86` reaches about 2.4:1 focused and 1.85:1
unfocused there. Where the background as shown would pass the selection
itself, as TokyoNight Night's does over white below about `0.65`, and with
nothing behind the background to show (opacity `1.0`, macOS Reduce
Transparency, the starfield or an image wallpaper that covers the window), the
ratios are measured against the background colour itself.

Text, underlines, strikeouts and link decorations on a highlight meet 4.5:1
against its effective fill, including dim, ANSI, indexed and truecolour text.
A user `selection-foreground` has first priority when it meets that ratio,
followed by the cell's own ink. Otherwise Kettle chooses the highest contrast
among the theme selection foreground, theme foreground, theme background, white
and black. An unreadable user colour also falls back. A link underline on a
highlight, hovered or not, keeps its dotted or solid shape but is drawn 1 px
thick in the colour of the text above it, not link blue or the accent.
Non-current search matches use the active selection fill by the same rule:
it is kept where it meets 3:1 against a cell's background and worked out
against that background where it does not. The current match uses its search
fill and gives a readable `search-foreground` first priority, then the same
cell/fallback rule. The optional `minimum-contrast` setting still applies
after highlight resolution.

Copy drops trailing whitespace at the end of each copied line by default.
`copy-trim-whitespace = false` includes every selected cell, trailing spaces
and padding included. Blocks copy one selected column slice per row, separated by
newlines, without joining wrapped rows. Stream selections join soft wraps.
Copy uses a conservative 16 MiB byte budget; a selection that exceeds it
leaves the clipboard unchanged.
See [Selecting text](../README.md#selecting-text) for mouse modifiers and copy
rules, and [Settings](SETTINGS.md) for live cursor and copy controls.

A pane scrolled into history shows a bottom-right **↓ 1,234 lines** pill. Click it
to return that pane to live output. It disappears at the bottom and when a pane
is too small to fit its label. It remains available with `scrollbar = never`.
The scrollbar retains its configured resting width, grows to at least 10 logical
pixels on hover or drag, and has a hit strip at least 12 logical pixels wide.
All sizes scale with display DPI.


## Editing the config from inside kettle (Preferences submenu)

Right-click → **Preferences ▸** changes six common settings at runtime. Its
`Advanced…` row opens the config file with the operating system's default app
for everything else:

| Submenu row | Config key written |
|---|---|
| Scrollbar (radio: always/auto/hidden) | `scrollbar` |
| Cursor blink (✓) | `cursor-blink` |
| Copy on select (✓) | `copy-on-select` |
| Bell (radio: off/visual/attention/both) | `bell` |
| Mouse-hide while typing (✓) | `mouse-hide-while-typing` |
| Confirm close (radio: always/running programs or multiple terminals/multiple terminals/never) | `ask-before-closing` |
| Font size + / − | (live-only; `font-size` not auto-persisted yet) |
| Advanced… | opens the active configuration file (`--config FILE` or `--profile NAME`), falling back to the platform default resolved by `kettle --config-path`, with the default app |

Each click both mutates the running `Config` (the change takes effect
immediately) and atomically rewrites the matching line in the config file via
the `kettle_config::persist_config_toggle` helper. Non-target comments, blank
lines, and key order are preserved; the targeted line is replaced (and
duplicate assignments for that key are collapsed), or appended if it does not
exist. The writer preserves the file's existing LF/CRLF convention, UTF-8 BOM
or UTF-16 encoding, and Unix permissions. A symlinked config is resolved once
and its regular target is replaced, leaving the dotfile-manager link intact.

The complete read, validation, backup, and replacement transaction holds a
per-target advisory lock, which serializes Kettle's own writers. Immediately
before staging, Kettle compares the current bytes with those originally read
and refuses an external change observed by that comparison. An editor that does
not honor the lock can still race after the comparison; portable filesystems do
not provide a content-based compare-and-swap. Files over 1 MiB, non-regular
targets, and an in-app edit that introduces additional config diagnostics are
refused; unrelated diagnostics already present in the hand-edited file do not
block a valid preference change. The first successful in-app edit creates an exact,
encoding-preserving `<resolved-config>.bak` if it does not already exist (the
default path is `~/.config/kettle/config.bak`); later edits never replace that
snapshot.

## Keybind grammar

`trigger` = `+`-joined modifiers and one key. Recognized modifier names:

- `shift`
- `ctrl` / `control` / `ctl` / `primary` — `primary` is GTK's portable
  spelling and appears in Terminator configs. It means Control on the desktops
  Terminator runs on, so kettle reads it as Control everywhere rather than
  moving the binding to a different key on one platform
- `alt` / `opt` / `option`
- `super` / `cmd` / `command` / `win` / `windows` / `meta` / `logo` —
  all aliases for the same Super-key bit, so a chord copied from a
  macOS / Windows / Linux config works without renaming.

Keys: any single printable character is a valid key — letters `a`..`z`,
**digits** `0`..`9` (e.g. `alt+1`..`alt+9` for `goto_tab:N`), and **punctuation**
(e.g. `ctrl+,` for `open_settings`). Plus the named keys: `f1`..`f12`,
`up`/`down`/`left`/`right`, `page_up`/`page_down` (aliases `pageup`/`pagedown`,
`prior`/`next`), `home`/`end`, `enter` (alias `return`), `tab`,
`escape` (alias `esc`), `backspace` (alias `bs`), `delete` (alias `del`), and the symbolic
names `plus`/`minus`/`equal` for `+`/`-`/`=`.

GTK's accelerator syntax works too, unchanged from a Terminator config:
`<Primary><Shift>t` is the same binding as `ctrl+shift+t`.
A symbol binding such as `<Control>plus` also matches the Shift needed to
produce `+` on the active layout. An explicit Shift chord takes precedence;
Shift on letters or on a symbol the layout produces without Shift remains
distinct. This needs the native event's unmodified layout character.

A typo'd modifier (`cttrl+t`, `supre+t`) is rejected outright and
flagged by `kettle --check-config` — it doesn't silently degrade
into a bare-key binding.

`action` is one of:

**Tabs**: `new_tab`, `close_tab`, `next_tab`, `previous_tab`,
`move_tab_left`, `move_tab_right`, `goto_tab:N` (1-based, N is the tab
number — `goto_tab:1` is the first tab), `new_tab_shell_N` (1-based —
open the Nth entry of the new-tab `▾` dropdown; `Ctrl+Shift+1..9` by
default), `undo_close_tab` (also
`reopen_tab` / `restore_tab` — restore the most recently-closed tab
from a bounded LIFO ring of 10), `duplicate_tab` (clone the focused
pane's argv + cwd into a new tab — `ssh prod` clones to a second
`ssh prod`).

**Splits**: `new_split:right` (also `split_right` / `split_vert`),
`new_split:down` (also `split_down` / `split_horiz`), `new_split:left`
(also `split_left`) and `new_split:up` (also `split_up`), which put the
new pane left of or above the focused one, `split_auto`
(pick by aspect ratio), `close_pane` (also `close_surface` /
`close_term`). New splits inherit the focused cwd; direct agent/editor
launches split to a shell prompt there, while `duplicate_pane` clones the
focused pane's exact argv + cwd into a right-side split.

**Focus + resize**: `focus_next`, `focus_prev`,
`goto_split:{up,down,left,right}`, `resize_{up,down,left,right}`,
`move_split:{up,down,left,right}` (move the focused pane beside its neighbour
in that direction),
`toggle_zoom` (also `toggle_split_zoom`), `rotate_cw` / `rotate_ccw`
(turn the whole tab's split layout a quarter turn; every pane moves to where
turning the screen would have put it, and the two
directions undo each other).

**Window**: `new_window` (opens another window **in this process**, so
tabs can move live between windows), `close_window`,
`toggle_fullscreen`, `toggle_quick_terminal`, `move_tab_to_new_window` (tear the focused tab out
into its own window LIVE — running programs keep running; dragging a tab
past the tab bar does the same with the mouse, Chromium-style: the live
window rides the pointer, and dropping it on another
kettle window's tab bar merges it there; also in the command palette and,
with Move Tab Left / Right, in the macOS Window menu), `move_tab_to_window` (choose another
window in this process without restarting the tab; also available in the
command palette, including on Wayland), `open_settings`
(`settings` — the Ctrl+, overlay), `layout_picker`, `about` (also
`show_about` — version, update status, GitHub link), `screenshot`
(`take_screenshot` / `terminalshot`).

**Editing**: `copy` (`copy_to_clipboard`), `paste`
(`paste_from_clipboard`), `select_all` (select the whole scrollback + screen;
in the command palette, bindable, no default chord), `select_to_top`
(`select_to_first_line`) and `select_to_bottom` (`select_to_last_line`) —
extend the selection to the top / bottom of the buffer, bound by default to
**Shift+Home** / **Shift+End** (Shift+click still extends to the click point).
**Ctrl+Home** / **Ctrl+End** scroll to the top / bottom instead.

**Search + jump**: `start_search` (`search`), `toggle_search_regex`, `prev_prompt`
(`jump_to_prompt_prev`), `next_prompt` (`jump_to_prompt_next`).

**Scrollback**: `scroll_line_up`, `scroll_line_down`, `scroll_page_up`,
`scroll_page_down`, `scroll_to_top`, `scroll_to_bottom`,
`clear_history` (also `clear_scrollback` / `clear_buffer` — wipes
scrollback only; keep the visible screen unlike `reset`).

**Broadcast**: `broadcast_all` (type to every pane in the window),
`broadcast_off`, `broadcast_group` (type to every pane in the focused pane's
named group — **in every window**, since a group is a set you declared and
`group_all` already spans them; typing and pasting both follow it),
`broadcast_panes_in_tab` (type to every pane in the focused tab, whatever
`broadcast-default` says; no default chord). The default
`Ctrl+Cmd+B` (macOS), `Ctrl+Shift+G` (Windows), or `Super+G` (elsewhere) chord
is `broadcast_tab`, which **toggles** —
pressing it again turns broadcast off, so you never have to reach for a second
chord to stop. Which scope it turns *on* is `broadcast-default`, and the
command palette names the chord's row after that scope. Under
`broadcast-default = off` the chord can only turn broadcast off.

The macOS **Shell** menu and the right-click menu offer the same scopes in a
**Broadcast Input** submenu: **Off**, **Panes in This Tab**, **Panes in This
Window** and **Panes in This Group**. The current scope is checked, and
choosing the checked row turns broadcast off. The chord's hint shows on the
row it turns on: the tab under `group`, the window under `all`, and no row
under `off`. **Panes in This Group** stays checked while any group is the
target; when the focused pane is in another group or none, choosing it turns
broadcast off, and choosing it again broadcasts to the focused pane's group.
It is unavailable while the focused pane has no group: greyed in the menu bar,
hidden in the right-click menu.

A second Kettle process is its own broadcast domain. Use one process with
several windows when a group needs to span those windows.

**Grouping** is separate from broadcasting: first put panes in a group, then
choose whether to broadcast to that group.
`group_all` / `ungroup_all` / `group_all_toggle` put every pane into the group
named `All`; `group_tab` and `group_win` assign a named group to the focused
tab or window (prompting for the name), while `group_tab_toggle` and
`group_win_toggle` toggle a generated one.

**Font**: `increase_font_size` (`zoom_in`), `decrease_font_size`
(`zoom_out`), `reset_font_size` (`zoom_normal`).

**Themes**: `next_theme`, `prev_theme` (`previous_theme`), `toggle_light_dark`
(swap between `light-theme` and `dark-theme`). `next_theme` and `prev_theme`
step through the bundled themes of the current theme's appearance, dark or
light, each followed by the most similar one, so a step never flips between a
light and a dark palette. `open_theme_picker` opens the theme picker: type to
filter every bundled theme by name, or move through them with the arrows, and
the window previews the selected theme. Enter keeps it and writes it to the
config file; Esc restores the theme you started with. With nothing typed it
lists the current theme's appearance first, in the same look order, and the
other appearance after it, and each row says whether its theme is dark or light
and shows a strip of its colours. The command palette lists it as "Choose
theme…" and the right-click menu as "Theme…", and Enter or a click on the
Settings Theme row opens it.

**Titles**: `edit_window_title`, `edit_tab_title`, `edit_pane_title` (open the
inline rename overlay for the OS window / active tab / focused pane). None has
a default key. `edit_tab_title` is also "Rename tab…" in the command palette
and Rename Tab… in the macOS Window menu; all three are in the right-click
menu as Set Window / Tab / Pane Title….

**Media**: `open_media_shelf` opens the focused pane's newest shelf item,
or closes its open preview. Clicking an inline card opens that item. By
default, the viewer is centered within 90% of its split, with visible padding,
and leaves terminal rows, columns and scroll position unchanged. It owns
window input: Tab selects controls, Enter activates, arrows move the picture
or seek video, and Esc closes. A primary backdrop click closes without
reaching the terminal. Video waits for Play.

Choose **Pin** (or P) to keep the same item, zoom and playback in a lane.
Only then does the terminal resize. `media-open-mode = lane` opens directly
in that layout. A lane takes about 40% of the pane on `preview-lane-side`,
leaving at least 20 terminal columns and 5 rows. With less room it becomes
a one-row strip; a pane too small even for that gets a notice.
Keys still go to the terminal while a lane shows. Drag the lane's edge
beside the terminal, marked by a short grip, to give it more or less of the
pane: the terminal keeps its 20 columns and 5 rows and the lane its least,
and the program in the pane sees its new size as you drag. The lane's header browses
the shelf (`‹`/`›`), opens the item outside (`↗`), collapses or expands the
lane (`▾`/`▴`) and closes it (`×`). For an SVG or a Mermaid diagram it also
shows the source the item was rendered from (`≡`, and `▣` back to the
picture), a row per line, with tabs expanded and characters that would
reorder or hide text shown as `�`; the wheel scrolls the source, and a
sideways swipe, or the wheel with Shift held, scrolls it sideways. The
picture zooms with `−` and `+`, a pinch, or the wheel with Cmd held on
macOS and Ctrl elsewhere, at the pointer, from an eighth of its fit to 32
times it; the wheel or a drag pans it, and `⤢` shows it fitted again.
Zoomed past the pixels the lane holds, the picture sharpens a moment after
it stops moving: the part in view is drawn again at the size shown (a
raster up to its own pixels), read from its file again only when you
zoomed or panned it yourself, and not at all once the file changed.
`◐` puts the picture on the next background: the pane's, white,
then a checkerboard; a diagram is drawn again for it, as its colors follow
the background, from the same source, and if the file changed since, the
lane says so and keeps the picture as it was. `⧉` copies the picture, or the
source while the source shows, exactly as it was read. An item read from a
file has `↻`, which reads the file again and shows what it holds now, in the
same place on the shelf. A gallery (the Mermaid diagrams of a Markdown file,
or several diagrams copied together) shows one diagram at a time, its place
at the end of the detail line between `‹` and `›`, which show the one
before or after; another diagram is drawn from the gallery as it was read,
never the file again, on the background you chose, replaces the one shown
only once it is ready (the place shows the one asked for meanwhile), starts
fitted and its source from the top, and stays put if it cannot be drawn.
`↻` reads a Markdown file's gallery again at the diagram shown. Where the header is too narrow, these give way,
open outside first, then zoom out, zoom in and fit, copy, browsing, the
background and the source, and reload last. A press or the wheel over a lane never
reaches the terminal, and menus and dialogs open over it without closing it.
The command palette lists it as "Open media shelf". On macOS its default
chord is `Ctrl+Shift+Cmd+I`; elsewhere it has no default binding.
`preview_link` (`preview-link`) is quick select for previews: it labels the
image (PNG, JPEG, WebP, BMP, GIF, TIFF, HEIF, AVIF), SVG, Mermaid (`.mmd`,
`.mermaid`), Markdown (`.md`, `.markdown`, whose Mermaid diagrams are a
gallery), PDF and video files the focused pane names, by their extension, as a
whole path (not on Windows, where path hints are never resolved) or a local
`file://` link.
Type a label to render that file in the configured preview presentation,
which opens when it is ready (see **Media** above). The default viewer owns
keyboard input; choose **Pin** or `media-open-mode = lane` to keep the preview
beside the terminal. An already open preview keeps its presentation. A link
from another machine's pane is refused. Behind tmux, screen or zellij, whose
server runs on this computer, it renders as it would locally.
A control client can open this quick select but not pick from it: only your
own key press or click reads a file. With nothing to label it says so; a
file that cannot be previewed (gone, unreadable, too large, not an image, or
previews unavailable) is explained in a notification. The right-click menu
offers the same as "Preview in Kettle" on a link to such a file. The command
palette lists it as "Preview a file in Kettle"; it has no default binding.
`preview_clipboard_path` (`preview-clipboard-path`) previews the file the
clipboard names using the configured preview presentation: the first image,
SVG, Mermaid, Markdown, PDF or video file a file manager copied, or else one
copied path
(resolved from the pane's folder; quotes and a shell's backslash escapes
around it are undone) or `file://` link. A copied path or link meets the
focused pane's gate, as one in its output would; a file a file manager copied
is this computer's. On Windows only a drive path counts, never a
`\\host\share` path.
Shift+right-click offers it too, as "Preview Copied File", when the
clipboard names such a file; only Shift+right-click reads the clipboard for
that. As with `preview_link`, a control client cannot have it read a file.
The palette lists it as "Preview the copied file in Kettle".
`render_clipboard_as_diagram` (`render-clipboard-as-diagram`) renders the
Mermaid diagram on the clipboard using the configured preview presentation,
as `/copy` in Claude Code or Codex leaves it: each ```` ```mermaid ```` fence
as written, several as one gallery paged with the `‹` and `›` on the preview's
detail line (at most 32: more are refused, never cut short), or else the
diagram in the text, from its front matter, directives or header to its end,
with the harness's bullet, label, notice, leading and trailing prose and
common indentation taken off. `render_selection_as_diagram`
(`render-selection-as-diagram`) does the same for the selected text, and also
joins back the rows the agent's UI wrapped at the pane's width; where a wrap
cannot be told from a new line for sure, as for text wrapped at another
width or a word Codex may have broken at a hyphen, it refuses and suggests
`/copy` rather than guess, and where an unindented diagram's end cannot be
told from the prose after it, it asks for just the diagram. The right-click
menu offers "Preview Selected Diagram" when the selection holds a diagram,
and Shift+right-click offers "Preview Copied Diagram" when the clipboard
does. Neither has a default binding, and a control client cannot have either
read the clipboard or a selection, nor have a menu it opens look at them for
its rows. The palette lists them as "Preview the copied diagram in Kettle"
and "Preview the selected diagram in Kettle". `copy_agent_setup`
(`copy-agent-setup`, palette: "Copy agent setup commands", no default
binding) puts on the clipboard the `codex` function `kettle agent-setup
--print` prints, for the shell new panes start (`shell`, else `SHELL`; bash,
zsh or fish, on macOS and Linux), naming this Kettle's own path; a
notification says it was copied, or why not: another shell, a translocated
copy whose path would not last, or a clipboard Kettle could not reach. `preview_next`,
`preview_previous` and `close_preview` browse and close the focused pane's
lane as its `›`, `‹` and `×` do ("Next preview", "Previous preview" and
"Close media preview" in the palette), `preview_next_page` and
`preview_previous_page` show the next or previous page of a PDF or of a
gallery (a Markdown file's Mermaid diagrams, or several copied together) as
the `›` and `‹` on its detail line, PageDown and PageUp do ("Next page in
preview" and "Previous page in preview"; `preview_next_diagram` and
`preview_previous_diagram`, their first names, still bind), and `preview_source`,
`preview_canvas`, `preview_copy`, `preview_reload`, `preview_zoom_in`,
`preview_zoom_out` and `preview_fit` as its `≡`, `◐`, `⧉`, `↻`, `+`, `−` and
`⤢` do ("Show preview source", "Next preview canvas", "Copy preview",
"Reload preview", "Zoom preview in", "Zoom preview out" and "Fit preview to
lane"), and `preview_play` ("Play or stop the silent preview") plays a
video's silent preview, eight frames from across it shown in turn, until it
or the lane's `■` stops it; it never starts while Kettle handles a control
request or a Lua script's action. `video_play` ("Play or pause the video") works as
the lane's `▶` and `‖` do: the video plays in the lane, with its sound, from
where it stands, and pauses; it too never starts while Kettle handles a
control request or a Lua script's action. `video_mute` ("Mute or unmute the
video") works as the lane's `♪` does, once the video's sound can play: it
mutes the sound, and lets it be heard again, the video playing on; a control
request or a Lua script's action may mute it, but never unmute it. `focus_preview` ("Focus preview") gives the focused pane's expanded
lane the keyboard, and its footer says "Keyboard: preview": the arrows move
the picture (or scroll a source), Page Up and Page Down show the previous
and next page of a PDF or gallery, Home and End its first and last, `+` and
`-` zoom, `0` fits it (or shows a source
from its start), `c` copies, Space or `k` plays or pauses a video (or stops its
silent preview), `m` mutes or unmutes its sound, `t` toggles theater mode,
`f` toggles fullscreen, and the Context Menu key or Shift+F10 opens the
preview menu. Left and Right step a video five seconds while it shows
in the lane, and every other key and chord, Enter,
digits and Kettle's own shortcuts included, does nothing, so nothing typed
reaches the program in the pane (Option chords count on macOS). Esc, a
click outside the lane, a file drop, another menu or dialog opening, or the
lane closing, collapsing or changing item gives the keyboard back; the
pointer and the wheel work as ever elsewhere, and an input method stays off
meanwhile. None has a default binding.

Hint labels use the shortest prefix-free codes, with the easiest keys assigned
from the bottom of the viewport. Blank cells before a match hold its label;
at column zero blank cells after it are used when available. Without space the
label overlays the match start. A translated legend explains the available
plain-label and Shift-label actions and Esc cancellation. Explicit OSC 8 captions
use their stored destination. Quick-select hints is also available in the
macOS Edit menu, the palette (with its current binding), and the terminal context
menu when a link or path is visible. This adds no configuration keys.

**Modals + UI**: `command_palette` (`palette`), `hint_mode` (`hints` /
`quick_select`; labels the URLs, paths (a quoted or backticked path may hold
spaces), git hashes and IP addresses on screen:
type a label to open a URL or copy anything else, or hold Shift while typing it
to copy a URL or open a path in the default app, resolved from that pane's
directory. A path is copied instead when it names another machine's file (a
detected ssh, mosh or container session), when the label marks only part of a
name, such as `out/report` in `out/report#1.pdf`, or when it climbs with `..`.
Behind tmux, screen or zellij a path opens as it would locally; one that is
not on this computer, as when an ssh inside tmux printed it, gets a notice.
Kettle does
not look the path up itself, so printed text cannot make it reach a network
mount. On Windows path hints are always copied),
`new_ssh` (`ssh`), `context_menu`
(`open_context_menu` — the right-click menu — mouse-only by default
but bindable to a keyboard trigger if you want the menu opened at the
cursor position).

**Vi-mode**: `toggle_vi_mode` (`vi_mode` / `vi`) — enter keyboard-driven
copy/navigation mode (default `Ctrl+Shift+Space`): `h`/`j`/`k`/`l` move,
`0`/`$`/`g`/`G`/`H`/`M`/`L` jump, `v` starts a visual selection, `V` a
line selection and `Ctrl+v` a block selection (pressing another of the three
switches the selection to that kind, the same one again ends it), `y` yanks it
to the clipboard, `Esc` exits. See `man kettle` for the full keymap.

**Misc**: `reset` (RIS — full terminal reset including engine state;
like `clear_history`, it acts on Kettle's terminal, not the program in it,
so it also works on a read-only pane),
`reload_config`, `detach_tab` (alias of `move_tab_to_new_window`),
`text:BYTES` (send literal text to the focused pane, as though typed — so
while broadcast is on it reaches every pane in scope, exactly as typing the
same bytes would).

`text:` takes a payload rather than a name, so it is spelled out here rather
than listed by `--list-actions`. Escapes: `\n` `\r` `\t` `\e` `\a` `\b` `\f`
`\v` `\0` `\xHH` (hex, `00`–`7f`) and `\\`. An `=` must be written `\x3d`,
because a `keybind` line is split on its last `=`. Anything else after a
backslash is an error rather than a literal backslash, so `--check-config`
names the line. The payload is capped at 256 bytes: this is a chord, not a
paste.

```ini
# What macOS text fields do, for terminal apps that only speak Control codes.
# `#` starts a comment only at the beginning of a line, so these cannot carry a
# trailing one — anything after the payload would become part of the payload.
# ^U — delete to start of line (bound by default on macOS)
keybind = cmd+backspace = text:\x15
# ^A — start of line
keybind = cmd+left      = text:\x01
# ^E — end of line
keybind = cmd+right     = text:\x05
```

Only the first is bound by default, and only on macOS. The other two are left
out because `^A` increments the number under the cursor in Vim's normal mode,
which edits the buffer silently. `^U` merely scrolls there. Turn the default
off with `keybind = cmd+backspace=unbind`.

> This list covers the common actions. For the **complete** set of bindable
> action names, including every alias, run `kettle --list-actions`. It prints a
> table that lives beside the parser rather than the parser itself, so a test
> in CI derives every name the parser accepts and fails if the table omits one.
> Each alias appears once, in its underscore spelling; hyphens and underscores
> are interchangeable everywhere (`bell_off` and `bell-off` both parse). What
> cannot be enumerated — the parametric `goto_tab:N`, `switch_to_tab_N`,
> `new_tab_shell_N` and `text:BYTES`, and the `unbind` sentinel — is printed as
> trailing notes.

The action `unbind` (also `none`, `null`, `false`, or an empty string) removes
the default binding for that trigger — useful when a default like
`Ctrl+Shift+C` collides with a chord your shell or another tool wants.
Example: `keybind = ctrl+shift+c=unbind`.

See [`kettle.example.config`](kettle.example.config).

## Settings display text

The Settings panel uses sentence case for labels and display values. Config
values keep their existing spelling. For example, `gpu-backend = auto` displays
as **Automatic**, while `gpu-backend = dx12` displays as **DirectX 12**.
GPU device labels resolve ID-only and name-only pins against detected adapters;
**Automatic** means no adapter is pinned.
`scrollback-bytes = 120MB` displays as **120 MB** under **Scrollback memory**.
**Update check interval** displays hours as **24 h**. The panel also displays
`13 px`, `6 px`, `10 s`, and `99%`. See [SETTINGS.md](SETTINGS.md).

Unknown keys, invalid values and malformed lines at startup and reload also
appear in a dismissible in-window notice. It names the config filename, source
line and key for up to three settings, followed by `+N more`. Each line explains
the problem: unknown settings can suggest a nearby known key or alias, invalid
values show the expected type or range when available (for example, `font-size`
expects a number from 5 to 72), and malformed lines identify a missing `=`.
Values are omitted, including environment values. Filenames and keys have
control characters removed and are limited to 64 characters each.

**Open Config** opens the file named by the notice with the operating system's
default app, using the same mechanism as **Advanced…**. If a screen reader
moves focus to the notice, Enter opens the file and Escape dismisses it. While
the terminal retains focus, those keys go to the running shell or program. Both
buttons remain available to screen readers. The terminal grid and IME cursor
do not move.
Valid settings still apply. Dismissing a notice keeps it hidden until the
diagnostics change. Use `kettle --check-config` for the complete diagnostics.

### Tab-strip appearance and controls

Solid tab strips use the theme background darkened by 24% for dark backgrounds
or 6% for light backgrounds. The active tab joins the terminal background,
including its transparency, and keeps an accent line. Wallpaper strips retain
`chrome-background` behaviour. Solid strips no longer use ANSI bright black;
`chrome-background = theme` selects that colour only with a wallpaper.

Horizontal New Tab Options and New Tab controls each have a hit area of at
least 28 by 28 points (growing with a taller strip), a 4 point gap, an 8 point outside margin, and rounded hover and
pressed feedback. Their tooltips follow the UI language; New Tab also shows
its current configured shortcut. At 420 by 260 points, overflow keeps the
active title visible alongside both controls. Closing or renaming tabs uses
the same layout as pointer actions and accessibility.

When a window loses focus on macOS or Linux, tab text and controls dim toward
their backgrounds while retaining at least 3:1 contrast, and the active accent
line becomes neutral. Focused labels and controls retain at least 4.5:1;
separators and hover fills retain at least 1.5:1. The tab strip does not take
keyboard focus from the terminal; tab-selection shortcuts remain available.

## Profiles and named layouts in the UI

See [Sessions, profiles and layouts](../README.md#sessions-profiles-and-layouts)
and the [getting started guide](GETTING-STARTED.md#profiles-layouts-and-ssh)
for the new-tab menu, macOS Shell menu and command palette routes. Profile
choices follow `kettle --list-profiles` from the default config directory's
`profiles/*.config`, including when launched with `--config FILE`. They launch
separate processes/windows. The empty Create Profile action opens that directory.
Named layouts use `layouts/*.json` beside the default config; Save Layout
replaces an existing name and excludes quick-terminal windows.

No new configuration keys are needed. Optional `keybind` actions
`save_layout` (name prompt), `open_layout_picker` (Open Layout) and
`open_profiles_directory` (create/open the profiles directory) expose the same
routes. `ssh` opens the SSH launcher; `ssh-host = name=target` supplies its
configured targets. Save names accept 1–128 ASCII letters, digits, `.`, `_` and
`-`, except `.` and `..`. Restore policy remains controlled by `restore-session`
and the launch flags described above.
