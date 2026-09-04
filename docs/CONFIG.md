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
`scroll-multiplier`, `minimum-contrast`, `cursor-blink-interval`) clamp at
parse-time *and* flag out-of-range values via `--check-config`.

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
| `theme` | string | `TokyoNight Night` | Any bundled theme (`kettle --list-themes`). Runtime theme changes — the Settings picker, the right-click Theme submenu, `next_theme`/`prev_theme`, light/dark toggle — are written back to this line, so a picked theme persists across launches |
| `font-family` | string | `JetBrainsMono Nerd Font` | Bundled; falls back to system fonts |
| `font-family-bold` / `-italic` / `-bold-italic` | string | — | Per-style family overrides (fall back to `font-family`) |
| `font-size` | float | `13` | |
| `cell-width` / `cell-height` | float 0.5–3.0 | `1.0` | Multiplier applied to measured terminal cell width / height. Values are clamped at parse time and reload live with font metric changes |
| `text-renderer` | enum | `grid` | `grid` \| `legacy` (v2.25.0). `grid` pins every glyph to its terminal cell (`col × cell_w`) so fallback-font glyphs, emoji, symbols, and ligatures cannot drift away from selection, cursor, or mouse hit testing. `legacy` restores the pre-2.25.0 continuous layout for diagnosing renderer regressions |
| `background` / `foreground` | color | from theme | Hex/`#rgb`/`rgb:`/X11 name |
| `cursor-color` | color | from theme | The block cursor color. `cursor-bg-color` and `cursor_bg_color` are aliases |
| `cursor-fg-color` | color | from theme | The glyph color under a focused block cursor. The block is filled with `cursor-color` and its glyph is recolored to this value |
| `selection-background` / `selection-foreground` | color | from theme | |
| `palette` | `N=#RRGGBB` | from theme | Repeatable, `N` = 0..15 |
| `search-foreground` / `search-background` | color | from theme | Search-match + quick-select highlight. Default derives from the active theme (`search-background` → the theme's yellow `palette[3]`, `search-foreground` → the theme background), so it matches whatever theme is set; override with an explicit color |
| `scrollback` | int / `infinite` | `10000` | Line-count history cap; `0`, `infinite` or `unlimited` = effectively unbounded before the byte cap is applied |
| `scrollback-bytes` (`scrollback-byte-limit`, `scrollback-memory`) | bytes with optional `K`/`M`/`G`, `KiB`/`MiB`/`GiB`, or `0` | `10000000` | Per-pane scrollback memory budget. Includes the active screen, protects visible rows, and trims oldest history by reducing the effective line cap. `0` disables the byte cap and uses `scrollback` only. The budget is an **estimate over the inline grid**: it does not walk each cell's optional heap storage (combining marks, underline color, hyperlink), because that would mean touching every cell on the PTY reader's path. Those are separately bounded — combining marks are capped per cell, and hyperlink text is shared across the cells of one link — so actual usage exceeds the configured value by a bounded factor rather than an unbounded one |
| `window-padding-x` / `window-padding-y` | float | `8` | Inner padding (px) |
| `window-width` / `window-height` | int cells | unset | Initial fresh-window terminal grid size. Width is clamped `[20, 400]`, height `[8, 200]`. If only one dimension is set, the other uses Kettle's startup baseline (`100x36`). Applied only as the startup seed; restored session geometry and explicit new-window geometry take precedence |
| `window-position-x` / `window-position-y` | int px | unset | Initial fresh-window position in physical pixels. Negative coordinates are valid for monitors left/above the primary display. Applied only as the startup seed; restored sessions and explicit new-window placement take precedence |
| `background-opacity` | float | `0.86` macOS/Windows; `0.99` elsewhere | 0..1. Solid backgrounds request an alpha surface below `1.0`; transparent backgrounds use `background-opacity × background-darkness`; image backgrounds always allow decoded image alpha; starfield stays opaque because its shader covers the surface. macOS and Windows default to visible native material; other targets stay at 99% whether compositor blur is available or not. Set this to `1.0` and `window-blur = false` for a fully opaque window. On macOS, the native titlebar stays opaque and follows Kettle's selected light or dark appearance rather than inheriting content alpha |
| `window-blur` | bool | `true` | Ask the window system to blur content behind Kettle. macOS keeps material below an opaque native titlebar, follows the selected light or dark appearance, and becomes opaque when Reduce Transparency is enabled. Windows requests the system backdrop and palette-matches its DWM caption. Other targets default to 99% opacity. Linux enables blur only when the active Wayland compositor advertises KWin's blur protocol; X11 and unsupported Wayland sessions also apply a 99% live-opacity safety floor to explicit lower values. Screenshots and the saved opacity are unchanged. A newly opened window is required when changing from an opaque startup surface |
| `cursor-style` | `block`\|`underline`\|`bar` (`beam`) | `block` | `beam` is an alias for `bar` |
| `cursor-style-blink` (`cursor-blink`, `cursor_blink`) | bool | `true` | Cursor blinks while the window is focused. The short alias `cursor-blink` is the spelling the right-click Preferences submenu writes back |
| `bell` | `off`\|`visual`\|`attention`\|`both` | `both` | Visual flash and/or window-attention (taskbar/dock urgency) on `BEL` |
| `bell-flash-intensity` (`bell_flash_intensity`) | float 0–1 | `0.10` | Peak alpha of the visual-bell flash. The flash is a full-surface wash of the theme foreground that decays to nothing over 300 ms, so this sets how bright its first frame is. The most frequent bell in practice is an empty Tab completion, which does not warrant a bright wash; raise this if you want the old, stronger flash, or set `0` to drop the flash while keeping window attention. Full-surface flashes are also the part of a terminal most likely to affect a photosensitive user |
| `osc52` (`clipboard`) | `off`\|`copy`\|`paste`\|`both` | `copy` | OSC 52 clipboard policy. `copy` allows programs to set the clipboard but **not** read it (a remote read is a clipboard-exfiltration risk); `paste`/`both` enable read. Target `c` uses the regular clipboard; target `p`/`s` uses Linux PRIMARY without cross-target fallback (platforms without a separate selection use their one clipboard). DA1 advertises clipboard extension `52` only when writes are enabled and the platform clipboard is available; live reload updates that advertisement for existing panes |
| `macos-option-as-alt` | `none`\|`left`\|`right`\|`both` | `none` | Selects which macOS Option key behaves as terminal Alt **for keys that produce text**. `none` preserves normal macOS composition on both sides, so Option-produced symbols and accented characters reach the PTY without a Meta/ESC prefix. Keys that compose no character — Backspace, Delete, the arrows, Home/End, Page Up/Down, Insert and the F-keys — always carry `Alt` to the PTY, on every setting and from either side, because there is no composition for the policy to protect: `⌥⌫` is `ESC DEL` (readline's `backward-kill-word`) and `⌥←`/`⌥→` are word-wise motions. kitty draws the same line. A selected side uses the unmodified key character and keeps `Alt` for keybinds, legacy xterm encoding, Kitty keyboard encoding, and modifier parameters. Ctrl+Option and Cmd+Option chords keep `Alt` in every mode, matching macOS's suppression of Option composition for those chords — they keep it for keybind matching and Kitty encoding; a Cmd-bearing chord has no legacy PTY encoding and is not written at all (see [`TERMINAL-CLIENT-COMPATIBILITY.md`](TERMINAL-CLIENT-COMPATIBILITY.md)). Applies to existing windows on live reload. The key remains parseable but is reported inert on non-macOS platforms so one shared config works everywhere |
| `modify-other-keys` (`modify_other_keys`) | `auto`\|`always`\|`off` | `auto` | Controls only Kettle's modified-Enter fallback before an application queries or sets a keyboard protocol. `auto` recognizes Codex, Claude Code, Gemini, and OpenCode. On Unix/macOS it requires noncanonical input and a fresh foreground process-group match to either the direct launch identity or the background process snapshot. On Windows it requires a running command with one unambiguous shell-child branch containing a recognized composer, or a direct composer launch; helper forks below that composer are supported. Nested shells, readline/libedit programs, SSH/WSL transports, wrappers, and snapshots without a recognized composer receive plain Enter, preventing the literal `;2;13~` suffix an unsolicited xterm sequence can leave behind. Use `always` for an unrecognized or unobservable client, including one inside SSH/WSL; legacy `enter` is its alias. `off` removes the fallback. All modes still honor application-selected xterm levels and Kitty CSI-u, which take precedence. Reloads live, and GUI, control-plane `send_keys`, and broadcast input evaluate the policy separately for each target pane |
| `paste-images` (`paste-image`) | bool | `on` | Turn a clipboard bitmap into an owner-only temporary PNG and paste its quoted path. Kettle keeps at most 64 files and 256 MiB of final PNG data per process, rejects source buffers above 256 MiB, removes partial writes, and deletes successful files at exit. `off` disables bitmap materialization without changing ordinary file paste. See [Architecture](ARCHITECTURE.md#private-clipboard-images-and-video-receipts) for the descriptor, identity, and crash-cleanup model |
| `paste-image-preview` (`paste_image_preview`) | bool | `on` | After the initiating pane accepts its own Kettle-created bitmap path, show a pane-local thumbnail receipt. It expands for four seconds, then contracts until its 30-second lifetime ends. Hover pauses that timer, but a two-minute hard limit always removes the receipt. The newest media paste replaces the previous receipt, and later keyboard, paste, or control input dismisses it because the command line may have changed. The card reports the image dimensions and never claims the client attached or opened it. Click the body to open the retained PNG or `×` to dismiss it. Remote panes warn that the path is local. `off` avoids creating or retaining preview pixels without disabling bitmap-to-path paste. Arbitrary paths are never previewed |
| `paste-files` | bool | `on` | Paste an explicit clipboard file list as shell-quoted paths. WSL panes receive translated `/mnt/…` paths. `off` makes copied files a no-op; drag and drop remains available |
| `paste-video-preview` (`paste_video_preview`) | bool | `on` | Show the same pane-local receipt for an explicitly copied or dropped video path after that pane accepts the paste. The first video gets a bounded poster; a batch reports its count. macOS and Windows use native thumbnail APIs. Linux reads an existing integrity-checked Freedesktop thumbnail-cache entry and otherwise keeps the generic poster. Kettle never decodes video, scans terminal text, or opens a video from the card; clicking its body or `×` only dismisses the receipt. Two background threads feed an eight-job queue. Each hidden child has a two-second deadline, a 256 by 160 RGBA limit, and one fresh-worker retry after a deadline failure. If a child cannot be reaped, that queue thread retires. Pending receipt state expires after 38 seconds if no worker replies. Preview eligibility requires a trusted non-link regular file and parent chain. Sources that fail the platform trust checks keep normal path paste but get no receipt; on Unix this includes group-writable and hard-linked files. The child holds the source open and revalidates it before and after extraction. `off` leaves path paste unchanged and skips poster work |
| `record` | `off`\|`on` | `off` | Arm the session recorder at launch, writing an asciicast trace of the window session. **Off by default.** Recording captures on-screen output verbatim — review a trace before sharing it. `--record PATH` / `--record-dir DIR` / `KETTLE_RECORD*` override this for a single launch. The window title carries `[REC]` while active. See [RECORDING.md](RECORDING.md) |
| `record-dir` | path | `<config-dir>/recordings` | Directory that `record = on` writes per-session traces into (collision-safe `kettle-session-*.cast` files; mode `0600` and directory mode `0700` on Unix, or a protected current-user file DACL on Windows). Ignored when recording is off or an explicit `--record PATH` is given |
| `record-raw-input` | bool | `off` | Capture RAW typed characters (**including passwords**) instead of redacted key tokens while recording. A separate, explicit opt-in from `record`; off by default. The title shows `[REC RAW]` while active |
| `tab-bar` | `off`\|`auto`\|`always` | `always` | When the tab bar is shown (`auto` = only with >1 tab) |
| `tab-bar-position` (`tab-position`) | `top`\|`bottom`\|`left`\|`right`\|`hidden` | `top` | Where the tab bar sits. `left`/`right` render a **vertical** tab strip (its width is `tab-bar-width`); `hidden` forces the bar off regardless of `tab-bar` |
| `tab-bar-width` | float 40–600 px | `180` | Width of the vertical tab strip when `tab-bar-position = left`/`right`. Clamped `[40, 600]`; ignored for `top`/`bottom` bars |
| `tab-min-width` | float 40–600 px | `120` | Minimum width of a horizontal tab segment. Tabs divide the bar evenly and **fill it** (no maximum — they always maximize width); once they would shrink below this, the bar overflows and (with `scroll-tabbar`) scrolls. Clamped `[40, 600]` |
| `scroll-tabbar` | bool | `true` | When horizontal tabs would shrink below `tab-min-width`, keep them at that width and scroll the bar with `‹ ›` arrow buttons + the mouse wheel (active tab kept in view). `false` lets them keep shrinking to fit instead |
| `unfocused-split-opacity` | float 0.1–1 | `0.7` | Dim level of unfocused split panes |
| `scroll-multiplier` (`mouse-scroll-multiplier`) | float 0.1–50 | `1.0` | Mouse-wheel scroll-speed multiplier (1.0 ≈ 3 lines/notch). Applied with **sub-notch precision**: precision touchpads and high-resolution wheels report a fraction of a notch per event, and the remainder is carried across events instead of being rounded away, so slow gestures scroll smoothly and proportionally. Small multipliers stay usable for the same reason — at `0.1` a notch is 0.3 lines, which accumulates over three notches rather than vanishing |
| `disable-mousewheel-zoom` | bool | `false` | When `true`, Ctrl+wheel does NOT change the font size. Useful for users who accidentally scroll-zoom on a laptop touchpad. The keyboard IncreaseFontSize / DecreaseFontSize / ResetFontSize chords still work |
| `smart-copy` | bool | `true` | `true` preserves the existing clipboard when there is no selection. `false` replaces it with empty text on every Ctrl+Shift+C. This is separate from `copy-on-select`, which controls automatic copying after selection |
| `menu-item` (repeatable, `menu-item = LABEL = CMD`) | string | none | Add a row to the right-click context menu that writes `CMD\n` to the focused pane's PTY when clicked. Repeatable — each `menu-item = …` line appends one row. Use `menu-item = Clear screen = clear`, `menu-item = Open editor = $EDITOR ~/.bashrc`, etc. For richer behavior (Lua callbacks, conditional rows) use `kettle.add_menu_item(label, callback)` from `init.lua` — see [`docs/examples/init.lua`](examples/init.lua) |
| `handle-size` | int -1–50 px | `-1` | Split-divider stroke width. `-1` = use the theme default (1 px). Higher values give a chunkier divider — useful on high-DPI displays where 1 px is hard to see |
| `geometry-hinting` | bool | `false` | When `true`, request an approximate 8×16 logical-pixel resize increment. This is not recomputed from the active font metrics, so it does not guarantee integral terminal rows/columns and does not track font zoom. X11 honors the hint; Wayland support varies by compositor; macOS ignores it, and Kettle does not currently apply a native Windows increment |
| `focus` | `click`\|`sloppy`\|`system` | `click` | Focus-follows-mouse policy. `click` (default) — focus on click. `sloppy` — focus on cursor movement; pane under the cursor becomes focused without clicking. `system` — kettle treats this as `click` (winit doesn't expose the OS-level focus policy, so the OS-managed mode falls back to explicit-click behavior) |
| `minimum-contrast` | float 0–21 | `0.0` | WCAG 2.0 minimum contrast ratio of cell text against its background; `0` = off. `4.5` ≈ WCAG AA, `7.0` ≈ AAA. Foreground is lifted toward white/black as needed |
| `window-title-format` (`title-format`) | string | `{title} — kettle` | OS window title template — placeholders `{title}` (active pane title), `{cwd}` (active pane cwd), `{tab}` (1-based tab index); `{{`/`}}` escape literal braces. Native OS titlebars strip Private Use / Nerd Font glyphs that desktop UI fonts commonly render as tofu boxes; Kettle-rendered tabs and pane titlebars keep them |
| `tab-format` (`tab-title-format`) | string | `{n}: {title}` | Per-tab label template — placeholders `{n}` (1-based tab index), `{title}` (focused pane title). The trailing `✕` close button is appended by the renderer |
| `scrollbar` | `never`\|`auto`\|`always` | `auto` | Per-pane overlay scrollbar. `auto` shows it whenever the pane has scrollback history — dim at rest, brighter while scrolled back or while the pointer hovers/drags it; `always` also draws the empty gutter with no history. Click or drag the thumb to scroll |
| `scrollbar-width` | float 2–40 logical px | `6` | Visible overlay-thumb width. Kettle scales it for display DPI and keeps a separate invisible hit strip of at least 12 logical px. Dragging preserves the pointer's position inside the thumb |
| `split-divider-color` | color | theme `palette[8]` | Pane border/divider color for *inactive* panes |
| `focused-split-color` (`split-divider-color-focused`) | color | theme `palette[4]` | Border color for the *focused* pane — the "here am I" accent. While **broadcast mode** is on (`Ctrl+Cmd+B` on macOS, `Ctrl+Shift+G` on Windows, or `Super+G` elsewhere), this is temporarily overridden by theme `palette[3]` (yellow) to signal the active state; the configured color is restored when broadcast turns off |
| `cursor-blink-interval` | int ms | `530` | Cursor blink half-period |
| `tab-silence-threshold-ms` (`tab-silence-threshold`) | int ms | `10000` | An inactive tab whose unseen output went quiet for this long transitions from the cyan `Output` dot to the dim `Silent` dot. Clamped `[1000, 600_000]` |
| `command-notify-threshold-ms` (`command-notify-threshold`) | int ms | `5000` | Minimum command duration before Kettle fires a desktop notification when an OSC 133 D (CommandEnd) event arrives **while the window is unfocused**. `0` disables. Requires shell integration because the shell emits the event. Clamped `[0, 86_400_000]` |
| `copy-on-select` | bool | `true` | Auto-copy the selection to the clipboard on release |
| `update-policy` | `off`\|`notify`\|`auto` | `auto` | Stable-channel behavior after the first-launch privacy skip: no automatic request, a passive banner, or an authenticated background install used after the next restart. **`auto` is the default** (kettle keeps itself current, oh-my-zsh style); set `off` to opt out. Official installer ownership is required for installation. See [UPDATES.md](UPDATES.md) |
| `update-check-interval-hours` | int hours | `24` | How often the background check may contact the release feed. Default 24 (daily); floored at 1. `update-policy = off` disables checking regardless |
| `update-check` (`check-for-updates`) | bool | compatibility alias | Legacy setting mapped to `notify` (`true`) or `off` (`false`). `update-policy` wins regardless of line order. `kettle --check-update` always performs a one-shot check |
| `restore-session` (`restore_session`) | bool | `false` | Reopen the previous session (tabs, splits, working dirs) on launch. **Off by default** — like every mainstream terminal, a new window/instance opens fresh (a single pane in the default cwd). The session is always *saved* on exit only when this is on (or `--restore` is passed), so a fresh window never clobbers a saved layout. `--restore` is the one-shot equivalent; `--layout NAME` restores a named workspace independently |
| `agent-server` | `off`\|`read-only`\|`full` | `off` | The agent control server mode. **Off by default.** When enabled, kettle starts a local-IPC control server that an AI agent / `kettle ctl` / `kettle mcp` can use to read the screen and drive panes (`read-only` reads / lists / subscribes; `full` also sends text + runs commands). Security: local-only — a Unix domain socket (mode `0600`) or a Windows named pipe (current-user DACL); no TCP. `--agent-server <mode>` is the per-launch override. See [docs/AGENT.md](AGENT.md) |
| `agent-badge` | string | `"[agent] "` | The per-pane titlebar prefix shown while an agent connection has the pane attached. Set to any glyph you like (`agent-badge = 🤖 `); empty disables it |
| `scroll-on-keystroke` (`scroll-on-input`) | bool | `true` | Jump back to the bottom when the user types while scrolled back |
| `scroll-on-output` | bool | `false` | Jump back to the bottom when new output arrives while scrolled back. Off by default so a chatty background job does not interrupt reading old output |
| `mouse-hide-while-typing` (`mouse-hide`) | bool | `true` | Hide the OS mouse cursor while typing and show it again on mouse movement |
| `word-delimiters` (`selection-word-chars`, `semantic-escape-chars`) | string | engine default | Characters that delimit a word for double-click selection. Leave empty for the engine default. For example, `()[]{}` makes `/` part of a word so paths and URLs select as one unit |
| `font-feature` | string | — | OpenType feature(s), repeatable / comma-list. Forms: `liga`, `+calt`, `-liga`, `liga off`, `ss01`, `cv01=2`, `zero 1`. Applied on top of the ligature toggle |
| `command` / `shell` | string | `$SHELL` | Program to launch |
| `ssh-host` | `name=user@host` | — | Repeatable; named target for the `Ctrl+Shift+S` SSH launcher |
| `keybind` | `trigger=action` | built in map | Repeatable |
| `accent-color` | `auto` \| `theme` \| color | **`auto`** | The UI-chrome accent — active-tab strip, focused-pane border, per-pane titlebars, drag ghost, settings/menu highlights. **`auto` (the default since v2.18) is Peacock behavior, per *window***: each window claims a distinct hue from the theme's accent pool, seeded by the working directory (same project → same starting hue, stable across launches) and live-deduped against every other kettle window — including other kettle processes — so two open windows never share a hue while the pool has a free one. A theme switch keeps each window's pool slot. **`theme`** (also `off`/`none`) opts out: every window uses the theme's signature accent (the default TokyoNight Night's blue `#7aa2f7`, matching the app icon; Catppuccin Mocha instead uses its mauve `#cba6f7`; `palette[4]` for themes without an `accent`). A `#rrggbb`/`#rgb`/`0xRRGGBB`/X11 color pins one color for every window (skips the dedupe). CLI `--accent COLOR` wins over the config. `palette[3]` broadcast yellow and the cursor are not affected by design |
| `status-bar` (`statusbar`) | `off\|top\|bottom` | `off` | Show a thin strip at the configured edge with `HH:MM:SS UTC · theme · focused pane title`. Disabled by default. Aliases: `none` / `false` = off, `on` / `true` = bottom |
| `trigger` | regex \[`:: cmd args`\] | — | Repeatable. A match against output in an unfocused pane requests attention. A 2 second throttle coalesces storms. With ` :: cmd args`, Kettle spawns the argv directly without a shell; `{0}` and `{1}` substitute capture values but cannot add arguments |
| `resize-overlay` (`resize_overlay`) | `always`\|`never`\|`after-first` | `after-first` | Show a centered `cols×rows` chip while resizing. `after-first` skips the initial window placement; `never` disables it |
| `theme-mode` (`theme_mode`) | `explicit`\|`light`\|`dark`\|`auto` (`system`/`follow-system`) | `explicit` | How the active theme is picked. `explicit` uses `theme`; `light`/`dark` force `light-theme`/`dark-theme`; `auto` follows the OS light/dark preference via winit when the platform reports one. If `theme-schedule` is set, the schedule owns the switch instead |
| `light-theme` / `dark-theme` | string | — (falls back to `theme`) | The two themes `theme-mode` switches between. Any bundled theme name (`kettle --list-themes`) |
| `theme-schedule` | string | — | Scheduled light/dark switch for `theme-mode = auto`; when present, it takes precedence over OS appearance following. Either two `HH:MM <role>` entries (`role` = `dark`/`light`), comma-separated — e.g. `19:00 dark,07:00 light` — or `auto` (aliases `sunrise/sunset`, `solar`) for sunrise/sunset (needs `theme-schedule-lat`/`-long`) |
| `theme-schedule-lat` / `theme-schedule-long` | float | — | Latitude `[-90, 90]` / longitude `[-180, 180]` for `theme-schedule = auto` sunrise/sunset. Out-of-range values are discarded (the schedule stays unset) |
| `allow-bold` | bool | `true` | When `false`, suppress the SGR bold attribute. Useful with fonts that have no bold face |
| `bold-is-bright` | bool | `false` | When `true`, bold text using a palette 0–7 color is remapped to the bright 8–15 variant (xterm convention) |
| `clear-select-on-copy` | bool | `false` | When `true`, the selection highlight is cleared right after a copy (some users prefer the selection to disappear once captured) |
| `invert-search` | bool | `false` | Flip the search bar's default step direction: `Enter` searches backward and `Shift+Enter` forward. Previous, Next, `Shift+F3`, and `F3` keep their literal directions |
| `search-wrap` | bool | `true` | Wrap from the last match to the first and back. When `false`, navigation stops at the buffer ends |
| `vim-menu-nav` | bool | `true` | Vim-style navigation in kettle's menus/overlays. List overlays (context menu, new-tab dropdown, settings) take `j`/`k` (wrapping), `g`/`G`, `Ctrl+d`/`Ctrl+u` (half page); in the context menu / new-tab dropdown `h` = back/close and `l` = drill in/activate, while in the settings panel `h`/`l` step the highlighted row's value (same as `←`/`→`); confirm dialogs take `y`/`n`; text-input overlays with a selection (palette, search, layout picker) use `Ctrl+j`/`Ctrl+k` (or `Ctrl+n`/`Ctrl+p`) so letters keep typing. Menu mnemonics skip the nav letters while enabled; type-to-search keeps working. `false` restores plain arrow-key navigation |
| `backspace-binding` | `ascii-del`\|`control-h`\|`escape-sequence`\|`auto` | `ascii-del` | Byte(s) the Backspace key sends. `ascii-del` = `0x7f` (the modern default); `control-h` = `0x08` for hosts expecting the old binding |
| `delete-binding` | `ascii-del`\|`control-h`\|`escape-sequence`\|`auto` | `escape-sequence` | Byte(s) the Delete key sends. `escape-sequence` = the standard `CSI 3~` |
| `login-shell` | bool | `false` | Launch the shell as a login shell (`-l`). Ignored for `wsl.exe` (where `-l` means "list distros" — see the WSL note below) |
| `shell-integration` | bool | `on` | Automatically load Kettle's integration when Windows chooses PowerShell or when `command` is exactly a bare `pwsh`, `pwsh.exe`, `powershell`, or `powershell.exe`. The user's `$PROFILE` loads first; Kettle then uses a bounded UTF-8 bootstrap. Arguments, wrappers, WSL, Unix/macOS shells, and other explicit commands remain exact and require the one-line installer from [SHELL-INTEGRATION.md](SHELL-INTEGRATION.md). `off` disables injection |
| `completion-overlay` | `auto`\|`off` | `auto` | Present shell-owned candidates in a detached card aligned with the command's first editable column and never at the cursor. Kettle prefers the lane above the command and flips the card below its final wrapped row when that shows more results. The card stays inside the active pane's terminal grid and never covers the command, pane title, or tab bar. If neither lane fits or the pane is narrower than 20 columns, no card is drawn. Wrapped input falls back to the first grid column. `auto` replaces only stock Fish or PowerShell bindings, never custom bindings, and stays off inside tmux/screen. Bash and Zsh provide cooperative helpers. The shell still owns matching and edits. Changes apply to new shells so an existing wrapper never becomes invisible mid-session |
| `term` | string | `xterm-256color` | The `TERM` value exported to the PTY |
| `colorterm` | string | `truecolor` | The `COLORTERM` value exported to the PTY (advertises 24-bit color) |
| `env` | `KEY=VALUE` | — | Repeatable; exports user environment variables to every spawned pane. Names must use portable env syntax (`[A-Za-z_][A-Za-z0-9_]*`), empty values are allowed, and later duplicates win. Kettle still owns terminal identity vars through `term`, `colorterm`, `TERM_PROGRAM`, and `TERM_PROGRAM_VERSION`; user vars are also appended to `WSLENV` for Windows → WSL launches |

### Scrollback search

`Ctrl+Shift+F` opens a responsive search bar at the bottom of the
window. Its controls are Editor, Previous, Next, Wrap, Case, Invert, and Close;
`search-wrap`, `search-case-sensitive`, and `invert-search` are also available
in **Settings → Search**. Changes made from either surface persist to the config.

Patterns use Rust regex syntax through `regex-automata`'s meta engine and are
limited to **4096 UTF-8 bytes**. A pattern that **fails to parse** is retried as
a literal, so searching for `call(x` or a bare `(` finds that text on screen
instead of reporting an error — most people typing into a search box are copying
something they can see, not writing a regex, and the bar offers no way to turn
regex off. A pattern that **does** parse keeps regex meaning, so `a|b` and `^row`
work as expected. The consequence is worth knowing: `call(x)` and `arr[0]` are
valid regex (a group and a character class), so they are matched as regex and
still will not find that literal text. Engine construction is bounded to a
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

Existing chunk progress is preserved while a PTY keeps producing output;
because rows can shift during that pass, a non-navigation scan is verified
again from a fresh viewport anchor after output has been quiet for 500 ms
before Kettle reports a definitive boundary or miss. If output interrupts an
explicit Previous/Next operation, that operation stays **Results limited**
until the user retries it; silently starting a default-direction quiet retry
would verify a different navigation request.

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
Signed grid coordinates keep negative scrollback rows valid. Only the active
and nearby matches are retained; the bar deliberately reports status rather
than an eager global match count. Search state belongs to the OS window, while
the last query is remembered per pane within that window.

`Enter` follows the configured default direction and `Shift+Enter` reverses it;
`F3` / `Shift+F3` are always Next / Previous. `Escape` closes the bar without
snapping the viewport away from the selected result. Search input is Kettle UI
chrome and is not sent to tmux, AstroNvim, Codex CLI, Claude Code CLI, or any
other program in the PTY.

### Auto light/dark theme switching

Set `theme-mode = auto` (or `system` / `follow-system`) with a light/dark
theme pair to follow the OS appearance preference when winit reports one:

```ini
theme-mode  = auto
light-theme = TokyoNight Day
dark-theme  = TokyoNight Night
```

Add `theme-schedule` when you want kettle to ignore OS appearance changes and
switch by time instead:

```ini
theme-mode     = auto
light-theme    = TokyoNight Day
dark-theme     = TokyoNight Night
# dark at 19:00, light at 07:00
theme-schedule = 19:00 dark,07:00 light
```

…or compute sunrise/sunset from your location:

```ini
theme-mode          = auto
light-theme         = TokyoNight Day
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

### Extended keys

Both kebab-case and underscore spellings are accepted for every key in this
table. For example, `show-titlebar` and `show_titlebar` are equivalent.

| Key | Type | Default | Notes |
|---|---|---|---|
| `window-state` | enum | `normal` | Launch state: `normal` \| `maximise` (`maximize`) \| `fullscreen` \| `hidden`. Honored by winit's `with_maximized` / `with_fullscreen` / `set_visible(false)` |
| `window-width` / `window-height` | int cells | unset | Fresh-window startup grid size. Width is clamped `[20, 400]`, height `[8, 200]`; if one side is omitted, the other uses Kettle's startup baseline (`100x36`). Session restore and explicit new-window geometry override this seed |
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
| `background-type` | enum | `solid` | `solid` \| `transparent` \| `image` \| `starfield` (v2.24.0 — a zero-config procedural GPU starfield; needs no `background-image`. A FIXED built-in example: its look is baked in, not tunable). Surfaced in Settings → Background. See **[BACKGROUNDS.md](BACKGROUNDS.md)** |
| `background-image` | path | — | Wallpaper image (for `background-type = image`). Supports PNG/JPEG/WebP/BMP/GIF, **animated GIF / APNG / animated WebP** (plays as a moving background — see `background-animation`). Tilde expansion supported. Editable inline in Settings → Background. Curated sources in **[BACKGROUNDS.md](BACKGROUNDS.md)** |
| `chrome-background` | enum | `theme` | When a wallpaper (`image` or `starfield`) is set, the opaque fill of the window chrome strips (tab bar, status bar) so the background never bleeds through them: `theme` (the theme's chrome color — default) \| `auto` (the background's average color, kept readable under the tab text; black over the starfield) \| `black` \| `white`. No effect without a wallpaper |
| `background-animation` | enum | `always` | How an animated background (a `starfield` or an animated `background-image`) plays: `always` (`on`/`true`, the v2.24.0 default — animate even when unfocused; still freezes when the window is minimized/occluded) \| `when-focused` (`focused`, animate only while focused, zero idle otherwise — battery-friendly) \| `off` (`static`/`false`, freeze on first frame). Surfaced in Settings → Background |
| `background-image-mode` | enum | `stretch_and_fill` | `stretch_and_fill` \| `tile` \| `center` \| `scale` (aspect-preserving fit) |
| `background-image-align-horiz` | enum | `center` | `left` \| `center` \| `right` (applies to `center` + `scale` modes) |
| `background-image-align-vert` | enum | `middle` | `top` \| `middle` \| `bottom` |
| `background-blur` | bool | `false` | CPU-side 3-pass separable box blur at decode (approximates Gaussian) |
| `background-darkness` | float 0..1 | `0.5` | Opacity of the terminal background over a transparent, image, or starfield backdrop. `0.0` is fully see-through; `1.0` hides the backdrop. Only applies when `background-type` is not `solid` |
| `inactive-color-offset` / `inactive-bg-color-offset` | float 0..1 | `1.0` / `1.0` | How far an unfocused pane recedes. Both are read with `unfocused-split-opacity`, and the strongest value wins. Kettle composites one overlay rather than reshaping each glyph |
| `exit-action` | enum | `close` | What happens when the shell exits: `close`, `hold` the dead pane, or `restart` the same argv and working directory in a new tab. Duplicate child-exit notifications are coalesced |
| `broadcast-default` | `all`\|`group`\|`off` | `group` | Scope enabled by the broadcast chord. `group` targets the active tab, `all` targets every pane in the window, and `off` prevents the chord from enabling broadcast. Kettle waits for the chord instead of starting a window in an input-mirroring state |
| `split-to-group` | bool | `false` | A new split joins the broadcast group of the pane it came from, so splitting a grouped pane widens the group instead of quietly dropping out of it |
| `autoclean-groups` | bool | `true` | Forget a broadcast group after its last pane closes. This prevents a stale group target from claiming a later pane that reuses the same name |
| `always-split-with-profile` | bool | `false` | Repeat the parent pane's direct launch command in a new split instead of opening a shell. Ordinary shell panes are cloned either way |
| `force-no-bell` | bool | `false` | Silence every bell path regardless of `bell`: visual flash, window attention, and the tab activity dot |
| `visible-bell` / `urgent-bell` | bool / bool | `—` | Legacy aliases for the unified `bell` key. Together they map to `both`; individually they map to `visual` or `attention`. An explicit `bell = …` setting wins regardless of file order |
| `log-strip-ansi` | bool | `false` | Strip ANSI escape sequences from the per-pane session log (`Action::ToggleSessionLog`) before writing. `true` → log is plain-text (CSI / OSC / single-char ESC all stripped); `false` → raw stream is preserved (`cat`-replayable in a terminal) |
| `light-theme` / `dark-theme` | theme name | `""` (falls back to `theme`) | Themes swapped by `Action::ToggleLightDark`. Lookup is case insensitive; an empty side falls back to `theme` |
| `search-case-sensitive` | enum | `smart` | `smart` ignores case until the query contains uppercase. `always` / `sensitive` forces matching case; `never` / `insensitive` always ignores it. `case-sensitive = true/false` remains accepted as an alias |
| `link-single-click` | bool | `false` | Single-click opens URLs (default needs `Ctrl`/`Cmd`+click) |
| `disable-mouse-paste` | bool | `false` | Block middle-click paste |
| `clipboard-paste-protection` | bool | `true` | Confirm multi-line pastes when any writable target would receive raw, non-bracketed paste. Single-line pastes and panes that enabled bracketed paste (editors/agent CLIs) paste immediately |
| `putty-paste-style` | bool | `false` | Right-click pastes instead of opening the context menu. By default it uses the same PRIMARY-first source as middle-click paste on Linux and falls back to the regular clipboard on platforms without PRIMARY |
| `putty-paste-style-source-clipboard` | bool | `false` | When `putty-paste-style = true`, source right-click paste from the regular system clipboard instead of X11 PRIMARY |
| `close-button-on-tab` | bool | `true` | Show `✕` on tab segments |
| `new-tab-after-current-tab` | bool | `false` | Insert vs append behavior when creating a new tab |
| `detachable-tabs` | bool | `true` | Allow cross-window tab tear-off / re-dock and the `move_tab_to_new_window` action. `false` keeps in-window tab switching and reordering but disables detach |
| `ask-before-closing` | enum | `multiple-terminals` | Close-confirmation policy: `always`, `multiple-terminals`, or `never`. Applies to the close-window, close-tab, and close-pane **actions** (`Ctrl+Shift+W`, `Ctrl+Shift+Q`, the tab-bar `✕`, middle-clicking a tab); panes sitting idle at an integrated-shell prompt do not count as work to lose. The **titlebar `✕` and `Alt+F4` are exempt** and close immediately: those are OS window-destroy requests rather than Kettle commands, and a terminal has no unsaved-document state to veto them with. Set `always` if you want them to ask too. Also on the right-click **Preferences ▸** menu as a radio group |
| `lua-sandbox` | enum | `safe` | Lua plugin trust level. `restricted` blocks `kettle.send_text` and `kettle.exec_action`. `safe` also removes command and file APIs from the Lua standard library, but a plugin can still run a command by typing it into the pane. `trusted` restores the command and file APIs (`os.execute`, `io.open`, `io.popen`, `loadfile`, `dofile`); the `debug` library and native C-module loading stay unavailable at every level because the VM is always mlua's safe state. Read any plugin before enabling it. See [`docs/examples/init.lua`](examples/init.lua) |

The automatically discovered `<config-dir>/init.lua` uses the same trusted
directory and leaf checks as the default config because even `safe` Lua can type
commands into the shell. A dotfile-manager link remains supported only when the
link itself has trusted ownership and one name and its resolved target passes
the same checks. `--lua-script FILE` is the explicit-path escape hatch for
project or shared scripts; it retains the 4 MiB bounded read and is a deliberate
trust grant, just as `--config FILE` is for configuration.

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

## Editing the config from inside kettle (Preferences submenu)

Most of the keys above can be toggled at runtime via right-click → **Preferences ▸**.
The submenu surfaces five common toggles + an `Advanced…` row that opens the
config file with the operating system's default app for everything else:

| Submenu row | Config key written |
|---|---|
| Scrollbar (radio: always/auto/hidden) | `scrollbar` |
| Cursor blink (✓) | `cursor-blink` |
| Copy on select (✓) | `copy-on-select` |
| Bell (radio: off/visual/attention/both) | `bell` |
| Mouse-hide while typing (✓) | `mouse-hide-while-typing` |
| Font size + / − | (live-only; `font-size` not auto-persisted yet) |
| Advanced… | opens `~/.config/kettle/config` with the default app |

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
`backspace` (alias `bs`), `delete` (alias `del`), and the symbolic
names `plus`/`minus`/`equal` for `+`/`-`/`=`.

GTK's accelerator syntax works too, unchanged from a Terminator config:
`<Primary><Shift>t` is the same binding as `ctrl+shift+t`.

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
`new_split:down` (also `split_down` / `split_horiz`), `split_auto`
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

**Window**: `new_window` (opens another window **in this process** since
v2.18 — tabs can move live between windows), `close_window`,
`toggle_fullscreen`, `move_tab_to_new_window` (tear the focused tab out
into its own window LIVE — running programs keep running; dragging a tab
past the tab bar does the same with the mouse, Chromium-style since
v2.19: the live window rides the pointer, and dropping it on another
kettle window's tab bar merges it there), `open_settings`
(`settings` — the Ctrl+, overlay), `layout_picker`, `about` (also
`show_about` — version, update status, GitHub link), `screenshot`
(`take_screenshot` / `terminalshot`).

**Editing**: `copy` (`copy_to_clipboard`), `paste`
(`paste_from_clipboard`), `select_all` (select the whole scrollback + screen;
in the command palette, bindable, no default chord), `select_to_top`
(`select_to_first_line`) and `select_to_bottom` (`select_to_last_line`) —
extend the selection to the top / bottom of the buffer, bound by default to
**Shift+Home** / **Shift+End** (Shift+click still extends to the click point).
Scroll-to-extremes moved to **Ctrl+Home** / **Ctrl+End** as a result.

**Search + jump**: `start_search` (`search`), `prev_prompt`
(`jump_to_prompt_prev`), `next_prompt` (`jump_to_prompt_next`).

**Scrollback**: `scroll_line_up`, `scroll_line_down`, `scroll_page_up`,
`scroll_page_down`, `scroll_to_top`, `scroll_to_bottom`,
`clear_history` (also `clear_scrollback` / `clear_buffer` — wipes
scrollback only; keep the visible screen unlike `reset`).

**Broadcast**: `broadcast_all` (type to every pane in the window),
`broadcast_off`, `broadcast_group` (type to every pane in the focused pane's
named group — **in every window**, since a group is a set you declared and
`group_all` already spans them; typing and pasting both follow it). The default
`Ctrl+Cmd+B` (macOS), `Ctrl+Shift+G` (Windows), or `Super+G` (elsewhere) chord
is `broadcast_tab`, which **toggles** —
pressing it again turns broadcast off, so you never have to reach for a second
chord to stop. Which scope it turns *on* is `broadcast-default`.

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
(swap between `light-theme` and `dark-theme`).

**Titles**: `edit_window_title`, `edit_tab_title`, `edit_pane_title` (open the
inline rename overlay for the OS window / active tab / focused pane).

**Modals + UI**: `command_palette` (`palette`), `hint_mode` (`hints` /
`quick_select`), `new_ssh` (`ssh`), `context_menu`
(`open_context_menu` — the right-click menu — mouse-only by default
but bindable to a keyboard trigger if you want the menu opened at the
cursor position).

**Vi-mode**: `toggle_vi_mode` (`vi_mode` / `vi`) — enter keyboard-driven
copy/navigation mode (default `Ctrl+Shift+Space`): `h`/`j`/`k`/`l` move,
`0`/`$`/`g`/`G`/`H`/`M`/`L` jump, `v` starts a visual selection, `y` yanks it to
the clipboard, `Esc` exits. See `man kettle` for the full keymap.

**Misc**: `reset` (RIS — full terminal reset including engine state),
`reload_config`, `detach_tab` (Unix-only cross-window tab tear-off),
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
