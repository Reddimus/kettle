# Changelog

All notable changes to kettle. Format roughly follows
[Keep a Changelog](https://keepachangelog.com/); the project moves in small,
durable, fully-tested cycles (lint · build · test · docs · commit · CI).

## [Unreleased]

### Documentation
- **`focused-split-color` row in CONFIG.md notes the broadcast-mode
  override.** Cycle 184 changed the focused-pane border to theme
  yellow when broadcast is on (the cycle-178 sibling indicator for
  single-tab / `tab-bar = auto` layouts). A user who'd configured
  `focused-split-color = #ff0000` and toggled broadcast on used to
  see the color "ignored" with no documented explanation. The
  CONFIG.md row now explains the temporary override — broadcast
  off restores the configured color. README's Terminator-
  multiplexing bullet gains a parenthetical for the indicator so
  the visual cue is discoverable before the user toggles broadcast
  blindly for the first time.

### Fixed
- **Theme filter rejects emacs autosave files (`#name#`).**
  Cycle 167's filter caught dotfiles (`.DS_Store`, `.gitignore`,
  `.#emacs-lock`) — but emacs's *unsaved-buffer* autosave is
  `#name#` (literal `#` on both sides, no leading dot). A
  maintainer editing a theme file in emacs and crashing leaves
  `#TokyoNight Night#` next to the real file, which the
  cycle-167 filter accepted as a theme. Add a leading-`#` skip:
  bundled themes never legitimately start with `#`, so the
  rejection is unambiguous. +2 asserts in the existing test.

- **Theme filter is case-insensitive for OS desktop metadata.**
  Cycle-167 follow-up. The bundled-theme filter's OS-metadata
  branch (`Thumbs.db` / `desktop.ini` / macOS `Icon\r`) used an
  exact-case `matches!`, while the editor-backup-suffix branch
  below it was already case-insensitive. NTFS is case-preserving
  but case-insensitive — a Windows checkout / Git Bash copy /
  robocopy mishap could land `THUMBS.DB` or `Desktop.ini` in the
  themes directory, slipping through the cycle-167 filter and
  surfacing as a phantom "THUMBS.DB" theme with garbage palette.
  Now both branches use the lowercased name. Test gains 4 more
  asserts (THUMBS.DB / Thumbs.DB / Desktop.ini / DESKTOP.INI).

- **`home_dir_fallback` caller now also gates on `is_dir`.**
  Cycles 162/180 made the helper probe HOME → USERPROFILE →
  APPDATA and filter empty values. But the caller fed whatever
  path the helper returned to `cmd.cwd` without checking it was
  actually a directory. A misconfigured `HOME=/etc/passwd`
  (exotic but possible — a script that set HOME to the wrong
  thing, or an env var pointing at a regular file) would have
  the OS spawn fail with "not a directory". Now: if the
  resolved home path isn't a directory, treat it the same as
  "no home" — leave `cmd.cwd` untouched and let `portable_pty`
  inherit kettle's launch directory (the same recovery as the
  no-env-var-set case). One-line `&& home.is_dir()` guard at
  the caller; helper stays pure. No new test (fs predicates
  aren't unit-testable without infrastructure the rest of the
  caller's tests don't stand up; correctness-by-construction
  via the helper's existing coverage + the new guard).

### Added
- **Focused-pane border tints yellow when broadcast is on.**
  Cycle-178 follow-up: the tab-bar accent flipped to yellow on
  broadcast, but with `tab-bar = auto` (the default) and only one
  tab open (the common single-window case), the tab bar is hidden
  and the cycle-178 indicator becomes invisible. The user could
  toggle broadcast on, forget about it, and lose track of where
  their keystrokes were going. This cycle adds a complementary
  per-pane indicator: when broadcast is on, the focused-pane
  border flips from `palette[4]` (theme accent blue, the standard
  focused-split color) to `palette[3]` (yellow, matching the
  cycle-178 tab-bar accent). Works regardless of `tab-bar` mode
  — even with the tab bar fully disabled (`tab-bar = off`) the
  user sees the visual cue. Inactive panes keep their normal
  divider color (broadcast is scoped to the active tab,
  cycle-112 invariant). No new test (render-time tint
  conditional, same pattern as cycle 178).

- **`clear_history` action — clear scrollback without resetting the
  terminal.** Every modern terminal exposes this (kitty
  `clear_terminal`, iTerm2 "Clear Buffer", WezTerm
  `clear_scrollback`). kettle's existing `reset` action is RIS
  (`\e c`) which clears the screen AND the engine state — bigger
  hammer than users want. The new `clear_history` action writes
  `CSI 3 J` (ANSI ED 3) to the focused pane, which clears the
  scrollback buffer only and leaves the visible grid intact.
  Aliases: `clear_history` / `clear_scrollback` / `clear_buffer`.
  Honors broadcast (cycle-173/174 invariant): when group input
  is on, every pane in the active tab clears its scrollback.
  Reachable via the command palette ("Clear scrollback") and
  bindable via `keybind = … = clear_history`. Unbound by
  default (the natural chord on most terminals — `Ctrl+Shift+L`
  — collides with the shell's form-feed; the user picks their
  own preferred chord). docs/CONFIG.md keybind grammar updated.

### Fixed
- **Drag-and-drop routes through bracketed paste like clipboard
  paste does.** Cycle-175 follow-up. The drop handler wrote the
  shell-quoted path bytes raw, even when the focused pane had
  `BRACKETED_PASTE` mode enabled (vim/neovim/fzf/mc default in
  modern setups). With brackets disabled the user got the path
  cleanly; with brackets enabled, the path bytes were *not*
  wrapped in `\e[200~ … \e[201~`, so vim treated each char as a
  normal-mode command — `'` opened a register selection, `:`
  entered command mode, the path digits hopped lines, etc. Now
  uses the same `input::paste_payload(text, bracketed)` helper
  that clipboard paste uses, with per-pane wrap when broadcast
  is on (cycle-174 invariant — a broadcast set containing one
  shell + one vim doesn't break either of them). Same chrome-
  wiring shape as cycles 173/174 — no new test, fix is correct-
  by-construction once it routes through the shared helper.

- **`XDG_CONFIG_HOME=""` no longer makes `default_path` return a
  *relative* path.** Cycle-180 sibling: same empty-env-var
  filter shape, applied to `Config::default_path`. Pre-cycle,
  the first arm read `var_os("XDG_CONFIG_HOME")` and produced
  `Some(PathBuf::from(""))` for an empty value — the final path
  became `"kettle/config"`, a relative path that resolves
  against whatever the current working directory happens to be.
  A user launching kettle from a directory that happened to
  contain a `kettle/` subdirectory could have kettle silently
  read a stray config file there instead of the user's real
  one — wrong config OR (worse, in a multi-user-shared CWD
  scenario) someone else's config. Fix: filter empty values
  in every arm of the XDG_CONFIG_HOME → HOME → APPDATA fallback;
  refactored as `default_path_from(lookup)` so the env-probe
  order + filter are unit-testable without mutating the process
  env. Test pins all four branches (XDG set / HOME fallback /
  APPDATA fallback / all-empty-or-unset → None). +1 test
  (243 total).

- **`HOME=""` (empty env var) is treated as unset in
  `home_dir_fallback`.** Cycle-162 follow-up. The cycle-162 fix
  introduced `home_dir_fallback` to probe HOME → USERPROFILE →
  APPDATA when the recorded session cwd no longer exists, so
  Windows users (whose `HOME` is unset) finally landed in their
  user profile instead of kettle's launch directory. But the
  probe used `var_os(k)` which returns `Some(OsString::new())`
  for an *empty* value (`HOME=""`) — a real shape in stripped-
  down CI containers, after a misconfigured `unset HOME` /
  `export HOME=` in a parent shell, or in custom Docker entry-
  points. The empty value flowed through to
  `CommandBuilder::cwd("")` which then handed the OS spawn an
  invalid empty path. Fix: filter empty values as if unset, so
  the probe continues to the next variable in the fallback
  chain. Test pins every level: HOME empty → USERPROFILE, both
  empty → APPDATA, all three empty → None (cmd.cwd left
  untouched). +1 test (242 total).

### Documentation
- **Hardcoded test-count claims removed from user-facing docs.**
  Cycle 172 caught `docs/TESTING.md`'s "213 tests as of cycle 128"
  drift (wrong by 40+ cycles); this cycle catches the matching
  stragglers — `docs/ARCHITECTURE.md` claimed "117 workspace
  tests" (wrong by 120+) and `docs/INSTALL.md` claimed "213 tests"
  in its build-verification snippet. Both reworded to range-
  stable phrasing ("an extensive workspace test suite" / "230+
  tests"). The cycle-172 drift guard now also flags any future
  `<digit> workspace tests` / `<digit> tests across` substring in
  the user-facing markdown set, so the next time someone hardcodes
  a count it fails CI instead of going stale silently.

### Added
- **Visual indicator when broadcast (group-input) mode is on.**
  Pre-cycle, toggling broadcast via Ctrl+Shift+G (or the command
  palette) flipped the input-routing flag with no UI cue — every
  keystroke went to every pane in the active tab, but the user
  had no way to tell at a glance. Cycle 173/174 sealed up the
  broadcast scoping (keystrokes / scroll-on-keystroke / paste);
  this cycle adds the obvious missing piece — a warning-yellow
  accent (theme palette[3]) on the active tab segment's left
  edge when broadcast is on. Inactive tabs stay normal (broadcast
  is scoped to the active tab; cycle-112 invariant). No new
  config key: uses the theme's standard ANSI yellow slot so it
  works automatically with every bundled theme. No new test
  (render-time tint; the conditional is a 4-line if/else read
  from `tabbar.broadcast`).

### Fixed
- **Session restore canonicalizes the theme name the same way parse
  does.** Cycle-176 sibling. The session.json file holds whatever
  theme name was current at save time. A session written by a
  pre-176 kettle could hold a typo'd or all-lowercase name (e.g.,
  the user wrote `theme = tokyonight night` in their config and
  the pre-176 parser stored it verbatim, then save_session wrote
  that lowercase form). On restore, the pre-cycle code re-stored
  the lowercase name in `cfg.theme_name` while `Theme::by_name`
  resolved the right palette case-insensitively — so the runtime
  used TokyoNight Night's palette but `--check-config` (on the
  next reload) would have echoed the lowercase form. Route the
  restore through `Theme::find_name` (the cycle-176 helper) so
  the same canonicalization the parse path uses applies to the
  restore path too. No new test (existing `find_name` coverage
  + the existing session-restore integration smoke). Same
  cycle-shape as 173/174 — sibling chrome-wiring fix that extends
  a prior cycle's invariant to one more code path.

- **`kettle --check-config` now prints the *actual* theme name in
  use, not the user's typo.** Pre-cycle, `parse_collect` did:
  ```rust
  cfg.theme_name = e.value.clone();      // typo stored verbatim
  cfg.theme = Theme::by_name(&e.value);  // silent fallback to default
  ```
  So a user writing `theme = TokyoNitght Night` (typo) had
  `--check-config` print `theme: TokyoNitght Night` while the
  runtime palette was actually TokyoNight Night's defaults. Same
  docs/runtime mismatch shape as cycle 139 (font-size clamp).
  Now: store the *canonical* bundled name (with original casing)
  when the lookup matches; leave `theme_name` at the prior
  default when it misses. Bonus: `theme = tokyonight night`
  (all-lowercase) now produces `theme_name = "TokyoNight Night"`
  (canonical casing) — was lowercase before. The malformed-value
  diagnostic still flags the typo separately so the user sees
  their mistake. New `Theme::find_name` companion to `by_name`.
  Test: `theme_name_matches_the_actually_loaded_palette` pins
  case-insensitive→canonical, typo→default-name, and the
  diagnostic-still-flags assertion. +1 test (241 total).

### Added
- **Drag-and-drop files.** Dropping a file onto the kettle window
  inserts its shell-quoted path at the cursor of the focused pane
  (or broadcasts to every pane in the active tab when group input
  is on). A trailing space is appended so the common workflow —
  type `cat `, drop a log file, press Enter — Just Works. POSIX-
  style single-quote escaping (close-escape-reopen for embedded
  apostrophes) so the same form works on bash / zsh / fish /
  PowerShell 7+. iTerm2 / WezTerm / kitty / Ghostty / GNOME
  Terminal all have this affordance. Test:
  `shell_quote_path_handles_spaces_quotes_and_multibyte` pins
  spaces, apostrophe escaping (single + repeated), multibyte
  paths, and empty input. +1 test (240 total).

### Fixed
- **Paste distributes to every pane in a broadcast group, not just
  the focused pane.** Cycle 173 sibling. With broadcast on
  (Ctrl+Shift+G group-input mode), keystrokes go to every pane in
  the active tab — paste is also user input and should follow the
  same scoping. Pre-cycle, Ctrl+Shift+V (or middle-click) wrote
  only to the focused pane regardless of broadcast state, so a
  user who'd just turned on broadcast to send the same command to
  three SSH sessions saw it work for typing but silently single-
  target for paste. New `Mux::broadcast_paste(text)` reads each
  pane's `BRACKETED_PASTE` mode separately and wraps the bytes
  per-pane (panes can disagree — e.g., one is in vim, one is at
  a shell prompt — and wrapping the wrong way would either
  inject literal `\e[200~`/`\e[201~` markers into the shell or
  leave bytes vulnerable to the paste-injection attack inside
  vim). Same active-tab scoping as `broadcast_write` and
  `broadcast_scroll_to_bottom` (cycle-112 leaf_ids invariant).
  Chrome-only, no new test (PTY-mode reads aren't unit-testable
  without infrastructure the rest of the mux tests don't stand
  up — same rationale as cycle 173 / 151).

### Fixed
- **`scroll-on-keystroke` (default `true`) now applies to every
  pane in a broadcast group, not just the focused pane.** The
  config flag says "snap the viewport back to the bottom on every
  keystroke" — meant to keep the user's view of incoming output
  current. With broadcast off, only the focused pane is written
  to and only it snaps; self-consistent. With broadcast on (the
  Ctrl+Shift+G group-input mode where typing goes to every pane
  in the active tab), the pre-cycle code wrote the bytes to all
  panes but skipped the snap entirely — so a user with broadcast
  on AND any pane scrolled back saw their typing reach the remote
  shells fine while the local view of those panes stayed pinned
  to history (no way to tell from the screen that the bytes
  actually went through). Fix: new
  `Mux::broadcast_scroll_to_bottom` companion to `broadcast_write`,
  same active-tab scoping (cycle-112 invariant); called from the
  same `scroll_on_keystroke` gate. No new test — the scoping
  matches `broadcast_write`'s, which is pinned by the cycle-112
  `leaf_ids` test; the snap itself requires a real Term lock that
  the existing mux unit tests don't stand up. Same shape as
  cycle 151 — chrome-only fix, correctness-by-construction.

### Documentation
- **User-facing docs no longer leak internal `cycle N` references.**
  Cycle 168 caught the audit-trail leak in `kettle --help`; this
  cycle extends the cleanup to the markdown docs the README links
  to. `docs/CONFIG.md` had two stragglers (`(cycle 138)` next to
  the bool-alias prose, `(cycle 163)` next to the modifier-typo
  rejection rule) — same mysterious-parenthetical UX issue as
  `--help`. `docs/TESTING.md`'s lead now says "230+ tests" instead
  of a specific cycle-number snapshot ("213 tests as of cycle 128")
  that's been wrong for 40+ cycles; the per-crate counts below
  remain order-of-magnitude. Regression test
  `user_facing_docs_have_no_internal_cycle_refs` reads README.md,
  docs/CONFIG.md, docs/INSTALL.md and scans for the pattern
  `cycle <digit>` — same drift-guard shape as cycle 168 for the
  CLI surface, but for the user-facing markdown surface.
  TESTING.md / ROADMAP.md / CONTRIBUTING.md are intentionally
  exempt (contributor-leaning, cycle refs serve as CHANGELOG
  anchors). +1 test (239 total).

### Internal
- **Two more blink-reset sites route through `reset_blink_phase()`.**
  The cycles 134-141 + 144 + 150 audit landed a shared
  `reset_blink_phase()` helper, but two callers still inlined the
  same two field writes (`blink_on = true; last_blink = now()`):
  `WindowEvent::Focused` and `WindowEvent::KeyboardInput`. A future
  change to the reset semantics (e.g., also clearing a `blink_pause`
  field if one's added) would need to touch every call site —
  routing through the helper keeps all eight user-driven blink-reset
  paths (Reset, focus changes, modal close, typing, tab close,
  window focus, DEC ?12 toggle, mouse focus) in lock-step. The one
  inline that remains is `CursorBlinkingChange`, which runs inside
  `self.mux.panes.values_mut()` and can't borrow `self` again —
  that one's documented in place. No behavior change; 238 tests
  still pass.

### Fixed
- **`kettle --list-keybinds` renders `Ctrl+Plus` / `Ctrl+Minus` /
  `Ctrl+Equal` for the punctuation keys, not the literal-`+`
  ambiguity of `Ctrl++` / `Ctrl+-` / `Ctrl+=`.** `Trigger::label`
  was uppercasing every `Char(c)` and joining with `+`, so the
  default zoom-in binding (Ctrl++ for the `+` key) showed up as
  `Ctrl++` — two adjacent `+` make it unclear whether the second
  one is the separator's repetition or the key itself. Same
  shape for `Ctrl+-` (zoom out: looks like a trailing dash) and
  `Ctrl+=` (also zoom in: looks like an assignment). The parser
  already accepts `plus` / `minus` / `equal` as named-key tokens
  (the same way the user would type them in their config file);
  the label now mirrors that convention so the row reads
  `Ctrl+Plus  IncreaseFontSize` and a user copying it back into
  their config file works without translation. Both kitty and
  Ghostty render these as `Plus`/`Minus`/`Equal` for the same
  reason. Test: pins the three named-token labels + two
  unaffected punctuation chars (`,` `/`) + plain letter
  regression. +1 test (238 total).

- **`font-feature = LIGA on` (uppercase tag) now actually toggles
  ligatures.** OpenType feature tags are case-sensitive per spec —
  every standard tag is lowercase (`liga`, `clig`, `calt`, `cv01`,
  `ss05`…). `FontFeature::parse` was storing whatever case the user
  typed, so `LIGA on` had two silent failures: (1) `is_ligature()`
  returned false because it only matched lowercase `liga`/`clig`/
  `calt`/`dlig`, so the coarse `cfg.font_ligatures` flag stayed
  stale and downstream code thought ligatures were still on; (2)
  the uppercase tag was passed verbatim to the cosmic-text /
  harfbuzz shaper, which uses a case-sensitive lookup and silently
  ignored the unknown `LIGA` tag. Net effect pre-fix: the user's
  `LIGA on` did nothing visible — ligatures didn't toggle, the
  feature didn't apply.
  Fix: `FontFeature::parse` lowercases the tag bytes before
  returning. Both the `is_ligature()` check and the FeatureTag
  passed to the shaper now see the canonical form. Test:
  `font_feature_tag_is_lowercased` pins uppercase / mixed-case
  inputs and asserts the downstream `cfg.font_ligatures` flag
  toggles the same way it would for lowercase. +1 test (237 total).

- **`kettle --help` no longer leaks internal cycle references, and
  `--config` documents the cycle-164 directory rejection.** The
  rustdoc-style doc comments for `--list-keybinds` and `--config`
  carried internal audit trail like `(cycle 103)` and
  `(cycle 106)` — useful for me reading the source, mysterious
  parentheticals when a user runs `kettle --help` in a real
  terminal. The `--config` description also still said "non-existent
  path is a hard error" with no mention that cycle 164 extended the
  check to reject directories too (typing `--config ~/.config/kettle`
  when you meant `.../kettle/config` is now a hard error, not a
  silent fallback to defaults).
  Fix: rewrote both doc comments to describe the *user-facing*
  behavior in plain English, dropping the cycle refs (the audit
  trail lives in code comments and CHANGELOG, where it belongs).
  Added a regression test that walks every clap `Arg`'s help and
  long-help (plus the top-level about/long-about) and asserts none
  contain the substring `"cycle "` — same shape as the cycle-116
  `defaults_has_no_shadow_collisions` drift guard, but for the
  CLI's user-facing surface. +1 test (236 total).

- **Bundled-theme filter is robust to OS/editor junk in
  `assets/themes/`.** `build.rs` skipped only the exact filenames
  `LICENSE` and `README.md`. A maintainer cloning the repo on
  macOS and opening the themes folder in Finder would pollute the
  bundled theme list with a `.DS_Store` "theme" whose contents are
  binary garbage — and the count is publicly surfaced
  (`kettle --list-themes`, README highlights). Same shape for a
  Windows checkout with `Thumbs.db`, an Emacs swap file, or
  `.swp`/`.bak`/`*~` backup files left over after editing a theme.
  Fix: extracted `is_bundled_theme_filename(name) -> bool` into a
  small `theme_filter` module the lib and `build.rs` share via
  `include!`. The filter rejects dotfiles
  (`.DS_Store`/`.gitignore`/`.directory`/etc.),
  desktop-metadata files (`Thumbs.db`/`desktop.ini`/macOS
  `Icon\r`), and editor backup-file suffixes (`~`/`.bak`/
  `.orig`/`.swp`/`.swo`/`.tmp`, case-insensitive). +1 test
  pinning all of the above plus four real theme names that must
  still pass (235 total).

- **Autodetected Wikipedia / Apple-docs / MDN URLs that legitimately
  end in `)` now stay clickable.** Both `links.rs` (the runtime
  hyperlink overlay) and `hints.rs` (`Ctrl+Shift+H` quick-select)
  had their own private `trim_trailing` that stripped *every*
  trailing `)` / `]` / `}` along with the other prose punctuation.
  A URL like `https://en.wikipedia.org/wiki/Foo_(bar)` was trimmed
  to `https://en.wikipedia.org/wiki/Foo_(bar` — a different
  (404) page. Same shape for any URL ending with a closing bracket
  used for disambiguation.
  Fix: shared `kettle_core::url_trim::trim_trailing` that
  bracket-balance-aware-strips: a `)` / `]` / `}` is removed only
  when the candidate substring has *more* closes than opens of the
  matching pair. So `..._(bar)` keeps its bracket (1 open + 1
  close = balanced), but `https://rust-lang.org)` from a
  `(https://rust-lang.org).` excerpt loses it (0 opens + 1 close =
  unbalanced) — both cases the user actually wants. Operates at
  byte level so multi-byte chars in IRI-ish URLs are passed through
  verbatim. +5 tests pinning sentence-punctuation, balanced-keep,
  unbalanced-strip, multi-byte-untouched, and an empty-input
  no-op (234 total).

- **`kettle --list-keybinds` columns line up again — even for the
  three default rows whose triggers exceed 16 chars.** `describe()`
  hard-coded the trigger column at 16 chars, so `Ctrl+Shift+PageDown`
  (19 chars; move-tab-right) and `Ctrl+Shift+PageUp` (17 chars;
  move-tab-left) overflowed the padding and their action column
  landed one or three bytes to the right of every other row.
  Visually jarring on the one CLI command whose purpose is making
  the keymap scannable. Fix: column width = max(16, longest
  trigger label) — same shape as `format_ssh_hosts` (cycle 105).
  Test pins the alignment contract: byte `longest+1` is the
  separator's second space and byte `longest+2` is the first
  action char on every row. +1 test (229 total).

- **`--config DIR` is now a hard error instead of a silent
  fallback-to-defaults.** Cycle 106 made `--config` fail when the
  path didn't exist. The matching "exists but isn't a regular file"
  case (typically a directory — a user typing `--config ~/.config/kettle`
  intending the file `~/.config/kettle/config`) wasn't covered. The
  path passed the existence check, `read_to_string` returned an
  `IsADirectory` error, `load_from_with_diagnostics` logged a
  `warn`-level message most users miss, and downstream branches used
  the default Config — the user saw the same "my theme is gone"
  symptom as the cycle-106 no-such-file case but with no obvious
  CLI-surface error to point at. Fix: hard-fail with
  `--config PATH: not a regular file` when `p.exists() && !p.is_file()`,
  mirroring the existing `--working-directory` shape (cycle 107).
  Extracted as a pure `config_path_problem(&Path) -> Option<&str>`
  helper so the truth table (missing / dir / regular file) is
  unit-testable without spawning the binary. +1 test (228 total).

- **Keybind modifier parsing recognizes `win`/`windows`/`meta`/`logo`
  as Super aliases, and *rejects* typo'd modifier names outright.**
  Before cycle 163, `parse_trigger` only knew `super`/`cmd`/`command`
  for the Super key — a user copying `keybind = win+t=new_tab` from
  their Windows config (or `meta+x` from a Linux config) silently
  saw the `win`/`meta` token fall to the `parse_key(other)` catchall,
  which returned None, then the parser kept iterating, so `key`
  ultimately landed on the *plain key* token (`t`/`x`). Result: every
  press of `t` in the terminal opened a new tab. Any typo'd modifier
  (`cttrl+t`, `supre+t`) had the same shape. Fix: extend the Super
  alias set (super / cmd / command / win / windows / meta / logo —
  the names every OS/WM/Qt ecosystem uses for the same key), AND
  make `parse_trigger` strict so a non-modifier in any but the
  last `+`-separated slot returns None. `--check-config` already
  gates triggers via `parse_trigger.is_some()`, so the rejected
  line now surfaces as a malformed-value diagnostic instead of
  becoming a "secret" plain-key binding stomping on normal typing.
  Test: pinned all seven Super aliases + multi-modifier chord +
  three typo rejections (`cttrl`, `contorl`, `supre`) + bare-`f5`
  regression. +1 test, docs/CONFIG.md updated.

- **Stale-cwd fallback now works on Windows too (and on stripped-down
  Linux containers).** When a saved session's recorded pane cwd no
  longer exists on disk — user moved the repo between launches, or
  the `-d` arg pointed at a since-deleted directory — kettle falls
  back to the OS home directory before letting `portable_pty` spawn
  the shell. The previous code only consulted `HOME`, which is unset
  on Windows by default: stale-cwd Windows users silently ended up
  in whatever directory they happened to launch kettle from
  (typically `C:\` from a Start-menu shortcut). Now
  `home_dir_fallback(lookup)` probes `HOME` → `USERPROFILE` →
  `APPDATA` in order, so all three platforms (Linux, macOS, Windows)
  converge on the same "user-home" intent. Same shape as
  cycle 159's macOS universal2 fix — Linux+macOS worked, Windows
  didn't, the env var probe order was the difference. The helper
  takes a `lookup` closure so its order can be unit-tested without
  mutating the real process env (which would race with the rest of
  the suite). Test: pinned truth table across HOME-set, USERPROFILE-
  only, APPDATA-only, and empty-env. +1 test (226 total).

- **OS mouse cursor is now the standard arrow over the tab bar and
  modal overlays (not the text I-beam).** `sync_cursor_icon` only
  considered two states — `Pointer` while a Ctrl-held URL was under
  the mouse, and `Text` everywhere else — so hovering the clickable
  tab bar, scrollbar-thumb-adjacent area, or any open modal (search
  bar, command palette, hint mode, SSH launcher) showed the I-beam,
  visually implying "this surface accepts text selection" when those
  surfaces don't. The fix extracts a pure
  `chrome_cursor_icon(in_tab_bar, modal_open) -> Option<CursorIcon>`
  helper that returns `Some(Default)` for chrome and `None` for
  content (the caller's existing Pointer/Text branch then runs),
  plus a new `any_modal_open()` reader on `App` that mirrors
  `close_all_modals`. iTerm2 / WezTerm / Ghostty / kitty all show
  the standard arrow over their chrome — this brings kettle in
  line. Test: the truth table of all four (in_tab_bar × modal_open)
  states pinned in `app::tests::chrome_cursor_icon_overrides_only_for_chrome`.

### Documentation
- **`CONTRIBUTING.md` documents the audit-cycle pattern.**
  After 150+ cycles the project has a distinctive workflow
  (find a bounded silent-fallback bug → extract a pure
  helper → wire it in → pin the contract with a test → land
  behind the full gate) that's hard to reverse-engineer
  from the CHANGELOG alone. New top-level file walks through
  the cycle shape, lists project layout, gives a real
  recent example (cycle 151's notify-filter fix), and points
  newcomers at `_ => {}` arms / the ROADMAP "Next" list as
  starting points. README's documentation section links to
  it.

### Build
- **macOS release builds are now actually universal (`x86_64` +
  `aarch64`).** The release workflow's artifact has been named
  `kettle-macos-universal.zip` since the project's first
  tagged release scaffolding, but the underlying binary was
  whatever single architecture `macos-latest` happened to be
  (currently arm64, but historically x86_64). An Intel-Mac
  user downloading the "universal" archive got a binary
  their CPU couldn't run; an Apple-Silicon user got a
  potentially-Rosetta-translated x86_64 binary, slow and
  unnecessary. Now the workflow:
  - Adds both `x86_64-apple-darwin` and `aarch64-apple-darwin`
    targets to the toolchain.
  - Builds release artifacts for each.
  - Combines them with `lipo -create` into a single
    universal2 binary at `target/release/kettle`.
  - The existing `.app` packaging step copies that universal
    binary unchanged.
  Linux and Windows still do the native single-arch build.

### Fixed
- **`--check-config` no longer flags empty values as malformed.**
  parse.rs documents the "empty value resets the key"
  semantics; cycle 121/122 made the runtime honor it
  explicitly for string keys, and the bool / enum / numeric
  arms naturally fall through to defaults on empty. But
  `detect_malformed_values` still tried to validate the
  empty string against each per-key contract, surfacing
  diagnostics like `malformed value: theme = ""` while the
  runtime quietly used the default. Disagreement.
  Now a single empty-value skip at the top of the per-key
  match handles every key uniformly. Diagnostic surface
  agrees with runtime — empty means "use default, no
  warning needed." +1 test covers theme / font-family /
  cursor-style / cursor-style-blink / bell / scrollbar /
  font-size / background-opacity all on empty plus a real
  typo regression guard.

### Fixed
- **Tab-close-via-middle-click and `Action::CloseWindow`
  save the session before exit.** Two exit paths were
  missing the save_session call that every other exit path
  already had (Action::CloseTab on the last tab, Action::
  ClosePane closing the final pane, WindowEvent::Close
  Requested via the OS window-X button). Result: a user
  middle-clicking their last tab or hitting `Ctrl+Shift+Q`
  (CloseWindow) exited kettle without persisting the
  now-empty session. On next launch, the *previous* multi-
  tab state from before the close still sat in
  session.json and silently restored — the user expected
  a fresh start, got their old layout back. Both paths now
  save before `event_loop.exit()`, matching the other
  exit handlers.

### Fixed
- **`detect_malformed_values` also strips a leading UTF-8 BOM.**
  Sibling to cycle 155. The cycle-155 strip lived only in
  `parse::parse`; `detect_malformed_values` does its own raw
  text scan for missing-`=` lines (cycle 96) and would still
  surface `missing `=` separator: "\u{feff}font-family"` on
  a BOM-prefixed config with a typo on the first line —
  invisible character mangled the diagnostic. Now the same
  one-line `strip_prefix('\u{feff}')` is applied here too.
  +1 test (`detect_malformed_values_strips_bom_before_scanning`)
  covers the missing-= + BOM combination and confirms a
  clean BOM-prefixed config isn't flagged.

### Fixed
- **Config parser strips a leading UTF-8 BOM.** Notepad and
  a few Windows editors save UTF-8 text files with a leading
  byte-order mark (`\u{feff}`, 0xEF 0xBB 0xBF). Without
  stripping it, the first config line parsed as `\u{feff}theme
  = …` and the BOM-prefixed key surfaced as an
  `unknown key: ﻿theme` in `--check-config` — invisible
  character making the diagnostic look bizarre, and the
  user's theme setting silently didn't apply. The parser now
  drops the BOM if it's at byte 0; a `\u{feff}` mid-file is
  not a BOM and stays in the value. +1 test
  (`strips_leading_utf8_bom`). Verified end-to-end against a
  `printf '\xef\xbb\xbftheme = ...'` fixture: status now
  reads `OK — no issues`.

### Fixed
- **Opening a modal closes any other modal first.** A user
  with the SSH launcher open who pressed `Ctrl+Shift+K` got
  both the SSH launcher AND the command palette rendered
  on top of each other, with the palette capturing keys
  (because the input dispatch checks hint → palette → ssh
  → search, first-open-wins). Visually confusing — the
  user couldn't tell which modal would receive their next
  keystroke without trying. Now `StartSearch`, `OpenSsh`,
  `CommandPalette`, and `HintMode` all call a new
  `close_all_modals()` helper before opening their own
  state. Extracted from cycle 111's `Action::Reset` sweep
  so both share one implementation.

### Fixed
- **Workspace `repository` URL points at the actual repo.**
  `Cargo.toml`'s `[workspace.package].repository` said
  `https://github.com/kevim/kettle` — but the actual repo
  has been `https://github.com/Reddimus/kettle` from the
  start. Stale metadata that affects: any future
  `cargo install kettle`, crates.io listings if published,
  any tooling that scrapes the Cargo.toml for an upstream
  URL. Other docs (INSTALL.md's `git clone …`) already
  had the correct URL.

### Fixed
- **Session restore agrees with `Theme::by_name` on case.**
  The session-restore branch checked `Theme::list().contains
  (&name)` (case-sensitive verbatim string match) before
  applying a stored theme, but `Theme::by_name(name)` is
  case-insensitive (cycle 0). A session written by an older
  kettle build, or hand-edited, holding a lowercase theme
  name (`tokyonight night`) would fail the verbatim
  `contains` check and stay on the default theme — even
  though `by_name` would have happily resolved it. Now
  the check uses `iter().any(|n| n.eq_ignore_ascii_case
  (name))` so the gate agrees with the apply.

### Fixed
- **Live config reload no longer fires on unrelated file
  events.** The `notify` watcher watched the config file's
  *directory* (NonRecursive) and reloaded on every event.
  Cycle 109's atomic session save writes
  `session.json.tmp.<pid>.<nanos>` then `rename`s it into
  place — each save fires 3+ notify events
  (create-temp / write-temp / rename), all of which used
  to pointlessly trigger a config reload. Editor swap
  files (`.config.swp`), theme caches, the user's own
  `vim` editing some other file in `~/.config/kettle/` —
  same story. Filter now matches `event.paths` against the
  watched config file specifically, so only edits to the
  config file itself cause a reload. No behavior change
  for the intended path (user edits config in any editor;
  notify fires for the config file; we reload).

### Fixed
- **DEC ?25l (hide cursor) is respected even when the window
  is unfocused.** The renderer's `draw_cursor` gate was
  `shape != Hidden && cp.line.0 >= 0 && pv.focused` — missing
  the `cursor_visible` flag. So when a TUI (vim, less, fzf,
  etc.) sent `\e[?25l` to hide its cursor and the user
  clicked away, the *unfocused-window hollow outline* still
  drew. Cursor was supposed to be invisible; it wasn't. The
  shape-based Hidden variant was correctly excluded, but
  DEC ?25 is a separate mode and routed through a different
  flag, so the bug only fired on the `shape != Hidden &&
  cursor_visible == false` combination — i.e. any program
  using `printf '\e[?25l'` rather than DECSCUSR `q`. Now
  the `cursor_visible` flag also gates `draw_cursor`, so a
  hidden cursor stays hidden in both focused and unfocused
  states, and across all DECSCUSR shapes (Block / Underline
  / Beam / HollowBlock).

### Fixed
- **`--screenshot` PNG honors `background-opacity` too.**
  Cycle 148 fixed the live-window path's clear-op alpha
  (`a: cfg.background_opacity`) and surface alpha-mode
  selection. The screenshot path's clear op still hardcoded
  `a: 1.0`, so `kettle --config /transparent.conf --screenshot
  out.png` produced an opaque PNG regardless of what the
  user asked for. PNG output is RGBA8 and the alpha channel
  is stored verbatim — honoring the config makes the
  screenshot match what the live window shows. Verified
  end-to-end: an `--screenshot` at `background-opacity = 0.5`
  produces a noticeably larger PNG (alpha varies across
  pixels) than the same shot at `1.0` (flat 0xff alpha).

### Fixed
- **`background-opacity` actually produces transparency.**
  Real bug. The old surface config used
  `alpha_mode: caps.alpha_modes[0]` — i.e. whatever the
  backend listed first, which on most platforms is
  `Opaque`. The `wgpu::Color { a: cfg.background_opacity }`
  on the clear op then had its alpha channel discarded by
  the surface composite, so `background-opacity = 0.5`
  rendered as fully opaque. A user setting transparency
  for a desktop-blur effect saw no difference between
  `1.0` and `0.5`. Now when `background_opacity < 1.0` we
  prefer `PreMultiplied → PostMultiplied → Inherit →
  Auto` (the standard alpha modes for compositing),
  falling back to whatever the backend lists first only
  if none of those are supported. Opaque configs are
  unchanged. Headless smoke still passes.

### Fixed
- **`Action::from_name` is now case-insensitive.** Same pattern
  as cycle 146's enum-key fix, applied to keybind action names.
  A user writing `keybind = ctrl+shift+c = Copy` (capitalized)
  used to silently drop the binding — `from_name` returned
  None on the unrecognized case variant, and `apply_keybind`'s
  silent-skip path swallowed it. `--check-config` flagged it
  via cycle 85, but the runtime didn't bind anything. Now
  lowercased (and whitespace-trimmed) before matching, so
  `Copy` / `COPY` / `copy` / `  paste  ` all resolve. The
  parametric `GOTO_TAB:1` form also works. +1 test
  (`action_from_name_is_case_insensitive`).

### Fixed
- **Enum config keys are now case-insensitive.** Cycle 138
  made the bool keys case-insensitive via `parse_bool`.
  The six enum keys (`bell`, `osc52`/`clipboard`, `tab-bar`,
  `tab-bar-position`, `scrollbar`, `cursor-style`) still
  matched `e.value.as_str()` verbatim, so `bell = OFF`
  silently fell into the catchall (→ `BellMode::Both`,
  the default) while `--check-config` flagged the same
  spelling as malformed. Both surfaces now lowercase
  before matching, so case variants validate and apply
  the same way as the canonical lowercase forms. +1 test
  (`enum_keys_are_case_insensitive`) covers all six keys
  with uppercase / mixed-case variants and confirms
  `--check-config` no longer flags them.

### Changed
- **`kettle --list-themes` is now case-insensitive alphabetical.**
  The build-script's pre-cycle sort was raw `String::cmp`, which
  is ASCII-bytewise: uppercase letters (0x41..0x5A) sort before
  lowercase (0x61..0x7A), so `CGA` came before `branch` because
  `'C' < 'b'` in ASCII. Skimming the 512-theme list was harder
  than it needed to be — users expect mixed-case alphabetical
  (matching what GNU `sort` does in a UTF-8 locale). New sort:
  `to_lowercase()` primary, original cmp tiebreak. End-to-end:
  `branch` now precedes `Calamity`; `CGA`/`Chalk` interleave
  with lowercase c-themes naturally. Also affects the order
  the `next_theme` / `prev_theme` chord cycles in.

### Fixed
- **Closing a tab via middle-click or ✕ also resets blink
  on the now-active tab.** Cycle 120's `reap_tabs` fix
  keeps `mux.active` pointing at the same tab the user was
  on when an *unfocused* tab closes; when the *focused*
  tab closes, focus naturally falls on a neighbor (matching
  every modern terminal). Either way the cursor lands on a
  potentially-different pane, and pre-cycle-144 that
  pane's cursor could be invisible for up to one
  `blink_interval` depending on the blink-timer phase.
  The tab-bar middle-click and ✕-click paths now snapshot
  `focus_key()` before the close and call
  `note_focus_change(pre)` after — same shape as cycles
  135/136's keyboard-and-pane-click paths. The last
  user-driven focus path that hadn't picked up the
  cycle-134→141 blink-reset pattern.

### Documentation
- **`docs/CONFIG.md` documents bool aliases, numeric clamps,
  and the `beam` cursor-style alias.** The bool-row entries
  just said `bool` with no hint that "yes" / "no" / "off" /
  "on" / "0" / "1" / "enabled" / "disabled" are also accepted
  (cycle 138). Numeric-range clamps (cycles 118/131/132/133)
  were never mentioned in the docs even though they affected
  user-facing behavior. The `beam` alias (cycle 142) wasn't
  in the cursor-style row. Added a "Type notes" preamble
  that documents all three concerns and updated the
  cursor-style row's value list.

### Added
- **`cursor-style = beam` accepted as an alias for `bar`.**
  Alacritty's config calls the vertical-stroke cursor
  `Beam`; kettle's enum calls it `Bar`. A user copying their
  Alacritty config got a silent fallback to `Block` plus a
  `--check-config` malformed-value warning. Now `beam`
  parses to `CursorStyle::Bar` directly and
  `detect_malformed_values` no longer flags it.
  +1 test (`cursor_style_accepts_beam_as_alacritty_alias_for_bar`)
  covers all four valid values, plus a real typo
  (`bream`) still flagging.

### Fixed
- **Typing also resets the cursor blink phase.** Final
  user-gesture path that still missed the blink reset. A
  fast typist hitting a key right as `blink_on` was false
  saw a brief flash of no-cursor before the next half-
  period. Alacritty / kitty / iTerm2 / WezTerm all reset
  on every keystroke; matches the rest of the user-driven
  paths kettle now handles (cycle 134: Reset; cycle 135:
  focus actions; cycle 136: mouse focus; cycle 140: modal
  close).

- **Closing a modal overlay also resets the cursor blink
  phase.** Cycle 134 fixed the chord-Reset path; cycles
  135/136 covered focus changes (keyboard and mouse). The
  four modal-close paths — Escape closing the search bar,
  command palette, quick-select hints, or SSH launcher —
  still left the cursor invisible for up to one
  `blink_interval` after the close if it landed on the
  off-half. Same "where's my cursor?" surprise on the
  pane the overlay was hiding.
  - New shared helper `fn reset_blink_phase(&mut self)`
    centralizes the two-line reset (cycle 134's body)
    so the five call sites — search-Escape, palette-Escape,
    hint-Escape, ssh-Escape, and `Action::Reset` — all
    use one path. The `note_focus_change` helper
    (cycle 136) now delegates to it.
  - The `CursorBlinkingChange` event handler (DEC ?12)
    can't call the helper because it runs inside a
    `self.mux.panes.values_mut()` loop (borrow conflict);
    keeps the inline two-line body, documented with a
    pointer to the helper.

- **`font-size` clamps at parse-time, not just at render-time.**
  Cycle 118 added `clamp_font_size` in `Renderer::new` /
  `set_font_size`; cycle 131 surfaced out-of-range as a
  `--check-config` diagnostic. But `cfg.font_size` still held
  the raw value — so `--check-config`'s `font: ... 500pt`
  print echoed the user's input *not* what the renderer would
  use. Now `parse_collect` also clamps to [5.0, 72.0] so the
  stored value matches reality. Cycle 132 already did this
  for the other clamped numerics; cycle 139 closes the
  symmetry. End-to-end: `font-size = 500` now reads as
  `font: ... 72pt` in `--check-config` (with the diagnostic
  still flagging the over-cap value).

### Fixed
- **Bool config keys accept the standard true/false aliases
  + flag unrecognized values.** All five bool fields used:
  `cfg.X = e.value != "false"`. Result: every non-literal-
  "false" value silently meant *true* — so `cursor-style-blink
  = no` enabled the blink instead of disabling it; `copy-on-
  select = 0` enabled copy; etc. A real footgun.
  - New `pub(crate) fn parse_bool(s: &str) -> Option<bool>`
    recognizes case-insensitive `true / yes / on / 1 /
    enabled / enable / y` for truthy and `false / no / off /
    0 / disabled / disable / n` for falsy.
  - The five bool parsers (`cursor-style-blink`,
    `copy-on-select`, `scroll-on-keystroke`,
    `scroll-on-output`, `mouse-hide-while-typing`) route
    through `parse_bool` — bad values keep the current state
    instead of silently flipping to `true`.
  - `detect_malformed_values` flags unrecognized values so the
    typo surfaces in `--check-config`.
  +1 test (`bool_keys_accept_yes_no_off_on_0_1_aliases`)
  covers all 8 truthy + 7 falsy aliases × all 5 keys, plus
  the typo→default-preserved + diagnostic-fires paths.

### Fixed
- **`Renderer::resize` clamps the surface to the device's
  max texture dimension.** Old `resize` only floor-clamped at 1
  (`surface.configure(0, …)` would panic). The ceiling went
  unchecked, so a window stretched past 8192 px (multi-4K
  spans, 8K displays, or a tiling-WM tile larger than the
  device limit) used to silently fail `surface.configure`'s
  validation and leave the surface in a stale state painting
  nothing. Now `width.clamp(1, device.limits().
  max_texture_dimension_2d)` clips to whatever the device
  actually supports — the user sees the visible top-left
  region cleanly instead of a frozen frame. Sibling to cycle
  119's `cap_axis_cells` which fixed the same class of bug on
  the `--screenshot` path.

- **Mouse-driven focus changes also reset the blink phase.**
  Cycle 135 caught the keyboard path; this cycle extends to:
  - Clicking a tab in the tab bar to switch tabs.
  - Clicking inside a pane to focus it (`Mux::focus_at`).
  Both could leave the new pane's cursor invisible for up to
  one `blink_interval` after the click, depending on the
  half-period the timer happened to be on. Extracted the
  cycle-135 pre/post pattern into shared helpers
  (`focus_key()` + `note_focus_change(pre)`) so the three
  focus-changing entry points (`handle_action`, tab-bar
  click, content-area click) all use one implementation.

- **Any focus-changing action also resets cursor blink phase.**
  Cycle 134 fixed it for `Action::Reset` specifically. The same
  "where's my cursor?" surprise applied to every focus-changing
  action: `NextTab` / `PrevTab` / `GotoTab(N)`, `FocusNext` /
  `Prev` / `Up` / `Down` / `Left` / `Right`, `ToggleZoom`, and
  any other action that flipped which pane the cursor lives in.
  Hit `Alt+Right` to jump to the next pane right as `blink_on`
  was false → cursor invisible on the new pane for up to one
  `blink_interval` (530 ms default), which is exactly the
  beat where you've just told kettle "show me where I'm
  typing next."

  Snapshot `(mux.active, mux.active_focus())` before the
  match runs; compare after. If the focused (tab, leaf)
  changed at all, reset `blink_on = true; last_blink =
  Instant::now()`. Catches every focus-changing path in
  one place without decorating each arm individually.

- **`Action::Reset` also resets the cursor blink phase.**
  Cycle 111 swept the modal overlays + selection so the
  chord meant "fresh start" — but it left the `blink_on`
  flag and `last_blink` timestamp untouched. Hitting Reset
  right as `blink_on` was false left the user staring at a
  *missing* cursor for up to one blink-interval (530 ms
  default) — confusing precisely because Reset is the chord
  users hit to recover from a visually-jammed terminal.
  Now sweeps `blink_on = true; last_blink = Instant::now()`
  alongside the cycle-111 modal/selection clears. Mirrors
  the same fix already applied to `TermEvent::CursorBlinking
  Change` so DEC mode 12 toggles also land the cursor
  visible-first.

### Fixed
- **`scrollback = N` clamped at `INFINITE_SCROLLBACK` (10 M
  lines), out-of-range flagged.** A user typo'd or
  curious-pasted `scrollback = 100000000` (100 M) used to
  flow that value verbatim into `cfg.scrollback`, which
  alacritty_terminal honored by reserving rows for ~250 GB
  of history on the first PTY spawn. The docstring on
  `INFINITE_SCROLLBACK` calls 10 M "practical stand-in for
  infinite"; anything higher is asking for an OOM. Now
  clamped at parse to `INFINITE_SCROLLBACK`, and
  `detect_malformed_values` flags above-cap values so the
  user sees the silent cap in `--check-config`. Cycle-132
  pattern, but on a field whose mistake was a memory
  footgun rather than a visual artifact. +1 test
  (`scrollback_clamps_at_infinite_and_flags_above`)
  covers 10M+1, 100M, in-range untouched, the three
  documented escape hatches (`infinite`/`unlimited`/`0`),
  and the cap-above diagnostic.

### Fixed
- **`--check-config` flags the other four clamped numerics
  + `background-opacity` clamps at parse.** Cycle 131
  surfaced `font-size`'s runtime-clamp / docs mismatch. The
  same pattern lived in four siblings:
  - `background-opacity` — no runtime clamp at all (raw value
    flowed to `wgpu::Color { a: ... }`, where alpha < 0 / > 1
    is undefined on some backends). **Now clamped at parse**
    to `[0.0, 1.0]` so the runtime stays safe even if the
    user ignores the warning. + diagnostic for out-of-range.
  - `unfocused-split-opacity` — clamped to `[0.1, 1.0]` at
    parse; diagnostic added.
  - `scroll-multiplier` / `mouse-scroll-multiplier` — clamped
    to `[0.1, 50.0]` at parse; diagnostic added.
  - `minimum-contrast` — clamped to `[0.0, 21.0]` at parse;
    diagnostic added.
  - `cursor-blink-interval` — clamped to `[50, 5000]` at
    parse; diagnostic added.
  +1 test (`detect_malformed_values_flags_clamped_numerics_out_of_range`)
  covers 9 out-of-range entries (all flagged), 14 in-range +
  boundary entries (none flagged), and the new
  `background-opacity` runtime-clamp behavior for the
  user-ignores-the-warning path.

### Fixed
- **`--check-config` flags `font-size` outside `[5.0, 72.0]`.**
  Cycle 118 added a runtime clamp at the renderer; a user
  config of `font-size = 500` silently rendered at the
  clamped 72pt. But `--check-config` echoed the raw value
  verbatim (`font: ... 500pt`), so the docs/diagnostic UI
  and the runtime disagreed without telling the user.
  Same shape as cycle 124's `palette = N=#hex` with N ≥ 16:
  surface the silent clamp as a malformed-value diagnostic.
  The runtime still clamps cleanly — the warning just stops
  the silent mismatch. +1 test
  (`detect_malformed_values_flags_font_size_out_of_renderer_range`)
  covers 500 / 0 / -4 / 72.5 (out of range) and 5 / 72 /
  13 / 13.5 (in-range, including bounds).

### Fixed
- **`Mux::split` while zoomed exits zoom so the user sees both
  halves.** The old `split` set `tab.focus = new_id` but left
  `tab.zoomed = true`, so `Mux::layout`'s zoom-collapse only
  returned the new leaf — the half the user had just split
  from *vanished from the screen* (still alive, just hidden)
  with no UX cue that the split happened. Every modern
  terminal exits zoom on split because "show me both" is the
  intent of the action (tmux's `display-panes` UX after
  `split-window`, WezTerm's `SplitHorizontal/Vertical`).
  Extracted the post-spawn tree mutation into a pure
  `insert_split(&mut Tab, new_id, dir)` helper so the
  contract is unit-testable without a real PTY spawn. +1
  test (`insert_split_exits_zoom_and_focuses_new_pane`)
  covering zoomed-before-split (zoom exits, both leaves
  render) and unzoomed-before-split (no-op on the flag,
  focus still moves).

### Documentation
- **`docs/TESTING.md` and `docs/INSTALL.md` test counts and
  coverage catch up to reality.** Massive drift: INSTALL.md
  claimed `cargo test --workspace` runs **20 tests**; TESTING.md
  enumerated ~33 tests across four crates. Actual workspace
  total is **213 tests** across six crates (2/56/75/10/37/33
  for kettle/kettle-config/kettle-core/kettle-render/kettle-ui/
  kettle-vt). 80+ cycles of additions had landed without the
  testing docs being refreshed. Rewrote TESTING.md with the
  correct counts, broader category descriptions, and pointers
  to the audit-cycle pattern that drives ongoing growth.
  INSTALL.md's test-count claim corrected.

### Fixed
- **`--screenshot foo.jpg` (or no extension) now fails up-front
  with a clear error.** `capture_png` writes via `image::save`,
  which dispatches on the file extension and is compiled
  PNG-only (`kettle-render/Cargo.toml`: `image = { …, features
  = ["png"] }`). A typo'd `.jpg` / `.bmp` / no-extension
  argument used to reach `image::save` *after* all the GPU work
  and surface a crate-internal error:
  `The file extension `."txt"` was not recognized as an image
  format`. Now pre-validated at the CLI surface:
  - `--screenshot foo.txt` → `Error: --screenshot foo.txt:
    extension .txt not supported; only .png is built in`
    (exit 1)
  - `--screenshot foo` → `Error: --screenshot foo: missing
    .png extension` (exit 1)
  - `--screenshot foo.PNG` → still works (case-insensitive)
  Same shape as the cycle-106/107 hard-fails on `--config /typo`
  and `--working-directory /typo` — surface bad input at the CLI
  surface, not deep in the engine.

### Documentation
- **README Quick-start CLI block matches reality.** Same drift
  cycle 126 caught in `--help` was also present in README's
  `Quick start` shell block:
  - `--list-keybinds` claimed "print the default keymap" — but
    cycle 103 made it show the *effective* keymap (defaults +
    overrides + unbinds) when `--config` is active.
  - `--list-actions` (cycle 104), `--list-ssh-hosts` (cycle
    105), and `--screenshot` (cycle 69) were missing entirely.
  - `--config FILE` claim "live-reloaded" stayed, with a new
    "error if it doesn't exist" addendum from cycle 106.
  Block updated; tooling claims now match runtime behavior so
  a first-time user reading the README finds the introspection
  surface kettle actually ships.

- **`kettle --help` text updated for cycle-103/105/106
  behavior changes.** `--list-keybinds` help previously said
  "Print the default keymap" — but cycle 103 made it show
  the *effective* keymap (defaults + overrides + unbinds)
  when a `--config FILE` is active. `--config` help still
  named only `--check-config` and `--screenshot` as
  consumers — cycle 103/105 added `--list-keybinds` and
  `--list-ssh-hosts` to that set, and cycle 106 made the
  flag hard-fail on a non-existent path. Both help strings
  now match runtime behavior; the cycle numbers stay in the
  help text as breadcrumbs for anyone tracing a behavior
  back to its source. No code change beyond the doc-comments
  read by `clap` to generate `--help`.
- **README keybind table gained 9 user-facing default chords.**
  The table previously surfaced only the basics (split / tab /
  copy-paste / search / focus / fullscreen / resize / scroll /
  font / broadcast / reload / reset) and quietly omitted SSH
  launcher (`Ctrl+Shift+S`), command palette (`Ctrl+Shift+K`),
  quick-select hints (`Ctrl+Shift+H`), split-auto
  (`Ctrl+Shift+A`), new window (`Ctrl+Shift+I`), pane zoom
  (`Ctrl+Shift+X`), jump-prompt (`Ctrl+Up/Down`), move-tab
  (`Ctrl+Shift+PgUp/Dn`), and goto-tab-N (`Alt+1..9`). All nine
  surfaced now, with the three "hidden-gem" rows (SSH /
  palette / hints) bolded to match the existing Search
  highlight. Footer line directs power users to
  `kettle --list-keybinds` (cycle 103) for the *effective*
  keymap after their `--config FILE` is applied.
- **+1 README-keybind regression guard.** New test
  `readme_documented_chords_are_actually_bound` pins each of
  the ten promoted chords (`Ctrl+Shift+S/K/H/A/I/X`, `Ctrl+
  Up/Down`, `Ctrl+Shift+PgUp/Dn`) to the action the README
  claims. If a future unbind / rebind drops one of these the
  test fails and the README's docs-drift is caught at CI
  time — same shape as cycles 100/104/117's drift guards but
  on the README surface.

### Fixed
- **`--check-config` flags `palette = N=#hex` with N ≥ 16.**
  The example config (cycle 100) advertised `palette = N=#hex`
  as supporting N in 0..=255, but the runtime apply path only
  writes `theme.palette[0..16]` — overrides for the xterm
  256-color extension (16..255) silently no-op'd. A user
  writing `palette = 200=#ff0000` (intending the bright-red
  cube slot) saw no effect and no warning. Two surfaces fixed:
  - `detect_malformed_values` (`--check-config`) flags any
    `palette = N=…` with N ≥ 16 so the user sees the typo.
  - The example config text reflects the real limit, with a
    note that runtime OSC 4 from a program can still override
    the 16..255 slots (just not the static config).
  Adding full runtime support for 16..255 would mean a Theme
  / renderer-resolve refactor; deferred. +1 test
  (`detect_malformed_values_flags_palette_index_out_of_range`).

### Fixed
- **`Action::NewWindow` now inherits `--config FILE`.** A user
  who launched kettle with `kettle --config /custom.conf` and
  then hit `Ctrl+Shift+I` (or invoked `New window` from the
  command palette) got a child process loading the *default*
  config path. Their theme / font / keybinds appeared in the
  original window but the new window looked like a vanilla
  kettle launch — confusing and easy to mistake for a settings-
  reset. The spawn now passes `--config <self.config_path>` to
  the child when the parent had one, so the new window starts
  with the same settings. No behavior change when no
  `--config` was passed; falls back to the cycle-67 "new tab"
  path if `current_exe()` is unresolvable.

### Fixed
- **`command =` clears the override; `ssh-host =` with empty
  halves is dropped at parse time.** Cycle-121 sibling. Two
  more empty-value bugs uncovered by extending the same
  audit:
  - `command = /usr/bin/fish` followed by `command =` (the
    user trying to revert) used to leave `cfg.shell =
    Some("")`. `shell_argv` then handed `vec![""]` to
    `Terminal::new`, producing an unspawnable empty program
    name. Now: empty value clears the override to `None`,
    so the engine falls back to `$SHELL` as intended.
  - `ssh-host = name=` or `ssh-host = =target` (one half
    empty) used to push `("name", "")` / `("", "target")`
    into `cfg.ssh_hosts`. `--check-config` flagged these as
    malformed (cycle 88) but the *runtime* list still
    contained them — the SSH launcher then showed an empty
    row or tried to connect to "". Now filtered at parse
    time so the diagnostic and the runtime state agree.
  Extended the cycle-121 test with both cases.
- **Empty string-config values no longer silently break
  rendering.** The parser docstring promises "empty value
  resets the key" but `parse_collect` unconditionally
  assigned `cfg.font_family = e.value.clone()` — so a single
  `font-family =` line silently set the family to `""`. The
  renderer's `measure_cell` then asked cosmic-text for an
  empty family name; the system fell back to *some* font but
  cell metrics drifted and glyphs rendered unpredictably.
  Same shape for `font-family-bold / -italic / -bold-italic`
  (per-style overrides) and `theme`. Fix:
  - `font-family =`: empty value is a no-op (keep the
    previous valid value; default is "JetBrainsMono Nerd
    Font").
  - `font-family-{bold,italic,bold-italic} =`: empty value
    *clears* the override (`Option::None`), so the per-style
    family falls back to the main `font-family`.
  - `theme =`: empty value is a no-op (keep the previous
    valid theme).
  +1 test (`empty_value_resets_string_keys_to_their_default`)
  pinning the contract for all five keys.
- **`Mux::reap` keeps `active` pointed at the same *tab*, not
  the same numeric index.** When a tab's last pane exited, the
  tab was removed from `self.tabs`, shifting every later tab
  left by one — but `self.active` was only adjusted by a
  trailing clamp ("if it ran off the end, pull it back"). So
  the case "a tab BEFORE active died" silently shifted focus to
  a different tab without any user action: focused on tab B
  (index 1), tab A dies → tabs become [B, C], `active` was 1
  → now indexes C instead of B. The fix decrements `active`
  whenever `ti < *active` at the moment of tab removal; if
  `ti == *active` (the user IS on the dying tab) focus
  naturally falls on the right-neighbor (matches every
  modern terminal). Logic extracted to pure `pub(crate) fn
  reap_tabs(&mut Vec<Tab>, &mut usize, &[u64])` so the
  active-index bookkeeping is unit-testable without spawning
  real PTYs to populate `self.panes`. +1 test
  (`reap_tabs_keeps_active_pointed_at_the_same_tab`) covers
  all five scenarios: leftmost-dies-while-mid-active,
  leftmost-dies-while-rightmost-active, active-itself-dies
  (right-neighbor takeover), active-is-last-and-dies
  (trailing clamp), and multi-tab death.
- **`--screenshot` caps cells to fit the wgpu 8192-per-side
  texture limit at any font size.** Cycle 69 added static
  `--cols ≤ 400 / --rows ≤ 200` clamps, but at a clamped 72pt
  font the cell is ~35×90px — so `--cols 200 --rows 100`
  computed an 18000×9000-pixel texture (above the 8192 limit)
  and aborted at GPU init with `dimension exceeds the limit of
  8192`. Cycle 119: `capture_png` now dynamically caps each
  axis against the actual cell pixel size via the new pure
  helper `cap_axis_cells(requested, cell_px, chrome_px) ->
  u32` (max-texture-px minus chrome, divided by cell-px,
  floored at 1). Plus it now returns the *actual* (cols,
  rows) used so the CLI's `wrote …` line tells the user when
  their request was capped (`wrote /tmp/k.png (189×89 cells
  — capped from 200×100 for GPU texture limit at current
  font size)`) instead of lying. Also: `capture_png` was the
  *other* unclamped `cfg.font_size` reader (cycle 118 only
  caught `Renderer::new`); that's clamped now too.
  +1 test (`cap_axis_cells_respects_8192_texture_limit`)
  covering happy-path passthrough, axis-specific caps,
  chrome shrinking the budget, and the 1-cell floor.

### Fixed
- **`Renderer::new` now clamps `cfg.font_size` to the same
  range `set_font_size` uses.** Cycle 73 added a `[5.0, 72.0]`
  clamp inside `set_font_size` (the runtime Ctrl+= / Ctrl+- /
  Ctrl+0 path), but `Renderer::new` still took `cfg.font_size`
  raw — so a user with `font-size = 200` in their config
  booted with 200pt cells, potentially hitting the wgpu 8192px-
  per-side texture limit and panicking GPU init. The bound was
  silently enforced only after a Ctrl+0 round-trip flowed
  through `set_font_size`. Same "downstream cache stale at
  startup" shape as cycle 98's font-family fix.
  - New pure helper `clamp_font_size(f32) -> f32` (sanitizes
    NaN to floor; clamps to `[5.0, 72.0]`; both setters now use
    it so the startup and runtime paths can't drift on which
    sizes they accept).
  - +1 test (`clamp_font_size_bounds_match_set_font_size`)
    covering in-range, at-bounds, above/below, negative, NaN,
    and ±infinity. Verified end-to-end: a `font-size = 500`
    config that would have hit the GPU texture limit now
    renders cleanly at the clamped 72pt.

### Added
- **Command palette gained Quick-select hints, Move tab
  left/right, and the four scroll-line / scroll-page entries.**
  When cycle 110 added `ScrollLineUp`/`ScrollLineDown`, the
  defaults map + `--list-actions` + the keybind name table all
  got updated, but the palette didn't — users invoking
  Ctrl+Shift+K and typing "scroll" got only "Scroll to top /
  bottom", no per-line nor per-page. Same drift for `HintMode`
  (Ctrl+Shift+H quick-select labels) and `MoveTabLeft/Right`,
  which had keybinds but no palette label. All five rows added,
  in registry order that puts scroll motions near each other.

### Tests
- **Palette drift guard: every actionable variant must appear
  (or be explicitly excluded).** New test
  `palette_includes_every_user_facing_action` enumerates every
  `Action` variant via an explicit match (so a new variant
  fails compilation until categorized), then asserts each
  variant is in `commands()` OR in a hand-curated `excluded`
  list with a one-line rationale (geometric directional
  motions, parametric `GotoTab(N)`, the palette itself).
  Catches the same shape as cycle 110's drift but on the
  palette surface, so the next time a new Action lands without
  a palette label the CI reports it.
- **Shadow-collision audit added to `defaults()`.** Cycle 115
  found one keybind collision (the cycle-110-introduced
  `Ctrl+Shift+Up/Down` landing on top of the
  `Ctrl+Shift+Arrows` Resize quartet). The class of bug is easy
  to reintroduce: `bind()` is `HashMap::insert()` which
  silently overwrites a duplicate trigger, so a CI run that
  passes `cargo test` can still ship an inconsistent keymap.
  New `defaults_audit() -> (Bindings, Vec<Trigger>)` returns
  both the final map AND the ordered list of every trigger
  the builder bound. `defaults()` becomes `defaults_audit().0`.
  Test `defaults_has_no_shadow_collisions` asserts
  `triggers.len() == map.len()` — and if it fires, builds a
  duplicate set so the panic message names exactly which
  trigger(s) shadowed (and by how many extra bind calls).
  Verifies cycle 115's fix was complete and locks the
  invariant going forward.

### Fixed
- **Cycle-110 keybind collision dropped:** the `Ctrl+Shift+Arrows
  → Resize<dir>` quartet was bound at line 412–415 of
  `keybinds.rs` defaults, then cycle 110 added `Ctrl+Shift+Up /
  Ctrl+Shift+Down → ScrollLineUp/Down` at line 462–463 of the
  same function. HashMap insertion order put the scroll-line
  binds last, **silently shadowing** the Resize-Up/Down chord
  while Resize-Left/Right remained mapped — an inconsistent
  four-direction map (Up/Down scroll, Left/Right resize) that
  passed cargo test but failed user expectation. The defaults
  now drop the Ctrl+Shift+Arrows resize quartet entirely;
  `Shift+Arrows` is the only canonical resize chord (already
  bound at line 418–421 from before, so no resize chord was
  actually lost — just the duplicate). README's keybind table
  updated to remove the `Ctrl+Shift+Arrows` resize column and
  to add a new row for the Scroll-line / Scroll-page / Scroll-
  top/bottom chord family. Cycle-110 test
  (`scroll_line_up_down_bound_to_ctrl_shift_arrows`) grew
  positive guards on `Shift+Arrows → Resize<dir>` for all four
  directions and *negative* guards that `Ctrl+Shift+Left/Right`
  are unbound (prevents a future reintroduction of the
  collision).

### Changed
- **`--check-config` echoes `font-feature` count and per-style
  font-family overrides.** Previously the summary surfaced
  `ssh: N host(s) configured` for SSH but silently dropped the
  other opt-in repeatable/optional keys — a user who had set
  `font-feature = liga` / `font-feature = cv01=2` / etc. saw
  nothing about them, same for the `font-family-{bold,italic,
  bold-italic}` overrides. Now both groups echo when actually
  set (default-config case stays terse). Output:
  - `font-features: <N> configured (ligatures=<bool>)`
  - `font-styles: per-style overrides for [bold, italic, ...]`
  Verified end-to-end against a config with both keys set
  (3 features, 2 styled families) and a `/dev/null` config
  (nothing printed for these lines).

### Fixed
- **`Action::CloseWindow` actually closes the window now (was
  an alias for `CloseTab`).** Both action variants exist in the
  `Action` enum and are surfaced by `--list-actions`, but the
  app handler folded them together:
  `Action::CloseWindow | Action::CloseTab => self.mux.close_tab()`
  which is just-the-focused-tab semantics. A user binding
  `close_window` for "kill the whole app" got tab-close behavior
  with no warning, and a multi-tab kettle window kept running.
  Now they're distinct: `CloseTab` still does `close_tab()`;
  `CloseWindow` calls a new `Mux::close_window()` that drops
  every tab + pane and resets `active = 0`, then the chrome
  exits the event loop. +1 test
  (`close_window_drops_every_tab_and_pane`).
- **`ToggleBroadcastAll` now scopes broadcast to the active tab,
  not every pane in every tab.** `broadcast_write` walked
  `self.panes.values_mut()` — the *whole* pane map, including
  panes in other tabs. A user with `broadcast = true` typing in
  one tab had their keystroke echoed into every pane across
  every tab (often unrelated work, often where they specifically
  *didn't* want their fan-out keystroke landing — `rm`, `git
  push`, anything). Terminator's `broadcast_all` is per-tab,
  iTerm2's "Send Input to All Sessions" defaults per-window,
  kitty's `send_text` targets the current tab. Kettle now
  matches: `Mux::broadcast_write` walks `tabs[active].root.
  leaf_ids()` instead. New `Node::leaf_ids() -> Vec<u64>`
  helper (DFS-order, symmetric with the existing `nth_leaf` /
  `leaf_index_of`). +1 test (`leaf_ids_walks_dfs_order`).
- **`Action::Reset` (Ctrl+Shift+R) now also sweeps kettle's local
  UI state.** Sending RIS (`ESC c`) to the engine reset the grid /
  DEC modes / alt-screen, but kettle owns several pieces of state
  *outside* the engine that survived the chord: the selection
  highlight, any open modal overlay (search bar, command palette,
  hint mode, SSH launcher). A user hitting Reset to recover from a
  visually-jammed terminal got a half-cleared result — fresh grid
  underneath, stale modal floating over it, or a leftover
  highlight on cells that just changed. Now sweeps all four after
  the RIS write: `clear_selection_on_input`, `mux.search.open =
  false`, `palette_input = None`, `hint_state = None`,
  `ssh_input = None`. Matches Alacritty's `Reset` action.

### Added
- **`scroll_line_up` / `scroll_line_down` actions bound to
  `Ctrl+Shift+Up` / `Ctrl+Shift+Down`.** Alacritty, kitty, and
  WezTerm all ship a keyboard chord for line-by-line scrollback;
  kettle had only `Shift+PageUp/PageDown` (one full screen at a
  time) and `Shift+Home/End` (jump to extremes). Filling the
  gap in the middle. New `Action::ScrollLineUp` / `ScrollLineDown`
  variants, `scroll_line_up` / `scroll_line_down` action names
  (also surfaced by `--list-actions`), default bindings on
  `Ctrl+Shift+Up/Down`. Sign matches the mouse-wheel path:
  `Scroll::Delta(+1)` scrolls back. Ctrl+Up/Down stays bound to
  `JumpPrev/NextPrompt` (cycle 47) — both coexist; only the
  Ctrl+Shift+ versions are the new line-scroll. +1 test
  (`scroll_line_up_down_bound_to_ctrl_shift_arrows`) covers the
  new bindings + a regression guard that the existing
  `JumpPrev/NextPrompt` (Ctrl+Up/Down) coexist.

### Fixed
- **`Session::save` is now atomic and surfaces I/O errors.**
  Cycle 108 fixed the *symptom* (corrupted session.json restored
  silently). This fixes the *cause*: the old `save` did
  `fs::write(p, text)` which is non-atomic — if kettle was
  killed mid-write (signal, panic, crash, power loss) the file
  ended up half-written. Now `save_to_path(&Session, &Path) ->
  io::Result<()>` writes to a `.tmp.<pid>.<nanos>` sibling and
  `rename`s it into place (atomic on every supported FS: POSIX
  `rename(2)`, Windows `MoveFileEx` with `MOVEFILE_REPLACE_
  EXISTING`). Mid-write death now leaves either the previous
  state intact (rename hadn't run) or the new state (rename
  succeeded) — never a half-written file. The pub `save`
  wrapper logs `log::warn!("could not save session to <path>:
  <err>")` on failure instead of silently swallowing every
  filesystem error (disk full, permission denied, locked dir).
  +2 tests: `save_to_path_is_atomic_and_round_trips` (asserts
  no leftover `.tmp.*` sibling + round-trip through load), and
  `save_to_path_overwrites_atomically` (rename replaces existing
  contents cleanly).
- **Corrupted `session.json` is backed up + a warning logged
  instead of silently discarding state.** A read error
  (no file on first launch, `HOME` changed) is the expected
  silent path. A JSON parse error is a real signal — kettle
  was killed mid-write, the disk filled up, the file got
  hand-edited badly — and used to silently drop the user's
  tabs/splits/focus state on the next launch with no
  diagnostic and no way to recover. Now: emit
  `log::warn!("session file <path> is corrupted (<err>);
  backed up to <path>.broken.<unix-seconds>")` and `rename`
  the broken file out of the way so the next launch starts
  fresh AND the user keeps a forensic artifact. If the
  rename fails (locked directory, permission issue) the warn
  still lands and the next save overwrites — the user's
  state is gone either way but at least they know. Logic
  factored into `pub(crate) fn load_from_path(p: &Path) ->
  Option<Session>` so the rename-on-corruption contract is
  testable without standing up the full app. +3 tests
  (missing file silent, corrupted file renamed+None, happy-
  path no-rename round-trip).
- **`--working-directory /typo` hard-fails instead of silently
  spawning in `$HOME`.** Cycle-107 sibling to cycle 106's
  `--config /typo` fix. The engine's PTY spawn (`Terminal::new`)
  uses `Some(d) if is_dir => cmd.cwd(d)` and falls back to
  `$HOME` otherwise — so `kettle -d ~/projets` (with a typo)
  silently started the shell in the user's home with no warning
  and no obvious cue that the explicit cwd was discarded. Now
  hard-fail at the top of `main` *before* the engine runs, with
  one of two errors so the fix is one keystroke away:
  - `--working-directory <path>: no such file or directory`
  - `--working-directory <path>: not a directory`
  (the latter for the case where the user accidentally pointed
  at a file instead of a directory). Both exit 1. Verified
  end-to-end: missing dir, regular file, existing dir all route
  correctly.
- **`--config /typo.conf` hard-fails instead of silently using
  defaults.** Every downstream branch (windowed run, `--screenshot`,
  every `--list-*` introspection, the `--check-config` fall-through)
  silently dropped to `Config::default()` when the user named a
  config file that didn't exist. So `kettle --config ~/typoconfig`
  produced a screenshot with the bundled theme and no warning, a
  keybinds list with no overrides, etc. — the user thought their
  file was being read. Hard-fail at the top of `main` with
  `Error: --config <path>: no such file` (exit 1) so the diagnostic
  lands exactly where the typo is. Omitting `--config` (the
  "kettle works out of the box" path) still falls back silently —
  that's intentional. Same "silent-fallback on bad input" shape as
  the cycle-44+ cluster, on the CLI surface.
- **`--screenshot` uses the same `Config::load_from` path as
  windowed startup and reload.** It was the lone hold-out: a
  hand-rolled `parse_collect` call meant malformed values silently
  defaulted with no `log::warn!` (the other paths warned), and
  unknown keys never appeared. Now consistent across all entry
  points.

### Added
- **`kettle --list-ssh-hosts` prints the configured `ssh-host`
  entries.** Companion to `--check-config` (which reported only a
  count) and the in-window Ctrl+Shift+S launcher (which shows them
  but requires opening kettle): users with many `ssh-host =
  name=user@host` lines wanted to verify the parse from the CLI
  without launching. Two-column table aligned to the longest name
  (floor 4 chars so single-character names don't collapse the
  column), sorted alphabetically; empty configs print `(no
  ssh-host entries configured)` so silence isn't ambiguous. Same
  `--config FILE` override convention as the rest of the
  introspection commands; falls back to the default config path.
  Formatting extracted to pure `format_ssh_hosts(&[(String,
  String)]) -> Vec<String>` so the table layout is unit-tested
  (`format_ssh_hosts_sorts_and_aligns_columns`) — sort order,
  alignment width, two-space separator, and the empty-input
  fallback all pinned.
- **`kettle --list-actions` enumerates every valid `keybind` action
  name.** The onboarding gap inverse of `--list-keybinds`: that one
  shows what's currently bound; this one shows what `keybind =
  trigger=…` values are valid. Previously, a user writing a new
  binding from scratch had to either read the kettle source or
  invoke `--check-config` after each guess to confirm a name parsed
  — both fall short of "I want to see the menu". 75 documented
  action tokens (including every alias — `focus_next` /
  `go_next` / `previous_tab` / `prev_tab`), sorted alphabetically,
  followed by two tail lines documenting the parametric
  `goto_tab:N` form and the `unbind` sentinel (which isn't an
  Action variant but is accepted by `apply_keybind`). New pure
  helper `keybinds::action_names() -> Vec<&'static str>`. Kept in
  sync with `Action::from_name` via a drift test
  (`action_names_round_trip_through_from_name`) that asserts every
  listed name parses back to `Some(Action)` — a typo in the list
  or a forgotten alias both fail it.

### Changed
- **`kettle --list-keybinds` honors `--config FILE` (or the default
  config path) and shows the *effective* keymap.** Previously the
  command always printed the built-in defaults regardless of which
  config was active, so a user who had spent time customizing their
  keybinds had no CLI way to confirm their `keybind = …` lines and
  `unbind` sentinels took effect — they had to restart kettle and
  test the chord by hand. New public `keybinds::describe(bindings:
  &Bindings) -> Vec<String>` factors out the sort+label rendering
  so `describe_defaults()` becomes `describe(&defaults())` and
  `main.rs` can pass `&cfg.keybinds` (which is the post-apply
  effective map) instead. End-to-end: overridden triggers show
  the new action label; unbound triggers don't appear in the
  output at all; brand-new bindings the user added land alongside
  the defaults, all in the same sorted listing. +1 test
  (`describe_reflects_user_overrides_and_unbinds`).

### Fixed
- **OSC 1 (icon name) now sets the tab title.** xterm distinguishes
  OSC 0 (icon + title), OSC 1 (icon only) and OSC 2 (title only);
  VTE/alacritty's dispatch only matches `"0" | "2"` and silently
  drops OSC 1 entirely. But vim / tmux / ranger / mc emit OSC 1
  to set their *short* title — exactly the string a tabbed
  terminal wants in the tab bar — so those titles disappeared in
  kettle. kitty / iTerm2 / Gnome Terminal / Konsole all treat OSC 1
  the same as OSC 2 in modern (tabbed) terminals; the icon-name
  distinction predates tabs. The extractor now rewrites the
  leading byte of OSC 1 payloads from `1` to `2` so VTE picks them
  up downstream and `TermEvent::Title` fires normally. Bracket-
  ST and BEL terminators both handled (vim uses `\e\\`; xterm
  uses `\a`). OSC 0 / OSC 2 left untouched. +1 test
  (`osc1_icon_name_rewrites_to_osc2_window_title`).

### Tests
- **Pin OSC 104 (no-param) and OSC 110/111/112 reset conformance.**
  Cycle 47 pinned OSC 104;N (single-index reset). Cycle 56/65/66
  pinned OSC 10/11/12 SET → renderer round-trips. The reset
  siblings — OSC 110 / 111 / 112 (reset default fg / bg / cursor)
  and OSC 104 with no parameters (reset *all* 256 palette
  indices) — were exercised through vte+alacritty but not pinned
  in kettle. A future upstream regression silently disconnecting
  any of those paths would slip through CI. Two new conformance
  tests:
    + `osc_110_111_112_reset_default_fg_bg_cursor_slots` — set
      each of `Colors[256..=258]` via OSC 10/11/12, confirm the
      matching `OSC 11X` clears the slot.
    + `osc_104_no_params_resets_all_256_palette_slots` —
      populate slots 1/2/200 via OSC 4, send `\e]104\a`, assert
      every index in `0..256` is back to None (the "reset
      palette to defaults" trick that theme-changers like
      `zsh-colorize` emit on exit).

### Documentation
- **`docs/kettle.example.config` documents every key kettle
  understands (was 9 of ~35).** New onboarding users copying the
  example into their own config never saw `font-feature`,
  `tab-bar`, `tab-bar-position`, `tab-format`,
  `window-title-format`, `scrollbar`, `cursor-color`,
  `cursor-blink-interval`, `bell`, `osc52`, `unfocused-split-
  opacity`, `focused-split-color`, `split-divider-color`,
  `mouse-hide-while-typing`, `word-delimiters`, `copy-on-select`,
  `scroll-on-keystroke`, `scroll-on-output`, `scroll-multiplier`,
  `minimum-contrast`, `selection-foreground`,
  `selection-background`, `command`/`shell`, `ssh-host`, the per-
  style `font-family-{bold,italic,bold-italic}` keys, the unbind
  sentinels, or the `palette = N=#hex` syntax. All now grouped
  under section headers with comments naming the valid value
  range / enum variants for each. Header callout reminds users
  that `#` is a *full-line* comment marker only — inline `#` in
  a value (e.g. a hex color) is part of the value, NOT a
  trailing comment. New test
  (`example_config_in_docs_uncommented_parses_with_zero_diagnostics`)
  strip-comments the file and runs the activated keys through
  `parse_collect` + `detect_malformed_values`; both must come
  back empty. Catches docs drift: any future key added without
  an example, or any example typo, fails this test.

### Fixed
- **`Config::load_from` now warns on malformed values, not just
  unknown keys.** The reload path (`Action::ReloadConfig`) called
  `Config::load_from`, which `log::warn!`-ed unrecognized keys but
  silently dropped bad values (`font-size = wrong`, missing `=`,
  unknown enum, …). A user hitting the reload chord after editing
  their config got no feedback when their typo didn't apply — they
  could only catch it via `kettle --check-config`. New
  `Config::load_from_with_diagnostics(path) -> (Config,
  Vec<String>, Vec<String>)` returns both diagnostic lists so
  callers can render them (future in-window banner, the existing
  `--check-config` path). `load_from` wraps it and `log::warn!`s
  each list with the file path. `--check-config` now uses the
  same helper, so the two diagnostic sources can't drift on which
  lints they run. +1 test
  (`load_from_with_diagnostics_surfaces_both_unknown_and_malformed`).
- **`Action::ReloadConfig` now applies `font-family` changes.** The
  reload handler picked up the new `font-size` (via the renderer's
  `set_font_size`) but left the renderer's cached `font_family`
  field at whatever was passed to `Renderer::new` at startup. A
  user editing `font-family = ...` in their config and hitting the
  reload chord saw the new size flow through immediately while the
  glyphs kept rendering in the *old* family — only a restart
  picked it up. Same shape as the cycle-44+ "reload swaps `self.cfg`
  but downstream caches are stale" cluster. New `Renderer::
  set_font_family(String)` setter (idempotent guard skips
  re-measure on no-op reloads, so steady-state reloads stay free);
  a sibling private `remeasure_cell()` factored out so the family
  and size setters share one re-measure path and can't drift on
  which fields they touch. `reload_config` calls
  `set_font_family` before `set_font_size` so the cell measurer
  sees the new family when size is re-applied (stale family for
  one frame is a real artifact otherwise). Tested via the
  headless `--screenshot` smoke that builds a full wgpu Renderer
  through the `capture_png` path; pure-helper unit tests aren't
  feasible without standing up wgpu, which the GPU selftest
  already does.

### Added
- **`keybind = TRIGGER = unbind` removes a default binding.**
  `apply_keybind` only ever *inserted* into the map; the closed
  `Action` enum has no "no-op" variant, so a user whose shell wants
  `Ctrl+Shift+C` for itself (some readline kits, certain TUI menus)
  had no way to remove kettle's default Copy on that chord. Now
  the action half accepts the sentinels `unbind` (Ghostty-style),
  `none`, `null`, `false`, or an empty string after the `=`; any
  of them calls `map.remove(&trigger)` instead of inserting. New
  pure helper `keybinds::is_unbind_token(s)` so `apply_keybind` and
  `detect_malformed_values` agree on what's a valid sentinel
  (otherwise `--check-config` would flag `keybind = ctrl+shift+c=
  unbind` as malformed). Aliases are case-insensitive
  (`Unbind` / `UNBIND` work). Unbinding a free trigger is a no-op,
  not an error. +2 tests
  (`apply_keybind_unbind_removes_default`,
  `is_unbind_token_recognizes_aliases`), plus the existing
  `detect_malformed_values_catches_bad_keybind_lines` test grew
  three positive assertions covering each sentinel.

### Fixed
- **`--check-config` flags config lines missing the `=` separator.**
  The line-oriented tokenizer (`parse.rs:21`) silently `continue`s on
  every non-comment, non-empty line that doesn't contain `=`. A typo
  like `font-family Jetbrains Mono` (forgot the equals), a left-over
  TOML-style `[section]` header from a config copied off another
  terminal, or a stray identifier on its own line all just disappeared
  with no warning — and `--check-config` happily reported
  `status: OK — no issues`. `detect_malformed_values` now scans the
  raw text (using the same comment / blank exclusion rules `parse::
  parse` applies internally) and emits `missing \`=\` separator: "<line>"`
  for each offender, so the user sees exactly which lines were
  ignored. Same shape as the cycle-70/84/85/86/87/88 silent-fallback
  cascade, but caught *before* parsing rather than after. +1 test
  (`detect_malformed_values_flags_lines_missing_equals`).
- **Explicit `kettle -e PROG` seeds the tab title from PROG.** Cycle 93
  surfaced `ssh <target>` for SSH panes but every *other* program
  launched with `-e` still showed the generic "kettle" placeholder
  forever: `kettle -e htop`, `kettle -e vim`, `kettle -e tmux` all
  fell through to the shell-default branch even though the user had
  just told us exactly what's running. Worse, the cycle-89 cwd-basename
  fallback doesn't help for these — `htop`/`top`/`less` and most
  full-screen TUIs never emit OSC 2 and either inherit the launching
  cwd (so the basename is your repo, not the program) or have none at
  all. `initial_pane_title(argv)` now extracts the **basename of
  `argv[0]`** as the seed (`/usr/bin/htop` → `htop`), with a hand-curated
  shell allow-list (`bash`, `zsh`, `fish`, `dash`, `ash`, `ksh`, `csh`,
  `tcsh`, `nu`, `elvish`, `xonsh`, `pwsh`, `powershell`, plus the
  `.exe` Windows spellings and `cmd`) that still routes through the
  "kettle" placeholder so the cwd-basename fallback runs — for shells
  the directory name is genuinely more useful than the literal "bash".
  SSH is still special-cased ahead of the basename path so
  `ssh me@box` keeps its argument. The function stays pure; the test
  grew five new assertions covering `htop` / `/usr/bin/htop` /
  `vim file.rs` / `python3 script.py` / `tmux` plus path-qualified
  shells (`/bin/bash`, `/usr/bin/fish`) and the Windows shell names.

### Security
- **SSH tab title seeded from the target.** Fresh SSH tabs
  (Ctrl+Shift+S launcher, restored sessions with an `ssh` argv)
  showed the literal `kettle` placeholder until the *remote*
  shell sent its first OSC 2 — distinguishing six SSH tabs at
  the same host was impossible during connection setup. The
  cycle-89 cwd-basename fallback didn't help (SSH panes have no
  local cwd to fall back to). New pure helper `initial_pane_title
  (argv)` inspects `argv[0] == "ssh"` and renders `ssh <target>`
  (first positional argument, skipping flags) at pane spawn time;
  the existing OSC 2 handler overwrites it the moment the remote
  shell sets a real title. Applies to both fresh launches and
  session restore since both flow through `spawn_pane`. +1 test
  covering ssh / non-ssh argvs and edge cases (`ssh -V`, etc).
- **`--list-keybinds` shows `Goto tab N` (1-based) instead of
  `GotoTab(0)`.** The action label was rendered via Rust's
  `Debug` derive — fine for non-parametric variants (`Copy`,
  `NewTab`, `SplitRight`, …) but leaked the 0-based internal
  index for `Action::GotoTab(0..=8)`. Users reading the listing
  saw `Alt+1 → GotoTab(0)` and reasonably wondered whether tabs
  were 0- or 1-indexed. New `action_label` helper renders the
  1-based human form for `GotoTab` and falls back to Debug for
  everything else (no churn on the other action labels). +1 test.
- **`--check-config` echoes window padding, opacity, and split
  colors.** The cycle-59 expansion of `--check-config` grouped
  many config gates but omitted `padding-x/y`,
  `background-opacity`, `unfocused-split-opacity`, and the cycle-83
  `focused-split-color` + companion `split-divider-color`. Added a
  `window:` line for the always-present numerics and a conditional
  `splits:` line for the opt-in overrides (only printed when at
  least one is set, so defaulted configs stay terse):

      window:  padding=8x8 opacity=1 unfocused-split=0.7
      splits:  focused=#ff8800 divider=#404040

- **OS window title also gets the cwd-basename fallback.** Cycle 89
  taught `Mux::tab_titles` to fall back to the cwd basename before
  the first OSC 2 — `window_title` (used for the OS-level title via
  `Window::set_title`) had the same gap and was returning the
  literal `"kettle"` placeholder even when the cwd was already
  known. Now mirrors the tab-title behavior so the window title and
  the in-app tab agree pre-OSC 2. The bail-out is also tighter: a
  cwd that *literally* equals "kettle" (e.g. `~/Repos/kettle`)
  doesn't collapse the substitution — only the placeholder-with-
  no-cwd path bails. +2 new asserts in the existing
  `window_title_formats_and_falls_back` test (now 1 test split
  into a wider matrix).
- **Tab title falls back to cwd basename before the first OSC 2.**
  Fresh tabs showed the literal placeholder "kettle" until the
  shell emitted `\e]2;…\007` on its first prompt. iTerm2 /
  Ghostty / WezTerm bridge that gap by showing the cwd basename
  or the running command — kettle now shows the cwd basename so
  a tab opened in `~/Repos/kettle` reads as `kettle` (the
  directory, useful) instead of `kettle` (the binary name,
  redundant). Real shell-set titles still win the moment they
  arrive. +1 test pinning the path-basename logic.
- **`--check-config` now catches `font-feature` and `ssh-host`
  typos.** Both arms also had the silent-drop pattern:
  `font-feature = liga,!@#,calt` silently dropped the bad `!@#`
  token leaving the user with a partial feature set; `ssh-host =
  no-equals-sign` silently dropped the entire entry, so the
  Ctrl+Shift+S launcher had no `name` to bind. Now flagged:
  every `font-feature` token has to parse via the documented
  syntax (`liga` / `+calt` / `cv01=2` / `zero on`) and every
  `ssh-host` line needs a non-empty `name=target` form. +1 test.
- **`--check-config` now catches unknown enum values.** Every
  enum-typed config arm (`cursor-style`, `bell`, `osc52` /
  `clipboard`, `tab-bar`, `tab-bar-position`, `scrollbar`) has an
  `_ => DefaultVariant` fallthrough — a typo like `cursor-style =
  wibble`, `bell = loud`, `scrollbar = sometimes` silently fell
  back to the default. The list of valid variants per key now
  lives alongside the apply arm (mirrored exactly), and
  `detect_malformed_values` flags anything not in the documented
  set. Sample after-fix output:

      status:  3 issue(s):
        - malformed value: cursor-style = "wibble"
        - malformed value: bell = "loud"
        - malformed value: scrollbar = "sometimes"

  +1 test covering 7 bad + ~25 good values (every variant + alias
  per key).
- **`--check-config` now catches unknown theme names.**
  `Theme::by_name` silently falls back to TokyoNight Night on an
  unknown name. A user copying `theme = …` from another terminal's
  config (Alacritty `colors.theme`, kitty `include theme.conf`)
  got no warning their theme wasn't bundled. Extended
  `detect_malformed_values` to scan against `Theme::list()`
  case-insensitively (matching `by_name`'s resolution), so
  `theme = NonExistent` now produces:

      status:  1 issue(s):
        - malformed value: theme = "NonExistent"

  +1 test covering an unknown name, plus three valid names
  (bundled, lowercase alias, and a different bundled theme).
- **`--check-config` now catches malformed `keybind = …` lines.**
  `apply_keybind` silently dropped on a bad trigger (typo in
  modifier or key name) or unknown action — a user with
  `keybind = ctrl+shift+nope=copy` or `keybind = ctrl+a=garbage_
  action` got zero feedback that their line never produced a
  binding. Same trap as the cycle-70 / cycle-84 setup. Extended
  `detect_malformed_values` to split each `keybind = ` value on
  `=` and route both halves through `parse_trigger` /
  `Action::from_name` (the same predicates the apply path uses),
  so what `--check-config` accepts is what actually binds. +1
  test (bad trigger + bad action + missing-separator counted;
  valid aliases like `split_horiz` and parametric `goto_tab:5`
  pass cleanly).
- **`--check-config` now catches malformed color values.** The
  cycle-70 `detect_malformed_values` side scan covered numeric/
  duration keys but skipped colors — `background = #not-a-color`
  or `cursor-color = whatever` silently kept the default while
  `--check-config` reported a clean status. Extended to also
  check `background`, `foreground`, `cursor-color`, `selection-
  bg/fg`, `search-bg/fg`, `split-divider-color`, `focused-split-
  color` (incl. alias `split-divider-color-focused`), and
  `palette = N=#hex` (validates both halves). Each goes through
  `Rgb::parse` — same path the apply arm uses — so what
  `--check-config` accepts is what actually applies. +1 test
  covering 6 bad + 7 good values (including X11 3-char hex
  shorthand and color names like `red` which are valid).
- **`focused-split-color` config key.** The inactive pane border
  color was already configurable via `split-divider-color`
  (introduced cycles ago); the *focused* pane's border was
  hard-wired to `theme.palette[4]`. Users with a theme whose
  accent blue blends into nearby content had no way to tune the
  "here am I" indicator without re-theming the whole palette.
  New `focused-split-color` (alias `split-divider-color-focused`)
  fills the gap; `None` keeps the theme-accent default. +1 test.
- **Session restore brings back the focused pane in each tab.**
  `STab` was only saving the split tree (`root`) — restore used
  `first_leaf()` to pick a focus, so every reopened tab landed on
  the leftmost pane regardless of which one the user had focused
  at save time. Now records `STab.focus: usize` as a DFS-order
  index of the focused leaf (pane ids are reallocated across
  restores, so the id itself isn't portable). `#[serde(default)]`
  means pre-cycle session files still load (defaults to `0` =
  first leaf, the previous behavior). +1 round-trip test, +1
  legacy-file test confirming back-compat.
- **All five underline style flags reach the renderer.** Cycle 79
  drew a single line for `Flags::UNDERLINE | UNDERCURL`. The
  engine actually tracks five style bits: UNDERLINE (`\e[4m`),
  DOUBLE_UNDERLINE (`\e[21m` / `\e[4:2m`), UNDERCURL (`\e[4:3m`,
  spell), DOTTED_UNDERLINE (`\e[4:4m`), DASHED_UNDERLINE
  (`\e[4:5m`). The render check now keys on `Flags::ALL_UNDERLINES`
  so every style draws *something*, and `DOUBLE_UNDERLINE` gets a
  second stacked line so the visually-distinct double-underline
  case looks different from plain. Wave/dotted/dashed visual
  styles still draw as a single line — a shader path is deferred,
  but the presence/absence cue is what matters most.
  +1 conformance test confirming each of the five SGR sequences
  reaches the correct engine flag.
- **SGR 58 per-cell underline color is now respected.** The
  cycle-79 underline render used the cell's `fg` for the line
  color — fine for plain `\e[4m` but wrong for neovim spell-check,
  git diff, and LSP diagnostics, which emit `\e[58;2;r;g;b m` to
  draw a *separate* (typically red) squiggle while keeping the
  text in its normal palette color. Renderer now reads
  `cell.underline_color()` and uses it for the underline quad,
  falling back to `fg` when unset. +1 conformance test pinning
  the engine contract: SGR 58 stores the spec, SGR 59 clears it,
  UNDERLINE flag survives.
- **SGR 4 underline + SGR 9 strikeout are rendered.** The engine
  tracked `Flags::UNDERLINE`, `Flags::UNDERCURL` (the `4:3` curly
  variant), and `Flags::STRIKEOUT` correctly — the conformance
  test `sgr_underline_dim_strike` pinned each bit reaching the
  cell since cycle ~14 — but the renderer never turned them into
  pixels. vim's `:set list`, neovim's spell-check, `diff` output,
  `git diff --color-words` deletions, man pages — none of these
  visual cues survived to the screen. New 1-px-tall quads at
  `cell_bottom - 2` for underline (and curly, drawn as a plain
  line for now — a real wave wants a shader tweak) and at
  `cell_mid` for strikeout, both using `fg` so the line color
  follows the text (or the dim / selection override above).
- **SGR 2 dim/faint is rendered.** The engine tracked
  `Flags::DIM` correctly (parsed by vte from `\e[2m`), and there's
  even a `sgr_underline_dim_strike` conformance test confirming the
  bit reaches the cell — but the renderer was ignoring it.
  Programs emitting dim text (fish prompt themers, `less` status
  lines, mc panel headers) rendered at full intensity. New pure
  `kettle_render::color::dim(fg, bg)` blends the fg halfway toward
  the cell bg (50 % intensity, the xterm/alacritty/iTerm2
  convention). Applied *before* the minimum-contrast lift so the
  lift can claw back legibility on themes where dim drops below
  WCAG. +1 helper test.
- **OS cursor turns into a pointing hand over Ctrl-clickable
  URLs.** Browser / iTerm2 / Ghostty convention: the mouse cursor
  morphs from text-I-beam to `CursorIcon::Pointer` while the user
  holds Ctrl (or Cmd, on macOS) and the pointer is on a
  hyperlink — same chord that actually opens the URL. Without
  this affordance, the underline-on-hover (already there) is the
  only hint that the link is clickable. Re-syncs on:
  - `CursorMoved` (position changed → hit-test may flip)
  - `ModifiersChanged` (Ctrl pressed/released → affordance flips
    without the mouse moving)
  - Per-frame in `redraw()` after `update_links()` so a URL
    scrolling out from under a held Ctrl (Ctrl+PageUp, scroll-
    on-output, etc.) doesn't leave the pointer-hand icon stuck
    on a now-empty cell.
  Deduped via `last_cursor_icon` so we don't issue a `set_cursor`
  syscall on every frame.
- **`selection-foreground` is now actually applied.** The config
  key was parsed, stored on `Theme.selection_foreground`, and then…
  ignored by the renderer — selected cells kept their normal text
  color. Dark text on a slightly darker selection background was
  often unreadable. Fixed by capturing the
  `RenderableContent.selection` range at the top of `build_pane`
  and swapping `fg` to `theme.selection_foreground` for cells
  whose point is in the range — applied *after* INVERSE so the
  selection always wins for readability (cells with INVERSE under
  a selection would otherwise render as inverse-fg on selection-bg,
  often invisible).
- **Local paste capped at 4 MiB.** OSC 52 (remote-program write
  into the system clipboard) was capped at 1 MiB back in cycle 47;
  the reverse direction (`paste_clipboard` reads the user's
  clipboard into the PTY) was uncapped — a user with a 1 GB file
  on the clipboard could shove every byte into the PTY in one
  shot and freeze the terminal until the program at the other end
  drained the pipe. 4 MiB fits any realistic code-review / log-
  snippet paste; bigger pastes are almost certainly fat-finger.
  Reuses the existing `clamp_osc52` byte-clamper (UTF-8 char-
  boundary preserved).
- **Tab title truncation honors display columns, not chars.** The
  `truncate(s, n)` helper used `chars().count()` to decide whether
  to cut — but every CJK character or emoji is 2 cells wide in the
  rendered tab segment, so a title like `中文中文中文` (6 chars / 12
  cells) sailed past the segment width without being trimmed and
  overflowed visually. Now sums `UnicodeWidthChar::width()` of each
  char and reserves 1 column for the trailing `…`. Pure helper,
  +1 test covering ASCII / CJK / mixed / edge cases (limit=0,
  exact-fit).
- **`Ctrl+Plus` font-zoom muscle memory works on US layouts.** On
  a US keyboard the `+` glyph lives on `Shift+=` — pressing what a
  user thinks of as "Ctrl+Plus" actually sends `mods=Ctrl+Shift,
  key='+'` to winit. The existing `bind(Ctrl, Char('+'))` binding
  needed bare Ctrl and didn't match. The user got zero feedback;
  font size just stayed put unless they typed `Ctrl+=` instead.
  Fixed by adding the obvious Ctrl+Shift variants of the
  zoom-in / zoom-out chords:
  - Ctrl+Plus, Ctrl+= (already)
  - **Ctrl+Shift+Plus, Ctrl+Shift+= (new)**
  - Ctrl+- (already)
  - **Ctrl+Shift+-, Ctrl+Shift+_ (new — `_` is the shifted `-`)**
  +1 test covering the whole 7-variant matrix.
- **Shift bypasses mouse tracking** (xterm / Alacritty / kitty /
  Ghostty convention). When a TUI like htop, tmux, vim, or fzf
  enables mouse mode (`CSI ?1000h`/`?1002h`/etc.), every click and
  wheel notch was being forwarded to the program — kettle's
  selection, scrollback, and shift-click-extend were completely
  locked out. Now `Shift+click` does a local selection, `Shift+
  drag` extends it, and `Shift+wheel` scrolls kettle's scrollback
  even while the TUI thinks it owns the mouse. Implemented as a
  single early-return in `send_mouse` (so press/release/drag all
  bypass uniformly) plus a parallel guard in the wheel branch.
  Nothing changes when Shift isn't held — mouse tracking still
  works the way it always did.
- **`--check-config` surfaces malformed numeric values.** Every
  numeric/duration config arm was guarded with `if let Ok(v) =
  e.value.parse() { … }` — clean code, but it silently fell back
  to the default when the value didn't parse. A user writing
  `font-size = 14px` or `scrollback = lots` saw a clean
  `status: OK` from `--check-config` while their setting was being
  ignored. New `Config::detect_malformed_values(text)` runs a
  side scan after parse and lists the bad ones; the
  `--check-config` body merges them with the unknown-key list:
  ```
  status:  3 issue(s):
    - unknown key: invalid
    - unknown key: unknown-key
    - malformed value: font-size = "not_a_number"
  ```
  Catches font-size, padding-x/y, background-opacity,
  unfocused-split-opacity, scroll-multiplier, minimum-contrast,
  scrollback (special: accepts `infinite`/`unlimited`/integer),
  and cursor-blink-interval. +1 test covering each. Side scan
  keeps adding-new-validated-keys to one place instead of every
  parse arm.
- **`--screenshot --cols`/`--rows` clamp instead of crashing.**
  Passing a large value (`--cols 100000`) tried to allocate a
  texture exceeding wgpu's per-side limit (8192 px on most GPUs)
  and panicked with `Dimension X value … exceeds the limit of
  8192`. Now clamped to `[20, 400]` cols and `[8, 200]` rows —
  every realistic screenshot fits comfortably, and `--cols 100000`
  produces a 400×200 PNG with a friendly `wrote PATH (400×200
  cells)` instead of a backtrace.
- **`kettle --list-themes | head` no longer panics on broken
  pipe.** Rust's runtime sets `SIGPIPE` to `SIG_IGN` at startup;
  when the reader of a pipeline closes its end early, the next
  `println!` returns `EPIPE` from `write` and the macro panics
  with `failed printing to stdout`. Every shell pipelining
  `--list-themes` (522 lines) or `--list-keybinds` (47 lines) into
  `head`, `grep`, or `less -F` was hitting this panic — silent
  unless you saw stderr, and `rc=0` because `head` itself exits 0.
  Fixed by resetting `SIGPIPE` to `SIG_DFL` at the top of `main`
  (Unix only; Windows has no `SIGPIPE`), so the process exits
  cleanly on EPIPE the way every other CLI tool does. New
  `libc = "0.2"` Unix-only dep (tiny, in the regular ecosystem).
- **`Action::NewWindow` (Ctrl+Shift+I) opens an actual new OS
  window.** The handler was sharing an arm with `Action::NewTab` —
  the parsed keybind dispatched cleanly all the way to a new
  *tab* in the existing window, so users pressing the "new
  window" chord were silently getting a tab (same shape as the
  empty-arm bug fixed in cycle 55). Now spawns a separate kettle
  process via `std::env::current_exe()` + `Command::spawn`, with
  stdio nulled and the child handle dropped so the OS reaps it.
  Falls back to a new tab if the current executable isn't
  resolvable (snap / appimage with custom argv0), keeping the
  keybind useful on weird platforms instead of silently failing.
- **OSC 10 (set default foreground) now reaches the per-pane
  text-area default color.** Companion to the OSC 11 chrome fix
  in cycle 65: a program issuing `OSC 10 ; rgb:RR/GG/BB ST` was
  populating `Colors[256]` and `color::resolve` honored it per-cell
  for fg, but glyphon's per-`TextArea` `default_color` was hard-
  wired to `theme.foreground` — the fallback when a span lacks an
  explicit `Attrs::color` (whitespace / IME composition / chrome
  strings rendered through the buffer). Now per-pane: each
  pane's `TextArea` reads its own `term_colors[256]` override; tab
  bar text and other chrome below keep `theme.foreground`. Same
  precedence as the OSC 10 *query* path.
- **OSC 11 (set default background) now reaches the chrome.**
  The cycle-56 fix paired OSC 12 (cursor color) set with the render
  path; OSC 11 had the same gap but on a larger surface — the
  engine parsed it and populated `Colors[257]`, `color::resolve`
  honored the override for individual cells, but three other places
  hard-wired `theme.background`: the surface clear-color (window
  padding / pane gaps), the active tab-bar segment, and the
  per-cell "is this the default bg, skip the quad?" check. A
  program flipping the bg to red would paint the cells red and
  leave the padding theme-blue — the chrome wouldn't follow. Now
  computed once per `render_frame` from the focused pane's
  `term_colors[257]` and threaded through all three places. Same
  precedence as the OSC 11 *query* path (cycle 44).
- **`Alt+1..9` jumps to tab 1..9** (kitty / Terminator / iTerm2 /
  Ghostty parity). The `Action::GotoTab(u8)` handler has existed
  since the early cycles, but `Action::from_name` had no parser
  for `goto_tab:N` strings and no default keybind, so the action
  was orphaned — users could neither bind it via config nor trigger
  it at all. Now: defaults bind Alt+1..Alt+9 → GotoTab(0..8), and
  config strings `keybind = alt+5=goto_tab:5` work (1-based to
  match the user's mental model; refused on `0` to surface the
  ambiguity rather than silently aliasing first-tab). Alt+0 is
  kept free for users who want to bind "last tab" manually.
  +2 tests (defaults table + parser rules incl. zero-rejection).
- **`Ctrl+Backspace` now sends BS (0x08) for delete-word muscle
  memory.** xterm/alacritty/Ghostty all distinguish the chord:
  plain Backspace → DEL (0x7F, readline `backward-delete-char`),
  Alt+Backspace → ESC+DEL (readline `backward-kill-word` / M-DEL),
  Ctrl+Backspace → BS (0x08). Kettle was mapping Ctrl+Backspace to
  plain DEL — same as a bare Backspace — so users coming from VS
  Code / browsers couldn't get delete-word with their muscle
  memory even after telling bash `bind '"\C-h":backward-kill-word'`
  (the shell never saw the BS that triggers it). +1 test covering
  all three flavors + the Ctrl+Alt combo.
- **OSC 4 multi-index query conformance is now pinned.** The
  cycle-44 fix shipped single-index replies (`OSC 4 ; 1 ; ?`); the
  vte parser already chunks the params so multi-index queries
  (`OSC 4 ; 1 ; ? ; 7 ; ?` — sent by tmux, neovim 0.10+, base16-
  shell-hook to probe an entire palette in one go) emit one
  `ColorRequest` per pair. Added an end-to-end test that asserts
  both indices come through; without per-pair dispatch the batched
  probers would see only the first reply and assume the rest of
  the palette equals the engine default, breaking dark/light
  detection they all rely on.
- **Full xterm Ctrl+<punctuation> C0 row.** Letter mappings
  (Ctrl+A → 0x01, …, Ctrl+Z → 0x1A) were already in place, plus
  `[` `\\` `]` ` `. Missing: `@` (NUL — same as Ctrl+Space), `^`
  (RS 0x1E — vim's alt-buffer toggle and tmux's `Ctrl-^` prefix),
  `_` (US 0x1F), and `/` (US 0x1F — tmux/nano undo). Each was
  previously falling through to "insert the literal character,"
  which silently broke those editor shortcuts. +1 test exercising
  the whole table.
- **`TERM_PROGRAM_VERSION` env var set on every spawned shell.**
  Companion to the existing `TERM_PROGRAM=kettle`; iTerm2 / kitty /
  WezTerm / Ghostty all set this pair. Neovim's
  `:checkhealth provider`, fish's prompt themers, and various
  shell/script diagnostics key off the pair when probing for known
  modern terminals — without the version, kettle showed up as "an
  unknown program calling itself kettle" rather than "kettle
  v0.1.0." Populated from `env!("CARGO_PKG_VERSION")` so a bumped
  `Cargo.toml` flows through with no separate string to maintain.
- **`--check-config` now echoes every per-cycle config gate.** The
  command was added back at cycle 5-ish and still only reported
  five fields (config path, theme, font, scrollback, keybind
  count). Since then we've added ~15 user-facing toggles — bell,
  OSC 52 policy, minimum-contrast, scroll-on-keystroke, scroll-on-
  output, scroll-multiplier, mouse-hide-while-typing, copy-on-
  select, tab-bar mode/position/format, window-title-format,
  word-delimiters, ssh-host count, cursor style/blink/interval —
  and none of them surfaced. A user setting `mouse-hide = false`
  had no way to verify it was actually applied without reading the
  source. `--check-config` now groups them by theme (cursor / bell+
  osc52+contrast / scroll / mouse / tabs / title / words / ssh) so
  the output stays scannable; `word-delimiters` and `ssh` lines
  only render when non-empty.
- **Bracketed paste also strips the *opening* marker `\x1b[200~`.**
  The injection-guard added earlier (and tested in
  `paste_strips_injected_end_marker`) only filtered the closing
  marker `\x1b[201~` — the well-known attack vector that ends paste
  mode early and lets the shell auto-execute the remainder. But the
  opening marker is the same class of bug on the other side: a
  paste containing `\x1b[200~` can confuse some shells into thinking
  they're entering paste mode mid-way, so our genuine `\x1b[201~` at
  the wrapper's end doesn't actually exit it — subsequent typed
  input is then absorbed as paste content. Alacritty / iTerm2 /
  WezTerm all strip both. +1 test (`paste_strips_injected_start_
  marker`) pairs the contract symmetrically with the close-marker
  test.
- **OSC 7 cwd percent-decoding handles UTF-8 paths correctly.**
  Shells (zsh `print -P %d`, bash `printf '\\e]7;…'`) percent-encode
  each *UTF-8 byte* of a non-ASCII filename individually — `café`
  arrives as `caf%C3%A9`. The old parser pushed each decoded byte
  as a `char`, which produced the Latin-1 garbage `cafÃ©` and broke
  new-tab/split cwd inheritance, the window title's `{cwd}`
  placeholder, and the OSC 7 session-restore path for every user
  with a non-ASCII directory in their tree. Fixed by decoding into
  a `Vec<u8>` and converting via `from_utf8_lossy`. +1 conformance
  test covering non-ASCII alone and mixed (`%20` space + `%C3%A9` +
  ASCII).
- **OSC 12 (set cursor color) now actually paints the cursor.**
  Companion bug to the OSC 4/10/11/12 *query* path shipped two
  weeks ago: the engine already parsed `OSC 12 ; rgb:RR/GG/BB ST`
  and populated `Colors[258]`, but the renderer hard-wired the
  cursor quad to `theme.cursor` so the override never reached the
  screen. Drawing now resolves via `kettle_render::color
  ::resolve_query(258, theme, term_colors)` — runtime override
  wins, theme value is the fallback. The same precedence rule the
  *query* path returns, so OSC 12 set + OSC 12 ? now agree.
  Confirmed end-to-end via a new test asserting OSC 10/11/12 SET
  populate engine slots 256 / 257 / 258 with the exact xparsecolor
  values.
- **`move_tab_left` / `move_tab_right` actions now actually move
  the tab.** They were bound to `Ctrl+Shift+PageUp` / `PageDown` in
  the default keymap (Terminator parity), parsed correctly, and
  threaded all the way to `Action::MoveTabLeft|MoveTabRight` in the
  app — and then dispatched into an empty arm. Every press was a
  silent no-op. Wired by a new `Mux::move_active_tab(delta: i32) ->
  bool` that swaps the active tab with its neighbor and clamps at
  the edges (no wrap, matching iTerm2 / Ghostty / WezTerm; wrap
  would have the tab bar lurch across on every press). +1 test
  covering swap, clamp, no-op, and the < 2 tabs case.
- **Selection auto-scrolls when you drag past the pane edge.**
  Previously the highlight stopped at the visible boundary — you
  had to release, scroll, then shift-click to extend. Every modern
  terminal (Alacritty / iTerm2 / WezTerm / kitty / Ghostty) keeps
  the scroll going while the mouse holds past the edge so a
  long-distance "select these 500 lines" gesture is a single
  click-and-drag. Per-frame rate scales with overshoot (1 line/
  frame at the edge, 2 at 10 px past, 3 at 40+ px) via a pure
  `selection_autoscroll_lines(y, top, bottom)` helper. The event
  loop wake-up cadence (`about_to_wait`) now keeps a 30 fps tick
  alive while drag-autoscroll is active, so the user doesn't have
  to wiggle the mouse to keep it moving. +1 test covering all six
  zones (inside, just-past, moderate, big × top/bottom).
- **`word-delimiters` config** (Alacritty `selection.
  semantic_escape_chars` parity, aliases `selection-word-chars` and
  `semantic-escape-chars`). Customizes what counts as a word for
  double-click selection (and the jump-to-prompt search that uses
  the same boundary set). Defaults to empty meaning "use the engine
  default" — `,│\`|:\"' ()[]{}<>\t`. Override to e.g. `()[]{}` to
  drop `/` and `:` from the delimiter set so a double-click on a URL
  or path picks it up whole. Plumbed through a new
  `Terminal::new(word_delimiters: Option<&str>)` arg →
  `TermConfig::semantic_escape_chars`. +1 config-parse test
  covering the canonical key and both aliases.
- **Shift+Click / right-click extend the selection** (xterm /
  Alacritty / iTerm2 / WezTerm convention). Previously every left
  click started a fresh selection at the click point, so the only
  way to grow a selection across a long scrollback was to start
  the drag over and hold all the way through. Now:
  - **Shift+left-click** anchors the current selection's start and
    pulls the end to the click — and you can keep dragging from
    there. Shift+Alt-Click still does block-select (Alt takes
    precedence). Shift+Click on empty space falls through to a
    normal new-selection.
  - **Right-click** extends an existing selection to the click;
    bare right-click on empty space is still a no-op so a stray
    right-click doesn't conjure a selection.
  Shared via a new `extend_selection_to_cursor` helper that updates
  the engine selection's right edge and enters drag mode for live
  follow-up. Copy-on-select fires on right-click extend too.
- **Wheel over tab bar cycles tabs** (kitty / iTerm2 / Ghostty
  parity). Spinning the mouse wheel while the pointer is over the
  tab bar now switches tabs (wheel-up = previous, wheel-down =
  next) instead of scrolling the focused pane's scrollback — the
  same gesture every modern terminal binds. Honors
  `tab-bar-position = bottom` and the hidden-bar case (`tab-bar =
  off` or `auto` with one tab). Pure `cursor_in_tab_bar_band`
  geometry helper, +1 unit test covering top/bottom/hidden bands.
- **`mouse-hide-while-typing` + selection clears on typing.** Two
  QoL gaps every modern terminal (Alacritty, kitty, WezTerm,
  iTerm2, Ghostty) has but kettle didn't:
  - The OS mouse cursor now hides on keyboard input (configurable,
    default on; aliases `mouse-hide`) and reappears on the next
    mouse move — so the pointer doesn't sit over the column you're
    editing.
  - The focused pane's selection is cleared on any keystroke that
    produces PTY bytes — so typing after a select doesn't leave a
    stale highlight behind to confuse the next copy/paste.
  Wired via small `hide_mouse_cursor`/`show_mouse_cursor`/
  `clear_selection_on_input` helpers on App. +1 config test.
- **Modified named keys now encode per xterm modifyCursorKeys** —
  `Ctrl+Right` (skip-word in bash/zsh/readline), `Ctrl+Delete`
  (delete-word), `Shift+Tab` (`CSI Z` back-tab used by readline /
  fzf / TUI forms), and modified arrows / F-keys / Insert / Delete /
  PageUp / PageDown / Home / End all previously collapsed to their
  unmodified sequence — vim users couldn't word-skip, fzf couldn't
  reverse-tab through fields. New pure `xterm_modifier(mods) → u32`
  emits the standard table (1 + shift + 2·alt + 4·ctrl + 8·super)
  and the encoder switches:
  - Arrows / Home / End → `CSI 1;<m>A..D|H|F` when modified
    (unmodified still honors DECCKM, modified always CSI).
  - Insert / Delete / PgUp / PgDn / F5..F12 → `CSI <n>;<m>~`.
  - F1..F4 → `CSI 1;<m>P..S` when modified (SS3 only when bare).
  - `Shift+Tab` → `CSI Z`.
  +2 tests covering the modifier table + every encoded family.
- **DECSCUSR cursor shape & DEC ?25 visibility now honor the
  running program.** Vim / neovim / fish flip cursor shape per-mode
  (`CSI 1 SP q` block in normal, `CSI 5 SP q` beam in insert,
  `CSI 3 SP q` underline in replace), and full-screen TUIs hide the
  cursor with `CSI ?25 l`. The renderer ignored both and always drew
  the static `cursor-style` config shape — so insert mode looked the
  same as normal mode, and the cursor stayed visible over `less`/
  `fzf`/`htop`. Fixed by seeding the engine's `default_cursor_style`
  from `cursor-style` at pane creation (so the user's static config
  is still the default) and reading the live
  `RenderableContent.cursor.shape` per frame — which the engine
  collapses `?25 l` into `CursorShape::Hidden` for, so a single
  guard handles both DECSCUSR and visibility. Added a new
  `HollowBlock` shape for when programs ask for an outline (vim's
  `:set guicursor` does this). +3 tests (config→engine mapping;
  engine ↔ ?25 hide/show round-trip; existing DECSCUSR shape test
  retained).
- **`scroll-on-keystroke` (alias `scroll-on-input`) + `scroll-on-
  output`** (Alacritty / xterm parity): two new bools governing
  scrollback behavior. `scroll-on-keystroke` defaults `true` (typing
  yanks you to the bottom — the longstanding behavior, now opt-out
  so pinning the viewport while typing is possible) and `scroll-on-
  output` defaults `false` (a chatty background process won't tear
  you away from the page you're reading; flip it on to chase the
  newest line). Output detection uses a pure
  `kettle_core::scrollbar::should_scroll_on_output` helper (history-
  size diff against the previous frame; first frame is a no-op) so
  the rule lives outside the render path. +1 config-parse test, +1
  pure-helper test.
- **OSC color set/reset round-trip conformance** — end-to-end test
  that `OSC 4 ; 1 ; rgb:…` writes into the engine's `Colors` slot and
  `OSC 104 ; 1` clears it. Guards the connection between OSC color
  set/reset (parser → engine) and the OSC 4/10/11/12 *query* reply
  path shipped last week — together they prove a full xparsecolor
  loop works.
- **DEC mode 12 (cursor blink) now honors the running program.**
  `CSI ?12 h` / `?12 l` is the standard way for vim, neovim, and
  shell prompts to ask the terminal for a solid (steady) or blinking
  cursor inside their UI. The engine raised
  `TermEvent::CursorBlinkingChange` and tracked the state on
  `cursor_style().blinking`, but the app's blink decision was hard-
  wired to the static `cursor-style-blink` config — every program
  request was silently ignored. Wired via a small
  `Terminal::cursor_blinking()` accessor (engine kept hidden), with
  the redraw + cursor-visibility path now intersecting config and
  live pane state. The event handler resets the blink phase so
  off→solid is immediate (no half-period delay). Default initial
  blink is seeded from `cursor-style-blink` at pane creation. +1
  conformance test.
- **`CSI 14 t` (text-area pixel size) now replies.** Sixel / kitty
  graphics / iTerm2 OSC 1337 apps probe this to compute
  pixel-perfect image placements (a 200-px image needs to know how
  many cells it covers); the engine raised
  `TextAreaSizeRequest(formatter)` but the app's event loop dropped
  it and the apps fell back to a 8×16 cell guess. New pure helper
  `kettle_render::reply_for_text_area_size(cols, rows, cell_w,
  cell_h, fmt)` feeds the engine formatter the live grid + cell
  dimensions and yields the canonical xtwinops reply
  `CSI 4 ; <height-px> ; <width-px> t`. +1 conformance test.
- **OSC 4 / 10 / 11 / 12 color queries now reply** (xparsecolor
  `rgb:RRRR/GGGG/BBBB` form). vim/neovim and tmux use these to detect
  the actual default foreground / background / cursor and the live
  palette so they pick a colorscheme that matches the terminal. The
  engine emitted `ColorRequest` events but the running app silently
  dropped them — now they're resolved against the active theme plus
  any runtime OSC overrides via a pure `kettle_render::reply_for_query`
  (palette 0..=15 → theme, 16..=255 → xterm cube, 256/257/258 →
  fg/bg/cursor; out-of-range → no reply). +2 tests (pure helper +
  end-to-end formatter shape for all four OSC prefixes).
- **`tab-format`** (alias `tab-title-format`): user-templatable per-tab
  label (default `{n}: {title}`) via the shared `template::fill`;
  unknown placeholders pass through verbatim; the trailing `✕` is
  still appended by the renderer. +1 test.
- **`window-title-format`** (alias `title-format`, Ghostty/WezTerm
  parity): template the OS window title with `{title}` / `{cwd}` /
  `{tab}` placeholders; `{{`/`}}` escape literal braces; unknown
  placeholders are left as literal text (typos visible). Pure
  `kettle_config::template::fill` + 4 tests.
- **`minimum-contrast`** (WezTerm parity) — keep text readable on
  low-contrast themes by lifting each cell's foreground toward
  white/black until it meets a configured WCAG 2.0 ratio (`0.0` = off,
  `4.5` ≈ AA, `7.0` ≈ AAA). Pure `color::with_min_contrast` over
  `relative_luminance`/`contrast_ratio` (+4 tests).
- Mouse-wheel scroll speed is now configurable: `scroll-multiplier`
  (alias `mouse-scroll-multiplier`, default `1.0` ≈ 3 lines per notch,
  clamped 0.1–50) scales both `LineDelta` and `PixelDelta` input;
  Ghostty/kitty parity. Pure `wheel_lines` helper, +2 tests.
- OSC 52 clipboard **writes are now size-capped** (1 MiB, truncated on
  a UTF-8 char boundary) so a hostile program can't push an unbounded
  payload into the system clipboard.
- **OSC 52 clipboard policy** (`osc52 = off|copy|paste|both`, default
  `copy`): clipboard *reads* via OSC 52 — which let a possibly-remote
  program exfiltrate your system clipboard — are now **denied by
  default** (an empty, well-formed reply is sent); writes remain
  allowed. Configurable per the new key (alias `clipboard`).
- Hardened **URL opening**: a URI from terminal output (an OSC 8
  hyperlink or autodetected link, opened via Ctrl/Cmd-click or hint
  mode) is now run through `links::is_safe_url` before the OS handler —
  only `http(s)`/`ftp(s)`/`mailto`/`file://` are allowed; custom
  schemes (`javascript:`, `vscode:`, `data:`, …), control characters,
  whitespace, `file://` path traversal, and absurd lengths are
  refused. Closes a known terminal scheme-handler abuse vector.

### Fixed
- Scrollback **search now scrolls the viewport to the active match**:
  matches in history (and `Tab`/`Shift+Tab` cycling onto them) bring
  the line into view (~⅓ from the top), once per match/query change so
  wheel-scrolling still works. Previously off-screen matches were found
  but never shown. Pure tested `search::reveal_offset`.
- Theme cycling (`next_theme`/`prev_theme`) now matches the current
  theme **case-insensitively and trimmed** (like `by_name`), so a
  config such as `theme = tokyonight night` cycles from the right
  place instead of jumping to the first theme.
- Split keys now match Terminator exactly: `Ctrl+Shift+O` splits
  horizontally (top/bottom), `Ctrl+Shift+E` splits vertically
  (left/right); `split_horiz`/`split_vert` action names corrected.

### Added
- `kettle --screenshot <out.png> [--cols --rows]`: renders a representative
  frame **offscreen** (no window) through the real `wgpu`/`glyphon`/quad
  path and writes a PNG. Used to generate the showcase images in
  **docs/UX-COMPARISON.md** — a cited UI/UX comparison matrix (kettle vs
  Ghostty/kitty/WezTerm/Terminator/Alacritty) with a tab-bar hit-region
  mermaid and the prioritized backlog status.
- UX backlog: unfocused-pane **dimming** (`unfocused-split-opacity`,
  default 0.7), **pane zoom/maximize** (`Ctrl+Shift+X`), per-pane
  **scrollbar** (`scrollbar = never|auto|always`), configurable
  **split-divider color**, configurable **cursor-blink interval**, and
  a **copy-on-select** toggle. Dimming/scrollbar use a post-text quad
  pass so they sit above glyphs.
- Tab bar redesign: per-tab close **✕** (click to close), trailing
  **+** new-tab button, **middle-click** a tab to close it,
  always-shown by default, active-tab accent, title eliding. New
  config `tab-bar` (off|auto|always) and `tab-bar-position`
  (top|bottom). Geometry is a single source of truth shared by the
  renderer and click hit-testing.
- `kettle --list-keybinds` prints the resolved default keymap
  (`trigger → action`, sorted) so the binding set is discoverable
  without reading the source (parallels `--list-themes`).
- A theme picked at runtime now **persists across restarts** — it's
  saved in `session.json` (`theme`, `#[serde(default)]` so older
  sessions still load) and reapplied on launch, until you change it
  again or reload the config.
- **Live theme switching**: `next_theme` / `prev_theme` keybind actions
  and "Next theme" / "Previous theme" command-palette entries cycle the
  ~512 bundled themes at runtime — no config edit or reload. Pure
  `Theme::cycle` (wrap-around; unknown current → first theme).
- The scrollback **scrollbar is now interactive**: left-click the
  focused pane's right-edge bar to jump the viewport there, then
  **drag** to scrub through history (x is ignored once grabbed, like a
  normal scrollbar; released on button-up). Geometry moved to a pure,
  tested `kettle_core::scrollbar` module (`thumb` for drawing,
  `target_offset` for the click mapping), shared by the renderer and
  the UI (was duplicated, untested math).
- `--config FILE` selects an explicit config file instead of the
  default path; it is honored by the running terminal (including the
  live-reload watcher, which now watches that file's directory) and by
  `--config-path`, `--check-config`, and `--screenshot`.
- **Middle-click pastes** the clipboard into the focused pane (standard
  X11 terminal behavior; bracketed-paste-safe via the shared
  `paste_clipboard`), when mouse-reporting isn't consuming the click
  and the cursor isn't over the tab bar (where middle-click still
  closes a tab).
- The OS **window title now follows the active pane** — switching tabs
  or focusing another split retitles the window (not just on OSC title
  events), with empty/placeholder titles falling back to `kettle`. The
  `set_title` call is deduped so it isn't a per-frame syscall.
- **Rectangular (block) selection**: hold `Alt` and drag to select a
  column block (iTerm2/Alacritty/WezTerm parity), via a pure
  `selection_kind(clicks, alt)` mapping; word/line still copy on press,
  Simple/Block copy on release.
- Standard launch CLI: `-e/--exec CMD …` runs a command in the first
  tab instead of the shell (consumes the rest of the args, hyphenated
  program flags included — e.g. `kettle -e ssh -t host`) and
  `-d/--working-directory DIR` sets its directory; either overrides a
  saved session for that first tab. (`kettle_ui::run_with(Options)`.)
- New tabs and splits now **inherit the focused pane's working
  directory** (via OSC 7), like WezTerm/iTerm/kitty — open a split and
  you're already in the same project. A since-deleted directory falls
  back to the default (`usable_cwd` guard) instead of failing to spawn.
- Quick-select **hint mode** is now usable (`Ctrl+Shift+H`): every
  visible URL / path / git-hash / IP gets a short label drawn over the
  focused pane (chip + glyph); type the label to open it (URLs via the
  OS handler) or copy it to the clipboard, `Backspace` to correct,
  `Esc` to cancel. New `hint_mode` keybind action.
- Quick-select / hint-mode core (`kettle_core::hints`, pure +
  fully-tested): scans the visible rows for URLs, filesystem paths,
  git hashes and IPv4 addresses (higher-priority kinds win on overlap,
  trailing punctuation trimmed, char-column coordinates) and generates
  minimal-width unique labels over a home-row alphabet. The overlay +
  key-to-act wiring is the next cycle.
- Docs: `ARCHITECTURE.md` refreshed to the current system — crate
  responsibilities, the side-channel chunk set
  (VirtualImage/Animation/RelativePlacement), the per-pane registries,
  the animation redraw tick, an accurate test count, and a **new
  mermaid diagram of the kitty graphics pipeline** (decode → registries
  → placeholder/relative/animation render).
- Search is now a **real regex with smart-case**: the `Ctrl+Shift+F`
  pattern is compiled as a regex (alternation, anchors, `\b`, …),
  case-insensitive unless it contains an uppercase character
  (ripgrep/vim smart-case), and an invalid pattern falls back to a
  literal search instead of returning nothing (`search::build_regex`).
- Command palette (`Ctrl+Shift+K`): a fuzzy action launcher over a
  29-command registry (`kettle_config::palette`) — type to filter,
  `Tab`/`↑↓` to select, `Enter` to run, `Esc` to cancel. Bottom-bar
  overlay reusing the SSH-launcher plumbing; new `command_palette`
  keybind action.
- Fuzzy matcher (`kettle_config::fuzzy`, dependency-free): subsequence
  scoring with prefix / word-boundary / camelCase / contiguity bonuses
  and a length penalty (`score`, `best`). The `Ctrl+Shift+S` SSH
  launcher now fuzzy-matches host names on `Tab`-complete and `Enter`
  (was prefix-only); the matcher is reusable by a future command
  palette.
- VT conformance sweep: IRM insert mode (`CSI 4h` shifts text right),
  DECTCEM cursor visibility (`CSI ?25 h/l`), LNM mode bit
  (`CSI 20 h/l`), DECCKM + DECKPAM/DECKPNM application cursor/keypad
  modes, and mouse-tracking DECSET flags (`?1000/?1002/?1003/?1006`)
  set and cleared — 5 end-to-end tests through the real vte path.
- kitty relative placements: parents can now also be **regular
  placements** (not just placeholders) and **relative chains** are
  resolved — a pure `resolve_chain` walks child→parent with a depth
  bound of 8 (kitty `ETOODEEP`; cycles are bounded, not infinite), with
  parent origins unified from placeholder cells and the image registry.
  This completes the kitty graphics protocol surface.
- kitty relative placements **now render** when the parent is a visible
  Unicode-placeholder (virtual) image: the child image is drawn `(h,v)`
  cells from the parent's placeholder origin (the min abs-line/column of
  its cells), through a per-terminal `Relatives` registry and the pure
  `relative_origin` clamp. Parents that aren't on screen this frame are
  skipped; the placement group still dies with its parent.
- kitty relative placements (decode/state): `a=p,P=,Q=` is recorded as
  a `RelativePlacement` (parent image/placement + `H`/`V` cell offset)
  instead of drawing at the cursor; a placement group dies with its
  parent (parent-image deletion cascades to its relatives). Render-time
  resolution of the on-screen position from the parent is the next
  sub-item.
- kitty animation frame compositing: partial-rect `a=f` frames are
  blended (or `X=1` replaced) over a chosen canvas — a previous frame
  (`c=`), a `Y=` background color, or transparent — and `r=` edits an
  existing frame in place; `a=c` copies a rectangle between frames
  (including onto the root image). New RGBA `ImageData::compose`
  (source-over) and `solid` primitives.
- kitty animation **now plays end-to-end**: `a=f` frames / `a=a`
  control snapshot through `Chunk::Animation` into a per-terminal
  `Animations` registry; at draw time a placement's image is swapped for
  the frame the playback clock selects, and the event loop schedules
  ~30 fps redraws while any animation is running. Root-frame gap via
  `a=a,r=1,z=`; animations are reaped with the image or by `a=d,d=f`.
- kitty animation playback-timing engine: pure, deterministic
  `current_frame(gaps, state, elapsed_ms)` mapping elapsed time to the
  frame to show — skips gapless frames, honors infinite/finite loop
  counts, `loading`-mode hold-at-end, and stopped→selected-frame. The
  renderer clock + frame substitution is the only remaining sub-item.
- kitty animation (decode/state layer): `a=f` animation-frame
  transmission (chunked via a single in-flight slot, gap from `z` with
  `z<0` = gapless base frames), `a=a` animation control (`c` current
  frame, `s` = stop/run/loading, `v` loop count, `r`+`z` per-frame gap),
  and `a=d,d=f` frame deletion (keeps the base image).
  `KittyState::frames()/animation()` expose the model for the upcoming
  playback/compositing cycle. Cited: kitty
  `docs/graphics-protocol.rst:839`.
- Font-feature tuning: `font-feature` now parses real OpenType tags
  (`liga`, `calt`, `ss01`, `cv01`, `zero`, …) with `+tag` / `-tag` /
  `tag=N` / `tag on|off` dialects, repeatable and comma-separated, and
  applies them through cosmic-text `FontFeatures` on top of the coarse
  ligature toggle (explicit settings win; Advanced shaping kept whenever
  any feature is set). Cited: Ghostty `font-feature`, kitty
  `font_features`.
- kitty placeholders: the **placement id** is now decoded from each
  cell's underline color (256/truecolor/named), feeding the spec's
  run-grouping and left-inheritance so cells of different placements no
  longer inherit across each other.
- kitty Unicode placeholders **now render**: each frame the visible grid
  is scanned for `U+10EEEE`, the image id is read from the cell
  foreground (256-color / truecolor / ANSI-named) plus the msb diacritic,
  contiguous runs apply the left-inheritance rules, and the referenced
  `U=1` virtual image is sliced per cell (`ImageData::crop` +
  `placeholder::tile_src_rect`, exact-tiling) and drawn through the
  existing GPU image pipeline. Virtual images are reaped on
  delete-by-id/all. (`Terminal::placeholder_tiles`.)
- kitty Unicode placeholders (decode layer): `kettle-vt::placeholder` —
  the 297-entry row/column diacritic table, per-cell diacritic parsing,
  32-bit image-id reconstruction (foreground + msb diacritic), and the
  omitted-diacritic left-inheritance algorithm; plus `U=1` **virtual
  placements** in the kitty decoder (`a=p,U=1` / `a=T,U=1` store the
  image and register a rows×cols placement without drawing at the
  cursor). Renderer compositing of placeholder cells is the next cycle.
- VT conformance: XTWINOPS `CSI 18 t` text-area size report
  (`CSI 8 ; rows ; cols t`), DSR `CSI 5 n` device-status (`→ CSI 0 n`),
  and an exact-match DA1 assertion (`CSI c`/`CSI 0 c` → `CSI ? 6 c`).
  44 conformance tests total.
- VT conformance suite — 35 end-to-end tests through the real
  `vte`+`alacritty_terminal` path: CUP/erase/SGR/tabs, scroll region,
  charsets, ICH/DCH/IL/DL, DECSC/DECRC, autowrap, origin mode, DECALN,
  REP, SO/SI, RIS, ECH, CHA/HPA/VPA, SU/SD, DECSCUSR, wide CJK,
  combining marks, OSC 4/8/52, DECRQM, DSR/DA1/DA2, DECSET 1049.
- kitty graphics advanced ops: transmit-only store, place-by-id,
  delete (all/by id), z-index ordering.
- Per-style font families (`font-family-bold/italic/bold-italic`) and a
  ligature shaping toggle.
- Configurable bell (`off|visual|attention|both`) with cross-platform
  window-attention (taskbar/dock urgency); no audio deps.
- Focus-event reporting (DEC ?1004).
- UX polish: safe bracketed paste, double/triple-click word/line select
  with auto-copy, focus-aware hollow cursor, cursor blink, visual bell.
- Offscreen GPU self-test (WGSL compile + render pass) run in CI on
  Linux/macOS/Windows.

## [0.1.0] — 2026-05-19

First cross-platform release; artifacts built on real runners and
attached to the GitHub release (Linux tar+`.desktop`, macOS `.app`,
Windows zip).

### Added
- GPU renderer: `wgpu` + `glyphon`, tiled multi-pane, tab bar, split
  dividers, focus border, cursor/selection/search overlays.
- Engine: `portable-pty` + `alacritty_terminal` + `vte`, per-pane
  reader thread, infinite scrollback option.
- Terminator-style tabs + binary split tree, broadcast input,
  Terminator-compatible keybinds incl. Shift+Arrow resize.
- 512 bundled Ghostty themes (default **TokyoNight Night**); bundled
  JetBrains Mono Nerd Font; Ghostty-syntax config with live reload.
- Regex search overlay; mouse selection + wheel scroll.
- Inline images: Sixel, kitty graphics, iTerm2 (OSC 1337).
- Hyperlinks: OSC 8 + URL autodetection, Ctrl/Cmd-click to open.
- Mouse-reporting passthrough (X10 + SGR 1006).
- Shell integration (OSC 133) + jump-to-prompt.
- Session save/restore (tab/split tree + per-pane cwd).
- SSH multiplexing (launcher + session-persisted SSH tabs).
- MIT licensed; CI matrix; docs with citations + mermaid diagrams.
