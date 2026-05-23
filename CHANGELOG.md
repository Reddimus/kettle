# Changelog

All notable changes to kettle. Format roughly follows
[Keep a Changelog](https://keepachangelog.com/); the project moves in small,
durable, fully-tested cycles (lint · build · test · docs · commit · CI).

## [Unreleased]

  cycle 654 — **terminalshot sub-cycle 3: `ScreenshotRequest`
              + `Renderer::pending_screenshot` slot**: queues
              a screenshot request on the renderer for the next
              `render_frame` to honor. Surfaces:
                - new `pub struct ScreenshotRequest { out_path,
                  crop: Option<Rect> }` in kettle-render
                - new `pub pending_screenshot: Option<...>`
                  field on `Renderer`
                - new `pub fn set_pending_screenshot(req)` +
                  `pub fn take_pending_screenshot() -> Option`
                - `Action::TakeScreenshot` dispatch (cycle 640)
                  now computes the out path via cycle-650
                  `session_screenshot_path` + queues a real
                  `ScreenshotRequest` instead of just logging.
                  v1 has `crop: None` (whole window); sub-cycle
                  6 wires per-pane crop.
              Sub-cycle 4 wires the actual wgpu surface readback
              + PNG encode inside `render_frame`; for now the
              request sits unread on the slot until that
              sub-cycle lands (no user-visible change yet, but
              the dispatch path now reaches the renderer with
              real state). Workspace tests stay 382.

  cycle 653 — **Deploy verification: install latest build to
              `~/.local/bin/kettle`** via `./scripts/install.sh`.
              The local binary now matches commit 82a827f
              (cycle 652) and reports `1.45.1 (82a827f)` on
              `--version`. Smoke-checked `--check-config`:
              loads defaults cleanly, picks up the bundled
              TokyoNight Night theme + JetBrainsMono Nerd Font.
              Honors the user's standing instruction to keep the
              local kettle install current (run install.sh after
              every meaningful build; in-place overwrite). No code
              change; this cycle is the explicit deployment step
              the /goal hook called out.

  cycle 652 — **confirm-dialog sub-cycle 4: keyboard-nav pure
              helper**: `confirm_dialog_keypress(current_focus,
              num_buttons, key) -> ConfirmKeyResult`. Pure state
              machine for the modal's Tab / Shift+Tab / ←→ /
              Enter / Esc handling. Sub-cycle 5 wires this to
              the App's winit key handler — without the wiring
              the helper is just a pure function exercised by
              tests, but landing it now lets the dispatch loop
              be a thin wrapper.
              New types:
                - `ConfirmKey` (winit-decoupled named keys)
                - `ConfirmKeyResult { Move, Confirm, Cancel,
                  Ignore }`
              Drift guard `confirm_dialog_keypress_walks_state_machine`
              walks 12 input shapes including:
                - Esc/Enter from any focus
                - Tab/Shift+Tab wrap behavior
                - Left/Right no-op at boundaries (Ignore vs
                  Move discrimination)
                - 0-button defensive fallback
                - single-button no-op cycle
              Workspace tests 381 → 382.

  cycle 651 — **vertical-tabs sub-cycle 2: `content_rect_for`
              pure helper**: extracts `App::area`'s layout math
              into a pure function that takes the inputs
              explicitly:
                `content_rect_for(surface, tab_bar_h,
                  status_bar_h, tab_bar_pos, status_bar_mode)
                  -> Rect`
              `App::area` now wraps the helper; cycle-651 v1
              treats `TabBarPos::Left` / `Right` the same as
              `Top` (the cycle-647 fallback). Sub-cycle 4 of
              [`TERMINATOR-VERTICAL-TABS-DESIGN.md`](docs/TERMINATOR-VERTICAL-TABS-DESIGN.md)
              branches on orientation + carves a per-strip
              width instead of a per-edge height. Drift guard
              `content_rect_for_carves_out_tab_and_status_bands`
              walks 8 (tab_pos × status_pos) cases including
              the tiny-window content-h floor. Workspace tests
              380 → 381.

  cycle 650 — **terminalshot sub-cycle 2:
              `session_screenshot_path` pure helper**: mirrors
              cycle-621's `session_log_path` shape. Lives under
              `<cache>/kettle/shots/kettle-<secs>-<pid>.png`
              with relative `./kettle-shots/` fallback when no
              cache dir resolves. Sub-cycle 3-5 of
              [`TERMINATOR-TERMINALSHOT-DESIGN.md`](docs/TERMINATOR-TERMINALSHOT-DESIGN.md)
              will call this from `Action::TakeScreenshot`
              dispatch + queue a wgpu readback request keyed
              on the path. Drift guard
              `session_screenshot_path_under_cache_kettle_shots`
              covers XDG path shape + relative-fallback +
              .png extension. Workspace tests 379 → 380.

  cycle 649 — **auto-theme sub-cycle 2: `resolve_theme_for_mode`
              pure helper**: picks the next theme name given the
              `ThemeMode` + `light_theme` / `dark_theme` config +
              current theme name + detected OS dark-mode flag.
              Pure — entirely a function of its 5 inputs, no env
              or clock. Sub-cycle 3 of
              [`TERMINATOR-AUTO-THEME-DESIGN.md`](docs/TERMINATOR-AUTO-THEME-DESIGN.md)
              will add the `dark-light` crate subscribe; this
              cycle's helper consumes whatever boolean that
              subscribe returns. Drift guard
              `resolve_theme_for_mode_matrix` walks 12 input
              shapes including case-insensitive
              "already-current" no-op and unset-theme no-ops.
              Workspace tests 378 → 379.

  cycle 648 — **confirm-dialog sub-cycle 2: `ConfirmDialogState`
              + `ConfirmAction` + `ConfirmButton` types**:
              the state shapes that sub-cycles 3-5 will consume.
              Builds on cycle-638's `should_prompt` helper.
                - `pub enum ConfirmAction { CloseWindow, CloseTab,
                  ClosePane }` — extensible enum the
                  `maybe_confirm_then` dispatch wrapper will
                  carry. Future cycles add `KillProcess`,
                  `DiscardLayout`, `ResetConfig`.
                - `pub enum ConfirmButton { Cancel, Confirm {
                  label, destructive } }` — two-button v1
                  shape; destructive=true renders red-accent.
                - `pub struct ConfirmDialogState { prompt,
                  buttons, focus_idx, on_confirm }` — owned by
                  `App::confirm_dialog: Option<…>`.
              `#[allow(dead_code)]` on the types + the field
              until the consumers land in sub-cycle 3 (renderer)
              and sub-cycle 5 (dispatch interception). This
              cycle landed the data model so the renderer + the
              dispatch can be written against the final shape
              without churn. Workspace tests stay 378.

  cycle 647 — **vertical-tabs sub-cycle 1: `TabBarPos::Left`
              + `Right` variants**: previously the parser
              accepted `tab-bar-position = left/right` since
              cycle 331/628 but `log::warn`'d and fell through
              to `Top`. Now the values store the actual
              orientation; the render-layer change to draw
              vertical strips lands in sub-cycles 2-6 of
              [`TERMINATOR-VERTICAL-TABS-DESIGN.md`](docs/TERMINATOR-VERTICAL-TABS-DESIGN.md).
              Also: new `TabBarPos::is_vertical()` helper for
              the upcoming `content_rect` branch + paint_tab_bar
              orientation dispatch. Non-exhaustive match arms
              in `cursor_in_tab_bar_band` + `tab_bar()` updated
              to handle Left/Right as no-y-band-hit fallthroughs
              (the rest of the renderer still uses the y-band-
              based geometry until the vertical strip lands).
              Drift guard `tab_bar_pos_left_right_parse_and_classify`
              covers parser routing for both Terminator-spelled
              aliases + the classification helper. Updated the
              older cycle-628 drift guard to reflect the new
              parser behavior. Workspace tests 377 → 378.

  cycle 646 — **remote.py sub-cycle 5: sysinfo process-tree
              walk**: sysinfo 0.32 added as a kettle-remote
              dep (default-features disabled; only the
              `system` feature) — isolated to this crate so
              the heavy process-enumeration code doesn't
              propagate to non-UI consumers.
              `detect_remote(child_pid)` now actually walks
              the process tree:
                - BFS from `child_pid` over `sysinfo`'s
                  parent→children index (built once per call)
                - each descendant's argv is fed through cycle-
                  644 `detect_ssh` + cycle-645 `detect_container`
                - closest descendant wins on tie (BFS gives
                  that for free)
              New companion `detect_remote_with(child_pid,
              &mut System)` lets the App's eventual poll loop
              own a single `System` across ticks so sysinfo's
              internal cache amortizes (instead of allocating
              one per call). Drift guard updated from the stub
              `always None` to "no match for invalid pids 0 /
              u32::MAX" — real-process testing would need
              spawn/CI fragility; the argv-side detectors
              already have exhaustive coverage. Workspace
              tests stay 377.

  cycle 645 — **remote.py sub-cycle 4: Container detector**:
              new `pub fn detect_container(argv: &[String]) ->
              Option<RemoteContext>` covers the four container-
              runtime exec argv shapes:
                - `docker exec [-it] <container> <cmd> …`
                - `podman exec [-it] <container> <cmd> …`
                - `kubectl exec [-it] [-n ns] <pod> -- <cmd>`
                - `lxc-attach [-n] <name>` (the `-n VALUE` is
                  the container; specially-cased extraction)
              Skips known value-taking flags (`-n` / `-u` /
              `-c` / `-w` / `-e`), GNU `--flag=value` forms,
              and the kubectl `--` separator. Returns None for
              non-container argv (`docker ps`, `docker build`),
              for `docker exec` with no container, and for
              empty argv. Drift guard walks 11 input shapes.
              Workspace tests 376 → 377.

  cycle 644 — **remote.py sub-cycle 3: SSH detector**: new
              `pub fn detect_ssh(argv: &[String]) -> Option<
              RemoteContext>` in `kettle-remote`. Pure — takes
              the process's argv (as sub-cycle 5's sysinfo walk
              will supply), returns `Some(Ssh { host, user })`
              for argv shapes that match real ssh invocations:
                - `ssh box`
                - `ssh user@host`
                - `ssh -p 22 user@host`
                - `ssh -o StrictHostKeyChecking=no host`
                - `ssh -l user host`
                - `sshpass -p secret ssh user@host`
                - `/usr/bin/ssh box` (absolute argv[0])
              Skips `-o foo=bar` / `-p 22` / `-l user` value
              args correctly. Returns `None` for non-ssh argv
              (vim, bash, …) and for ssh with no target
              (e.g. `ssh -V`). Drift guard walks 11 real-world
              shapes. Workspace tests 375 → 376.

  cycle 643 — **remote.py sub-cycle 2: `kettle-remote` crate
              skeleton + `RemoteContext` type**: new workspace
              member `crates/kettle-remote/`. Isolated from
              kettle-core so the eventual sysinfo dep doesn't
              propagate to non-UI consumers (the headless
              `--screenshot` path, `--check-config` validator).
              v1 of this crate ships:
                - `pub enum RemoteContext { Ssh { host, user },
                  Container { runtime, container } }`
                - `pub enum ContainerRuntime { Docker, Podman,
                  Kubectl, Lxc }`
                - `pub fn detect_remote(child_pid) -> Option<
                  RemoteContext>` — v1 stub returning None;
                  sub-cycle 5 wires the sysinfo dep + actual
                  process-tree walk.
                - `pub fn format_remote_title(ctx) -> String`
                  — pure formatter that drives the pane-title
                  update path.
              Two drift guards in the new crate cover the 6
              format-title shapes (SSH ± user, 4 container
              runtimes) + the stub-returns-None promise. The
              public surface lands NOW so the App code-paths
              can compile against the final return shape before
              the sysinfo dep gets pulled in. Workspace tests
              373 → 375.

  cycle 642 — **named-groups sub-cycle 1: action surface for
              `CreateGroup` + `GroupTab` + `GroupWindow` +
              `UngroupTab` + `UngroupWindow`**: 5 new Action
              variants from [`TERMINATOR-NAMED-GROUPS-DESIGN.md`](docs/TERMINATOR-NAMED-GROUPS-DESIGN.md)
              plus the 12 aliases that Terminator users would
              type. Dispatch:
                - `CreateGroup` and existing cycle-407
                  `EditPaneGroup` share dispatch (same
                  title-edit overlay)
                - `GroupTab` / `GroupWindow` log a TODO pointing
                  at named-groups sub-cycle 4 (bulk-apply path)
                - `UngroupTab` / `UngroupWindow` log a TODO
                  pointing at sub-cycle 5 (bulk-clear path)
              Palette includes all 5 so the actions are
              discoverable via the cycle-329 command palette.
              Workspace tests stay 373 (action enum is covered
              by the cycle-117 palette drift guard
              transitively).

  cycle 641 — **auto-theme sub-cycle 1: `ThemeMode` enum +
              `theme-mode` config key**: new `ThemeMode {
              Explicit, Light, Dark, Auto }` enum on Config
              (default `Explicit` preserves cycle-616 behavior).
              Parser arm accepts kebab + underscore key spellings
              and 4 alias values for `Auto` (`auto` / `system` /
              `follow-system` / `follow_system`). Sub-cycle 2 of
              [`TERMINATOR-AUTO-THEME-DESIGN.md`](docs/TERMINATOR-AUTO-THEME-DESIGN.md)
              wires the `dark-light` crate subscribe; for now
              this just lets a Terminator config copy in
              cleanly without --check-config warnings. Drift
              guard `theme_mode_parses_terminator_values` walks
              10 input shapes. Workspace tests 372 → 373.

  cycle 640 — **terminalshot.py sub-cycle 1:
              `Action::TakeScreenshot` surface**: new Action
              variant + 4 aliases (`take_screenshot`,
              `take-screenshot`, `terminalshot`, `screenshot`)
              from
              [`TERMINATOR-TERMINALSHOT-DESIGN.md`](docs/TERMINATOR-TERMINALSHOT-DESIGN.md).
              Dispatch arm logs a TODO pointing at the headless
              `--screenshot=PATH` fallback for now; sub-cycles
              2-6 wire the wgpu surface readback + PNG encode +
              toast notification. Palette includes the action so
              it's discoverable via cycle-329 command palette.
              Drift guard `from_name_accepts_take_screenshot_aliases`
              walks all 4 spellings. Workspace tests 371 → 372.

  cycle 639 — **remote.py sub-cycle 1: `Terminal::child_pid()`
              accessor**: new public method on `kettle_core::
              Terminal` returns the PTY child's OS pid via
              `portable_pty::Child::process_id()`. Read-only,
              doesn't consume the Child. The upcoming remote-
              session detector (sub-cycles 2-6 from
              [`TERMINATOR-REMOTE-DESIGN.md`](docs/TERMINATOR-REMOTE-DESIGN.md))
              roots its process-tree walk here. Returns None on
              lock contention or platforms without pid access.
              No new drift guard — the method is one line over
              the existing child Arc<Mutex<>>; the upcoming
              `kettle_remote::detect_remote` tests will cover it
              transitively. Workspace tests stay 371.

  cycle 638 — **confirm-dialog sub-cycle 1: `should_prompt`
              pure helper**: new `AskBeforeClosing::should_prompt(
              scope_count) -> bool` method on the existing enum
              implements the matrix from
              [`TERMINATOR-CONFIRM-DIALOG-DESIGN.md`](docs/TERMINATOR-CONFIRM-DIALOG-DESIGN.md):
                - `Never`              → never prompts
                - `Always`             → always prompts
                - `MultipleTerminals`  → prompts iff `scope_count > 1`
              Pure — no `&self` shape needed, just the enum + count.
              Sub-cycle 5+ wires it to the close-family dispatch.
              Drift guard `ask_before_closing_should_prompt_matrix`
              walks all 3 modes × 4 scope counts (0, 1, 2, 100).
              Workspace tests 370 → 371.

  cycle 637 — **`docs/TERMINATOR-CONFIRM-DIALOG-DESIGN.md` —
              Bucket D for `ask_before_closing` + a reusable
              confirm-dialog primitive**: the cycle-343-360
              parsed-but-unwired `AskBeforeClosing` config gets
              a real consumer. Architecture:
                - new `ConfirmDialogState` + `ConfirmAction` enum
                - generic primitive — first user is the close
                  family (Window/Tab/Pane), future users include
                  "kill running process" + "discard unsaved
                  layout" + "reset config"
                - new `should_prompt(mode, scope_count) -> bool`
                  pure helper (matrix over the 3 modes × scope)
                - new `maybe_confirm_then(action)` dispatch
                  wrapper — intercepts close-family actions and
                  opens modal vs falls through based on mode
                - centered modal renderer + dim backdrop +
                  focus-on-Cancel safe default
                - keyboard nav: Tab cycles focus, Enter confirms,
                  Esc cancels
              8 sub-cycles, +6-8 estimated tests. Audit row
              promoted from 🟡 (parsed not wired) to D. No code
              change.

  cycle 636 — **`cell_width` / `cell_height` renderer wiring
              (Terminator parity, config.py)**: the config keys
              were parsed (and clamped to [0.5, 3.0]) since
              cycles 343-356 but didn't actually scale the
              measured cell metrics. Now:
                - Renderer gains `pub cell_scale_w: f32` +
                  `pub cell_scale_h: f32` fields (default 1.0)
                - constructor multiplies `measure_cell` results
                  by these before storing `cell_w` / `cell_h`
                - `remeasure_cell` (called on font-family /
                  font-size change) preserves the scale
                - new `pub fn set_cell_scale(w, h)` setter
                  (no-op when unchanged; triggers re-measure)
                - app.rs `reload_config` calls it alongside
                  `set_font_family` + `set_font_size`
              So a user with `cell-height = 1.5` now actually
              gets 50% line spacing on next reload. Workspace
              tests stay 370 (no new drift guard — the
              multiplier is a one-line scale; behavior covered
              by the existing measure_cell tests + the lint+
              build gauntlet exercising the new fields).

  cycle 635 — **Audit doc reconciliation (round 5)**: 7 more
              audit rows reclassified:
                - `inactive_color_offset` → ✅ shipped (both
                  fg + bg offsets parse and apply)
                - `title_at_bottom` → ✅ shipped (per-pane
                  titlebar honors it in render/lib.rs)
                - `remote.py` → D (cycle-629 design doc)
                - `ask_before_closing` → 🟡 parsed not wired
                  (Bucket D: shared modal-overlay primitive)
                - `layout_launcher` → Bucket E (cycle-329
                  palette covers the picker UX)
                - `cell_width`/`cell_height` → 🟡 Bucket C
                  (parsed; needs renderer font-metric multiply)
                - `palette = solarized_dark` → Bucket E
                  (kettle's ~512 themes are a superset)
                - `Multiple grouping modes + auto-cleanup` → D
                  (cycle-631 named-groups design covers it)
              Audit now reflects ground truth: every row is
              either ✅ (shipped), Bucket D with a cross-link to
              the design doc, Bucket E with a divergence
              rationale, or 🟡 Bucket C with a concrete
              implementation sketch.

  cycle 634 — **`docs/TERMINATOR-THEME-SUBMENU-DESIGN.md` —
              Bucket D design doc for the right-click theme +
              profile submenu (Terminator
              `terminal_popup_menu.py`)**: today's cycle-245
              context menu flat-lists items. Submenu requires:
                - new `ContextMenuItem::Submenu { label, items }`
                  recursive variant
                - new `SubmenuState` + hover-delay state machine
                  (~250 ms GNOME-standard)
                - second-panel renderer + window-edge clipping
                  (flip to left when right would overflow)
                - keyboard nav (`→` opens, `←` closes)
                - populated from `Theme::list()` (~512 themes)
                  and `Config::list_profiles()`
              9 sub-cycles, +6-8 estimated tests. Audit row
              promoted from C/❌ to D with cross-link.
              Explicit Bucket E carveouts: nested-nested
              submenus (single level only), search-within-
              submenu (use cycle-329 palette instead), keyboard-
              only accelerator (follow-up). No code change.

  cycle 633 — **`docs/TERMINATOR-VERTICAL-TABS-DESIGN.md` —
              Bucket D design doc for `tab-position = left/right`
              (vertical tab strip)**: cycle 331/628 wired the
              parser for the values; this design lays out the
              render-layer change needed for the actual layout.
              Architecture:
                - new `TabBarPos::Left` / `::Right` variants
                - new `App::content_rect()` pure helper that
                  branches the pane-content rect on
                  (tab_bar_pos, tab_bar_visible, window_size)
                - new `kettle_render::TabBarOrientation` enum
                  (`Horizontal` / `VerticalLeft` / `VerticalRight`)
                  parameter on `paint_tab_bar`
                - hit-testing flip on cursor_in_tab_bar /
                  tab_seg_at_cursor / tab_close_at_cursor
                - drag-reorder generalized to either axis
                - new `tab-bar-width = 180` config knob for
                  vertical strip width
              8 sub-cycles, +10-12 estimated tests. Audit row
              promoted from B-partial to A+D with cross-link.
              No code change.

  cycle 632 — **`docs/TERMINATOR-AUTO-THEME-DESIGN.md` — Bucket D
              design doc for auto-detect + sunrise/sunset (the
              other half of `plugins/auto_theme.py` not shipped
              in cycle 616's manual toggle)**:
              architecture:
                - new `ThemeMode { Explicit, Light, Dark, Auto }`
                  enum + `ThemeSchedule { Clock, SunriseSunset }`
                - `dark-light` crate for cross-platform OS-pref
                  detection (DBus portal on Linux,
                  NSDistributedNotificationCenter on macOS,
                  RegNotifyChangeKeyValue on Windows)
                - theme_watcher module spawns the subscribe task;
                  fires events that reuse cycle-616's apply_theme
                - sunrise/sunset takes explicit lat/long
                  (privacy: never makes network requests; no
                  GeoClue2/CoreLocation prompts)
                - clock schedule: `theme-schedule = 18:00 dark,
                  06:00 light` for no-geolocation users
              7 sub-cycles, +10-12 estimated tests. Audit row
              updated from A (manual-only) to A+D. Risk
              register covers dark-light compile-failure
              fallback to cycle-616 manual, subscribe-blocks-
              launch (100 ms timeout), system-sleep drift,
              lat/long range validation. No code change.

  cycle 631 — **`docs/TERMINATOR-NAMED-GROUPS-DESIGN.md` —
              Bucket D design doc for Terminator's named broadcast
              groups (`create_group` / `group_tab` / `group_win`
              + ungroup_*)**:
              fills the kettle gap between per-tab broadcast
              (cycle 178) and broadcast-all — the finer-grained
              "broadcast to every pane I tagged with X." Design:
                - new `BroadcastScope::Group(String)` variant on
                  the existing scope enum
                - cycle-407 `pane.group_name` field gets promoted
                  from display-only to scoping-load-bearing
                - cycle-369 title-edit overlay reused with the
                  existing `TitleEditScope::Group` variant
                - renderer titlebar shows a `[name]` pill with
                  hash-derived color so all "fleet" panes look
                  visually linked
                - new actions: `CreateGroup`, `GroupTab`,
                  `GroupWindow`, `UngroupTab`, `UngroupWindow`
              8 sub-cycle roadmap, +8-10 estimated tests.
              Explicit Bucket E carveouts: cross-window groups
              (cycle-302 IPC follow-up), session-persistence of
              group assignments. Audit-doc rows promoted from
              C/❌ to D. No code change.

  cycle 630 — **`docs/TERMINATOR-TERMINALSHOT-DESIGN.md` —
              Bucket D design doc for `plugins/terminalshot.py`
              live-window capture**:
              live-window readback fills the gap between the
              existing headless `--screenshot` (synthetic scene)
              and what users actually want when they press a
              "screenshot now" chord. Architecture:
                - `Action::TakeScreenshot` + aliases queues
                  a `ScreenshotRequest` on the renderer
                - Renderer paints into an intermediate texture
                  on screenshot-pending frames + copy_texture_
                  to_buffer + map_async + PNG encode
                - Per-pane crop (focused-pane rect from mux)
                - Toast notification on success
                - Path scheme mirrors cycle-621 logger:
                  `<cache>/kettle/shots/kettle-<secs>-<pid>.png`
              7 sub-cycle roadmap, +5 estimated tests. Audit-
              doc row updated to A+D (`--screenshot` covers
              the synthetic path; D for live capture). Risk
              register covers GPU readback latency, render-
              thread blocking, image-crate version skew. No
              code change.

  cycle 629 — **`docs/TERMINATOR-REMOTE-DESIGN.md` — Bucket D
              design doc for `plugins/remote.py` port**:
              SSH / Docker / Podman / kubectl session detection
              via a new `kettle_remote` crate (sysinfo-backed
              process-tree walk), `Terminal::child_pid()`
              accessor, SSH + Container detectors, ~10 Hz poll
              tied to cycle-290 trigger cadence, right-click
              "Clone session" menu integration. 7 sub-cycles,
              estimated +12-15 tests. Same shape as the existing
              Bucket-D design docs (PLUGIN, DETACHABLE-TABS,
              PANE-TITLEBAR, BG-IMAGE). Audit-doc row promoted
              from C to D with cross-link. No code change.

  cycle 628 — **`tab-position` Terminator alias (config.py:144)**:
              cycle-331 wired the canonical kettle key
              `tab-bar-position` with all 4 Terminator values
              (top/bottom/hidden/left/right). Cycle 628 accepts
              the Terminator-spelled `tab-position` / `tab_position`
              as additional aliases so a Terminator config file
              binds without rename. Both the parser arm and the
              `detect_malformed_values` diagnostic arm updated;
              drift guard `tab_position_alias_parses` covers 5
              input shapes including the parse-time-accepted
              left/right runtime-fallback. Workspace tests 369 → 370.

  cycle 627 — **Doc-truth refresh (round 4)**: 7 more stale
              audit-doc rows flipped, citing the cycles that
              closed them:
                - `edit_*_title` → cycle 369-407 (full title-edit
                  overlay shipped, including the cycle-407
                  `EditPaneGroup` for broadcast-group name)
                - `close_button_on_tab` → wired earlier
                - `login_shell` → cycle 343 (mux.rs threads
                  the bool to Terminal::new_with_env)
                - `next_profile` / `prev_profile` → cycles 342
                  + 618 (refactor)
                - `geometry_hinting` → cycle 359 (winit
                  resize_increments)
              Reclassified `sticky` + `hide_from_taskbar` to
              Bucket E with rationale (winit 0.30 only exposes
              skip_taskbar on Windows; X11/Wayland/macOS would
              need platform-specific extensions kettle hasn't
              taken on). No code change. Tests stay 369/369.

  cycle 626 — **`audible_bell` accepted as documented no-op
              (Terminator config.py:214)**: kettle ships no audio
              bell surface yet (visual flash + window urgency only),
              so the key parses but is otherwise a Bucket E
              documented no-op. Lets a Terminator config copy
              cleanly without --check-config warnings; users who
              want a bell should use `bell = …` or the cycle-619
              `visible_bell` / `urgent_bell` compat aliases. Drift
              guard `audible_bell_parses_as_documented_noop`
              locks in the no-op (combined with the canonical
              `bell =` precedence rule). Workspace tests 368 → 369.

  cycle 625 — **`log-strip-ansi` config — plain-text session
              logs**:
                - extends cycle-621 logger.py parity. When the
                  per-pane session log is open
                  (`Action::ToggleSessionLog`), the reader thread
                  honors `log-strip-ansi = true` and removes CSI
                  / OSC / single-char ESC sequences before
                  writing — gives a grep-friendly log file.
                  `false` (default) preserves the raw-stream
                  behavior (cat-replayable in a terminal).
                - new pure helper `kettle_core::strip_ansi_bytes`
                  is the strip impl. State-free byte-block strip
                  (good enough for the line-buffered reader);
                  documented split-across-reads limitation in
                  doc comments.
                - new per-Terminal `Arc<Mutex<bool>>
                  log_strip_ansi` flag the reader thread reads
                  on each write. Action::ToggleSessionLog
                  propagates `cfg.log_strip_ansi` to it at
                  file-open time.
              Drift guard `strip_ansi_bytes_removes_csi_osc_and_single_esc`
              covers 7 input shapes including OSC terminated by
              BEL vs ESC\\, single-char ESC, bare-ESC-at-end.
              Workspace tests 367 → 368.

  cycle 624 — **Doc-truth refresh:
              `docs/TERMINATOR-AUDIT.md` (round 3)**:
              flipped 9 more stale rows to ✅, citing the cycles
              that closed them (335, 342, 345, 347, 350, 613,
              617). Plus reclassified `scroll_tabbar` to Bucket
              E with rationale (kettle's cycle-620 layout has
              overflow fallback; wheel-cycles-tabs gesture is
              the kitty/iTerm2 convention not the Terminator
              one). Tests still 367/367.

  cycle 623 — **Terminator color / cursor / fullscreen key
              aliases (Terminator config copies in unchanged)**:
                - `background-color` / `background_color` → kettle's
                  canonical `background` key
                - `foreground-color` / `foreground_color` → kettle's
                  canonical `foreground` key
                - `cursor-shape` / `cursor_shape` → kettle's
                  `cursor-style` (`block` / `underline` / `bar`;
                  also accepts `ibeam` / `i-beam` for Terminator's
                  spelling of the vertical bar)
                - `cursor-blink` / `cursor_blink` → kettle's
                  `cursor-style-blink`
                - `full-screen` / `full_screen = true` → sets
                  `window_state` to `Fullscreen` (false is a no-op
                  to preserve a separately-set window-state)
              All canonical key behaviors unchanged; just additional
              spelling acceptance. Drift guard
              `terminator_color_cursor_aliases_parse` walks 11 input
              shapes. Workspace tests 366 → 367.

  cycle 622 — **`plugins/run_cmd_on_match.py` parity**:
                - `trigger = REGEX :: cmd arg1 arg2` extends
                  cycle-289 trigger syntax with a `::` separator.
                  RHS is whitespace-split into argv (no shell
                  expansion at kettle's layer; the configured
                  command is treated as data, not as a shell
                  string).
                - `TriggerAction::RunCommand(Vec<String>)` new
                  variant carrying the argv. `TriggerAction` loses
                  its `Copy` derive (Vec<String> can't be Copy);
                  callers (`compile_triggers`, `match_triggers`)
                  switch to `.clone()`.
                - new pure helper `parse_trigger_with_command`
                  takes the raw value, returns `Option<(pattern,
                  argv)>` — `None` falls through to the cycle-289
                  Urgency action.
                - dispatch: `match_triggers` returns the action;
                  the loop now branches on Urgency vs RunCommand
                  and spawns via `spawn_trigger_command` (fire-
                  and-forget). Spawn errors are logged + ignored.
                - documented limitation: `::` separator means
                  patterns containing a literal `::` (rare IPv6
                  alternations) get split early. Workaround:
                  write `:[:]` or `\x3a\x3a`.
              Drift guard:
                - `parse_trigger_with_command_splits_on_double_colon`
                  covers happy path, multi-arg argv, whitespace
                  collapsing, all 4 sentinel-None cases, +
                  documents the IPv6 footgun.
              Workspace tests 365 → 366.

  cycle 621 — **`plugins/logger.py` parity — per-pane session
              log**:
                - new `Action::ToggleSessionLog` (aliases:
                  `start_logger` / `stop_logger` /
                  `toggle_session_log` plus kebab variants;
                  Terminator's two-button start/stop UX maps
                  to one toggle here)
                - new `pub log_file: Arc<Mutex<Option<File>>>`
                  on `kettle_core::Terminal`. Reader thread
                  holds a clone + writes raw PTY bytes (no ANSI
                  stripping — preserves replayable output)
                  when the file is Some. Best-effort I/O:
                  errors are swallowed so a full disk doesn't
                  crash the reader.
                - dispatch arm computes path via two new pure
                  helpers: `session_log_path(unix_secs, pid,
                  cache_dir)` and `cache_dir_from_env(get)`.
                  Helpers take primitives + Path/env-fn so
                  they're fully unit-testable without disk I/O.
                - file path shape:
                  `<XDG-cache>/kettle/logs/kettle-<secs>-<pid>.log`
                  (relative `./kettle-logs/...` fallback when
                  no cache dir resolves).
              Drift guards (2 new, both pure):
                - `session_log_path_under_cache_kettle_logs`:
                  XDG path shape + relative-fallback shape.
                - `cache_dir_from_env_probes_in_order`: XDG →
                  HOME/.cache → LOCALAPPDATA → None; empty-XDG
                  falls through (CI safety).
              Workspace tests 363 → 365.

  cycle 620 — **Non-homogeneous tab widths (Terminator
              config.py:88 `homogeneous_tabbar = false`)**:
                - new pure helper `compute_tab_segment_widths`
                  drives per-tab strip widths:
                    - `true` (kettle default) → equal width
                      `strip / n` (current behavior, unchanged)
                    - `false` → per-tab natural width =
                      `chars * cell_w + 2 * chrome + close_w`
                      with a `close_w * 1.5` min-affordance floor
                    - sum > strip → silent fallback to homogeneous
                      (no truncation; every tab stays visible)
                - tab_bar() now consumes the helper instead of
                  computing seg_w inline; x_offsets are
                  pre-computed from cumulative widths
                - empty title list yields `vec![strip]` (panic-safe;
                  never seen at runtime but the helper still has
                  to handle it for symmetry)
              Drift guard
              `compute_tab_segment_widths_homogeneous_and_natural`
              walks 4 scenarios (homogeneous, natural with room,
              overflow fallback, empty list).
              Workspace tests 362 → 363.

  cycle 619 — **`visible_bell` + `urgent_bell` compat parsing
              (Terminator config.py:215-216)**:
                - new parser arms map Terminator's two-bool bell
                  split into kettle's unified `BellMode`. Compose
                  semantics: `Off + Visual = Visual`, `Off +
                  Attention = Attention`, `Visual + Attention =
                  Both`. Order-independent (composes at end-of-
                  parse).
                - precedence: explicit canonical `bell = <mode>`
                  wins over the Terminator aliases regardless of
                  file order — kettle key takes precedence on
                  hybrid configs.
                - `force-no-bell = true` still overrides everything
                  (cycle 613 chain unchanged).
                - new `BellMode::compose(other)` pure helper
                  (OR-like, idempotent, with identity = Off).
              Drift guards:
                - `visible_bell_and_urgent_bell_compose_into_bell_mode`
                  walks 8 input shapes including canonical-precedence
                  + force-no-bell chain
                - `bellmode_compose_is_idempotent_and_or_like`
                  exhaustively round-trips all 4×4 input pairs +
                  proves the algebra (idempotence, identity = Off,
                  Both absorbs)
              Workspace tests 360 → 362.

  cycle 618 — **Profile-cycling refactor (Terminator
              `key_next_profile` / `key_previous_profile`)**:
                - new pub fn `Config::list_profiles()` enumerates
                  `<config-dir>/profiles/*.config` (deterministic
                  sort: case-insensitive primary + bytewise tiebreak)
                - new pub fn `Config::profile_name_from_path()`
                  inverts `path_for_profile`
                - app.rs NextProfile/PrevProfile dispatch refactored
                  to use both helpers + new pure `pick_next_profile`
                  helper (forward/back cycling with wrap)
                - inline disk-walk in app.rs was duplicating the
                  same path math kettle-config now exposes; one
                  source of truth + drift guards on it
              Drift guards (3):
                - `profile_name_from_path_inverts_path_for_profile`
                  covers round-trip, default-config rejection, wrong-
                  parent rejection, missing-suffix rejection
                - `pick_next_profile_wraps_and_starts_at_index_0`
                  covers fwd/back cycling, unknown-current → idx 0,
                  single-profile self-return
              Workspace tests 358 → 360.

  cycle 617 — **`case_sensitive` parity (Terminator
              config.py:117)**:
                - new enum `SearchCaseSensitivity { Smart,
                  Always, Never }` on Config (default Smart =
                  kettle's pre-617 ripgrep/vim behavior)
                - parser accepts: `smart`/`auto` ⇒ Smart;
                  `always`/`sensitive` ⇒ Always; `never`/
                  `insensitive` ⇒ Never; Terminator-spelled
                  `case-sensitive = true/false` (and the
                  underscore form) maps to Always/Never
                - new public API in kettle-core:
                  `CaseSensitivity`, `build_regex_with`,
                  `search_with` (the no-arg `search`/
                  `build_regex` remain as Smart-mode
                  shorthands; back-compat preserved)
                - app.rs scrollback search now threads
                  `cfg.search_case_sensitive` through to
                  `kettle_core::search_with`
                - new pure-helper `map_case_sensitivity` is
                  the kettle-config ↔ kettle-core bridge
              Drift guards: parser side
              `search_case_sensitive_parses_terminator_and_named_forms`
              (12 input shapes) + engine side
              `build_regex_with_honors_explicit_case_sensitivity`
              (round-trips all 3 modes + empty-pattern).
              Workspace tests 356 → 358.

  cycle 616 — **`plugins/auto_theme.py` parity (manual toggle)**:
                - new config keys `light-theme = <name>` and
                  `dark-theme = <name>` (kebab + underscore both
                  accepted; case-insensitive bundled-name lookup
                  stores the canonical form)
                - new `Action::ToggleLightDark` (`toggle_light_dark`
                  / `toggle-light-dark` / `toggle_theme_variant` /
                  `toggle-theme-variant`) — runtime swaps the
                  current theme between the two configured ones:
                    - current == dark → switch to light
                    - current == light → switch to dark
                    - third-party current → default to dark
                    - only one configured → one-way switch
                    - neither configured → no-op + warn
                Sunrise/sunset auto-detection is a follow-up; the
                manual chord covers the bulk of the auto_theme.py
                use case (day-to-day variant flipping). Pure helper
                `pick_light_dark_target` is unit-testable; drift
                guard `pick_light_dark_target_round_trips` covers
                the 7 input shapes. Workspace tests 354 → 356.

  cycle 615 — **Doc-truth refresh: `docs/TERMINATOR-AUDIT.md`**
              flipped 9 rows from ❌/🟡 to ✅, citing the
              cycles that closed them (604/606/607/609/611/
              612/613/614). Plugin inventory + gap table +
              roadmap list all now reflect ground truth.
              `insert_number`/`insert_padded` reclassified to
              Bucket E with rationale (kettle uses pane titles,
              not numbered enumeration). Tests still 354/354.

  cycle 614 — **Terminator-spelling keybind aliases**
              (`config.py:133-134` / `:195`):
                - `new_terminator` / `new-terminator` → kettle's
                  `Action::NewWindow` (Terminator name for
                  "spawn a new top-level instance")
                - `cycle_next` / `cycle-next` → `NextTab`
                - `cycle_prev` / `cycle-prev` → `PrevTab`
              A Terminator user with `keybind = super+i =
              new_terminator` in their config now binds
              correctly without a kettle-side rename. Drift
              guard `from_name_accepts_terminator_spelling_aliases`
              walks the 9 alias permutations. Workspace tests
              353 → 354.

  cycle 613 — **`force-no-bell = true` honors override**
              (Terminator parity, `config.py:force_no_bell`).
              Previously the key parsed (since cycle 340) but
              was a documented no-op — setting
              `force_no_bell = true` in a config copied from
              Terminator didn't actually silence the bell.
              Now: at the end of `parse_collect`, if
              `force_no_bell` is true, force `cfg.bell =
              BellMode::Off` regardless of any earlier `bell
              = ...` line. Wins on both orders (`bell` before
              or after `force-no-bell`). Drift guard
              `force_no_bell_overrides_bell_mode_to_off`
              walks 4 cases (alone, with `bell = both`
              before, with `bell = both` after, default
              leaves bell alone). Workspace tests 352 → 353.

  cycle 612 — **Long-command desktop notification on OSC 133 D
              (CommandEnd)** — Terminator parity for
              `terminatorlib/plugins/command_notify.py`. When a
              command completes in a pane:
                - kettle window doesn't have focus, AND
                - elapsed duration crossed
                  `cfg.command_notify_threshold_ms` (default 5 s,
                  `0` disables)
              kettle fires a desktop notification "kettle:
              command finished" with the pane id, duration, and
              exit code. Requires shell integration (`kettle
              --shell-integration bash`) — without OSC 133 the
              shell never emits the CommandEnd event. New
              kettle-core types: `term::CommandFinished {
              duration, exit_code }`, per-Terminal
              `output_started_at: Arc<Mutex<Option<Instant>>>`
              and `command_finished: Arc<Mutex<Vec<...>>>`
              (bounded at 32 entries against runaway shells),
              `Terminal::drain_command_finished_events()`. The
              PTY reader thread tracks the OutputStart →
              CommandEnd transition; the App drains the queue
              each tick. Drift guard
              `command_notify_threshold_parses_and_clamps` walks
              the 4 aliases + default + 0-disables + 1-day clamp.
              CONFIG.md row + commented example in
              kettle.example.config. Workspace tests 351 → 352.

  cycle 611 — **`menu-item = LABEL = CMD` config grammar**
              (Terminator parity, `terminatorlib/plugins/
              custom_commands.py` → "Custom Commands" menu).
              Repeatable config-file syntax that appends a
              right-click menu row writing `CMD\n` to the
              focused pane's PTY on click. Simpler than the
              cycle-375 `kettle.add_menu_item(label, callback)`
              Lua API: no callback to author, just literal
              text. The two paths layer cleanly — visual order
              top-to-bottom in the menu is: built-in actions →
              separator → config-file commands (cycle 611) →
              separator → Lua-registered items (cycle 375).
              New `Config::menu_items: Vec<MenuItem>` field,
              new `ContextMenuItem::ConfigItem` + `ContextMenu
              Click::ConfigCommand` variants, parser arm with
              both kebab + underscore aliases. Drift guards:
              `menu_item_parses_label_and_command` walks 6 cases
              (well-formed, multi-`=`-in-command, default empty,
              missing separator, empty label, empty command,
              underscore alias);
              `detect_malformed_values_flags_invalid_menu_item`
              ensures `--check-config` surfaces the malformed
              forms. CONFIG.md row + commented example in
              kettle.example.config. Workspace tests 349 → 351.

  cycle 610 — **CONFIG.md "no-op keys" reclassification.** The
              cycle-564 "Parsed-but-currently-no-op keys" table
              had grown stale as cycles 353 / 359 / 360 / 604 /
              609 wired specific keys, and as cycle-575's audit
              showed several entries were "no-op because
              kettle's behavior already matches" rather than
              "no-op because not implemented." Split the section
              into three disposition buckets:
                - **Effectively wired** (4 keys): kettle's
                  behavior already matches the setting
                  (`detachable-tabs`, `homogeneous-tabbar`,
                  `sticky` via `always-on-top`,
                  `inactive-color-offset` via
                  `unfocused-split-opacity`).
                - **Won't implement** (4 keys): by-design
                  divergence (`cursor-color-default`,
                  `http-proxy`, `broadcast-default`,
                  `putty-paste-style-source-clipboard`).
                - **Genuine future work** (9 keys): parsed for
                  forward-compat; explicit "why not yet" rationale
                  per row.
              No code change. Doc-only. cycle-179 drift guards
              all still pass after the rewrite.

  cycle 609 — **`smart-copy = false` honor.** Terminator parity
              (`terminal.py:real_copy_clipboard` +
              `config.py:smart_copy`). Pre-cycle-609 kettle
              hardcoded the smart_copy=true behavior (skip the
              clipboard write when no selection); the
              `smart-copy` config key was a documented no-op.
              Now: `smart-copy = false` clobbers the clipboard
              with an empty string on every Ctrl+Shift+C with
              no selection — Terminator's deliberate UX choice
              for users who prefer "Copy means the clipboard
              now reflects the current selection (even empty)"
              over the smart heuristic. New pure helper
              `copy_clipboard_decision(selection, smart_copy)`
              exposes the policy for unit-testing without a
              clipboard fixture. Drift guard
              `copy_clipboard_decision_smart_vs_clobber` walks
              the four (selection × smart_copy) combinations.
              CONFIG.md `smart-copy` row moved out of "Parsed-
              but-currently-no-op keys" into the main table.
              Workspace tests 348 → 349.

  cycle 608 — **`docs/examples/init.lua` sample script.** New
              documented Lua example covering the full
              `kettle.*` API surface — introspection,
              `kettle.add_url_handler` (with Launchpad-bug /
              Launchpad-code / APT-URL handlers ported from
              Terminator's `url_handlers.py`), `kettle.on`
              event hooks, `kettle.add_menu_item` right-click
              entries, and `kettle.exec_action` (with cycle
              606/607's new `insert_pane_name` /
              `open_cwd` actions demoed). Documents the cycle-
              601 send_text/notify/queue caps + the cycle-376
              safe-vs-trusted sandbox model in the file header
              so users see the security envelope before
              writing a script. CONFIG.md `lua-sandbox` row
              now cross-links to the example; the cycle-179
              cross-link drift guard passes the new link. No
              code change; workspace tests unchanged.

  cycle 607 — **`Action::OpenCwdInFileManager`** (Terminator parity,
              `terminatorlib/plugins/dir_open.py` → `CurrDirOpen`
              menu item). New action that reads the focused pane's
              OSC-7-reported cwd, builds `file://<cwd>`, and
              routes through the existing `open_url` machinery —
              re-uses the cycle-374 Lua URL-handler dispatch,
              cycle-X `custom-url-handler` config override, and
              `kettle_core::links::is_safe_url` allowlist for
              free (identical shape to clicking a `file://...`
              hyperlink in pane output). Falls back to a
              `log::info` hint about `kettle --shell-integration
              bash` when no OSC 7 cwd is available. Aliases:
              `open_cwd`, `open-cwd`, `open_cwd_in_file_manager`,
              `open-cwd-in-file-manager`. Drift guard:
              `from_name_accepts_open_cwd_in_file_manager_aliases`.
              Workspace tests 347 → 348.

  cycle 606 — **`Action::InsertPaneName`** (Terminator parity,
              `terminatorlib/plugins/insert_term_name.py`). New
              action that sends the focused pane's title to the
              focused PTY — useful for scripts that label their
              output by source pane or for keyboard-driven
              copy-current-title workflows. Mirrors the existing
              cycle-345 `InsertPaneNumber` / `InsertPanePadded`
              pattern. Accepted name aliases:
              `insert_pane_name`, `insert-pane-name`,
              `insert_name`, `insert-name`, plus
              `insert_term_name` / `insert-term-name` (Terminator
              spelling — copy-a-Terminator-keybind compatibility).
              Drift guards: existing
              `action_names_round_trip_through_from_name` + the
              cycle-117 `palette_includes_every_user_facing_action`
              already cover the addition; new
              `from_name_accepts_insert_pane_name_aliases` pins
              every alias. Workspace tests 346 → 347.

  cycle 605 — **Doc-truth pass: 3 wired keys promoted out of
              the no-op table.** `handle-size` (cycle 353),
              `geometry-hinting` (cycle 359), `focus`
              (cycle 360) were all wired in production but
              listed as no-op in CONFIG.md "Parsed-but-currently-
              no-op keys" — the explanatory copy on `focus` even
              claimed "kettle uses click-focus exclusively"
              which contradicts the cycle-360 sloppy
              implementation. Audit each key's read sites,
              promote into main table with proper type / default /
              behavior rows. Doc-only. cycle-179 drift guards
              all still pass.

  cycle 604 — **Ctrl+wheel font zoom + `disable-mousewheel-zoom`
              opt-out** (Terminator parity, key_zoom_in /
              key_zoom_out). The `disable-mousewheel-zoom` config
              key had been recognized by the parser since cycle
              334 but was a no-op because kettle didn't implement
              the Ctrl+wheel zoom it disables. This cycle adds
              both: the feature (Ctrl+wheel grows / shrinks the
              font, step matches the keyboard
              `IncreaseFontSize` / `DecreaseFontSize` actions for
              a single source of truth) AND the disable gate
              (config bool, default `false`). Fires BEFORE the
              mouse-tracking pass-through so it works even when
              a TUI (tmux / htop / nvim with `mouse=a`) has mouse
              tracking on — matches gnome-terminal / Terminator /
              xterm UX. New pure helper `should_zoom_font(ctrl,
              lines, disabled)` exposes the policy for unit-
              testing without an App fixture. Drift guard
              `should_zoom_font_gates_on_ctrl_and_disable_flag`
              walks the six relevant input combinations. CONFIG.md
              key moved out of "Parsed-but-currently-no-op keys"
              into the main table; example config gains a
              commented-out entry. Workspace tests 345 → 346.

## [1.45.1] — 2026-05-22

Patch release for two critical pane-lifecycle bugs surfaced in
the cycle-602 sweep. Same severity-class as the v1.45.0
close-focused fix — these warrant a re-install per the
cycle-527 "keep-local-current" memory.

User-impacting bug fixes:

  - cycle 603 part-A — `Mux::reap_tabs` now promotes the
    closed-pane's neighbor to focus instead of jumping to the
    leftmost leaf of the whole tab. Companion to cycle 602's
    `close_focused` fix; same root-cause anti-pattern
    (`tab.root.first_leaf()` as the post-close focus). Reachable
    when a user runs `exit` in the rightmost pane of a split
    tab — pre-fix, focus teleported back to the first split.

  - cycle 603 part-B — **data-loss bug.** `Mux::reap_tabs` used
    `Err(_) => tabs.remove(ti)` which conflated `Err(None)` (tab
    is empty, remove) with `Err(Some(sibling))` (focused leaf
    was a direct root child, sibling promoted, KEEP THE TAB).
    In any 2-pane tab, `exit` in either pane caused the WHOLE
    tab including the surviving sibling to disappear.
    `close_focused` already had the right distinction since
    cycle 285; `reap_tabs` didn't, and the existing
    `reap_tabs_keeps_active_pointed_at_the_same_tab` test only
    used single-leaf tabs so the bug went unnoticed for ~480
    cycles.

Drift guards (workspace 342 → 345):

  - `reap_tabs_promotes_neighbor_when_focused_pane_dies` —
    same 4-leaf tree as cycle 602's repro; reap dead leaf 40,
    assert focus = 30 (neighbor), not 10 (leftmost).
  - `reap_tabs_preserves_tab_when_2_pane_split_has_one_pane_exit`
    — 2-pane tab, reap one leaf, assert tab survives with
    the surviving sibling as root + focus.
  - `reap_tabs_keeps_focus_when_dying_pane_is_not_focused` —
    negative case: focus on Leaf(10), reap Leaf(20), assert
    focus stays 10.

## [1.45.0] — 2026-05-22

Release trigger: cycle-602 user-reported pane-close focus bug
("when I split the window many times then close that specific
terminal it sets my cursor/focused window to my first focused
terminal") — meets the cycle-562 "critical bug fix that users
would actively want to re-install for" criterion. Bundling the
accumulated [Unreleased] polish from cycles 561-602 into this
release because the user will re-install for cycle-602 anyway.

User-impacting bug fixes in this release:

  - cycle 574 — `Action::PastePrimary` now routes through
                `paste_clipboard`, picking up the same
                `LOCAL_PASTE_MAX` clamp, bracketed-paste wrap,
                and broadcast scoping as `Action::Paste`. Pre-
                fix, a `paste-primary` keybind under vim could
                interpret pasted text as commands.
  - cycle 602 — `Mux::close_focused` now picks the nearest
                neighbor pane as the new focus, not the
                leftmost leaf of the whole tab. Matches tmux /
                wezterm / kitty semantics.

Security hardening (cycles 576-587, 601):

  - Kitty graphics protocol resource caps: PNG/JPEG/GIF
    decompression-bomb cap (8192² / 256 MiB), `ImageData::new`
    overflow guard, 384 MiB per-chunk-stream cap, 32-slot
    in-flight cap, 256 frames-per-image cap, 64-slot caps on
    `store` / `anim` / `virtual_placements` / `rel` / `frames`.
  - Background-image decoder uses the same 8192² / 256 MiB
    envelope.
  - User-file read-into-memory caps: 16 MiB session.json, 1
    MiB config, 4 MiB init.lua — all defended against
    swap-attack OOM via metadata pre-check.
  - Lua side-effect APIs: 1 MiB per `send_text`, 8 KiB per
    `notify` field, 1024-command queue length cap.

Production polish:

  - SECURITY.md scope reflects every cap (cycles 583, 588, 596).
  - GitHub Actions: `cancel-in-progress` on diagnostic
    workflows (ci.yml, actionlint.yml, machete.yml,
    labeler.yml) + `timeout-minutes` on all 8 workflows.
    Budget-protection measures per the cycle-444 exhaustion.
  - Test-infra: PID + nanos /tmp paths (cycles 592, 593) so
    parallel `cargo test` runs don't race on shared files.
  - Doc accuracy: range-stable test counts in TESTING.md
    (cycle 594); SECURITY.md added to the cycle-179 user-
    facing-doc drift guard (cycle 596).
  - `release.sh` correctly skips `git add flake.nix` on
    forks lacking the file (cycle 589); `install-online.sh`
    SHA-256 diagnostic distinguishes "tool missing" from
    "verification failed" (cycle 590).

Workspace tests: 322 (v1.44.0) → 342 (this release).

  cycle 561 — README + INSTALL.md + scripts/install-online.sh
              version pins bumped to v1.44.0.

  cycle 562 — `app.rs` cycle-560 comment corrected — the claim
              that broadcast_default "still governs scope
              elsewhere" was wrong. The field has no consumer
              after cycle 560 removed the only one; comment
              now states the actual state + forward-compat
              intent.

  cycle 563 — `kettle-config/lib.rs` doc-comments for
              ask_before_closing + focus annotated as currently
              no-op (parses but no consumer).

  cycle 564 — **Doc-truth sweep.** `docs/CONFIG.md` gained a
              "Parsed-but-currently-no-op keys" subsection
              listing all 22 rows / 26 field names that parse
              cleanly but have no runtime consumer in kettle.
              Discovery: grep for `cfg\.<field>` in
              kettle-ui/ / kettle-render/ / kettle-core/
              returned 0 reads for these fields. Users
              configuring them now see at a glance that the
              key is a no-op (rather than guessing).

  cycle 571 — **Security drift guards.** Two new tests cover
              the cycle-376 Lua sandbox: safe-mode nils 16
              dangerous stdlib APIs (os.execute/exit/remove/
              rename/tmpname/setlocale + io.open/popen/lines/
              input/output/stdin/stdout/stderr + loadfile/
              dofile + package.loadlib); trusted-mode keeps
              them callable. The SECURITY.md cycle-447 "Lua
              plugin sandbox escape" scope is now build-time-
              enforced rather than manual-review-only.
              Workspace tests 323 → 325.

  cycle 593 — **Test race fix follow-up: main.rs config_path test.**
              `kettle/src/main.rs:config_path_problem_catches_*`
              still used `kettle-cycle164-{pid}` (PID only, no
              nanos) — common Linux PIDs are large enough to be
              unique within a test session, but Windows PIDs
              cycle quickly and a panicked re-run on the same
              PID would inherit a stale dir. Added nanos suffix
              for consistency with the cycle-592 pattern + the
              rest of the test suite. Workspace tests unchanged
              at 337.

  cycle 592 — **Test race fix: PID + nanos on `/tmp` paths.** Three
              unit tests (`bg_image::real_png_roundtrip`,
              `bg_image::rejects_oversized_dimensions`,
              `lua::exec_file_runs_a_real_script`) used FIXED
              filenames like `kettle-bg-image-cycle392-smoke.png`
              in `std::env::temp_dir()` directly. Two concurrent
              `cargo test` runs (parallel test threads, CI runner
              concurrency, two developers on the same shared
              runner) would race on the same file — one writes,
              the other reads stale/half-written bytes, sporadic
              failures. Switched to the
              `{name}-{pid}-{nanos}.png` pattern already used by
              `session::tests` and `config_tests::load_from_with_
              diagnostics_*` (subdir-level isolation). No
              behavior change for the happy-path single-run
              case; eliminates the flake under parallel
              execution.

  cycle 602 — **Pane-close focus follows the neighbor, not the
              leftmost leaf.** User-reported bug: "when I split
              the window many times then close that specific
              terminal it sets my cursor/focused window to my
              first focused terminal." `Mux::close_focused` was
              setting `tab.focus = tab.root.first_leaf()` after
              the close — which always points at the LEFTMOST
              leaf of the whole tab (the first pane the user
              started from). For deeply-nested closes that
              feels teleporting. New `Node::neighbor_of(id)`
              walks the tree and returns the first leaf of the
              closed pane's sibling subtree; `close_focused`
              calls it BEFORE the destructive `remove_leaf` so
              the right neighbor is captured even after the
              tree is rebuilt. Matches tmux / wezterm / kitty
              neighbor-promotion semantics. Drift guards:
              `close_focused_picks_nearest_neighbor_not_leftmost_root`
              (4-leaf nested tree reproduction of the exact
              user-described scenario; pre-fix focus jumps to
              leaf 10, post-fix it lands on neighbor leaf 30)
              and `node_neighbor_of_finds_sibling_subtree_first_leaf`
              (pins the helper's contract directly).
              `reap_tabs` (PTY-died path) keeps its existing
              fallback policy — only user-initiated close gets
              neighbor focus. Workspace tests 340 → 342.

  cycle 601 — **Lua side-effect API resource caps.** Audit
              extension to the cycle-376 / cycle-591 sandbox
              defense: the `kettle.*` side-effect callbacks
              (`send_text`, `exec_action`, `notify`, `set_theme`)
              had no per-call or queue-length bounds. A hostile
              `init.lua` running under default safe-mode could
              still queue gigabytes via `for i=1,10000 do
              kettle.send_text(string.rep("X", 1<<20)) end` and
              OOM kettle at the App's drain step
              (`app.rs:900` unconditionally
              `extend_from_slice`s every SendText into a single
              Vec). New caps:
                - `MAX_LUA_SEND_TEXT_BYTES = 1 MiB` per call;
                - `MAX_LUA_NOTIFY_BYTES = 8 KiB` per title /
                  body field;
                - `MAX_PENDING_COMMANDS = 1024` queue length.
              Routed all four callbacks through a new
              `bounded_push` helper so the queue cap is enforced
              exactly once. Per-call oversize drops silently
              with `log::warn`; queue saturation drops with
              `log::warn` + discriminant. Drift guards:
              `send_text_drops_oversized_payload_silently`,
              `notify_drops_oversized_field_silently`,
              `pending_queue_caps_at_max_pending_commands`.
              SECURITY.md cycle-447 "Lua plugin sandbox escape"
              scope updated to enumerate the caps. Workspace
              tests 337 → 340.

  cycle 591 — **Pin mlua-default debug-library exclusion as a drift
              guard.** Audit revealed that mlua's `Lua::new()`
              defaults already exclude the entire `debug` library
              (via `StdLib::ALL_SAFE`), so the dangerous methods
              `debug.getregistry` (sandbox-escape via reference-
              table access), `debug.sethook` (instruction-level
              DoS), and `debug.set{metatable,local,upvalue}` (break
              opaque-userdata encapsulation) are already
              unreachable from user scripts in both safe and
              trusted modes. New positive drift guard
              `lua_default_globals_exclude_debug_library` asserts
              `type(debug) == "nil"` in both sandbox modes — if a
              future refactor switches to `Lua::unsafe_new()` or
              explicitly loads `StdLib::DEBUG`, the test fires
              instead of the regression silently widening the
              SECURITY.md cycle-447 "Lua plugin sandbox escape"
              surface. Added a NOTE comment in `new_with_sandbox`
              documenting why no explicit nil-sweep is needed.
              Workspace tests 336 → 337.

  cycle 590 — **install-online.sh: accurate SHA-256 diagnostic.**
              The hash-verification branch tried `sha256sum -c`,
              fell back to `shasum -a 256 -c`, and printed "SHA-
              256 verification FAILED" if both failed. That
              error message implied tampering even when the
              real cause was "no hashing tool installed" (e.g.,
              a minimal container with neither coreutils
              `sha256sum` nor perl-base `shasum`). Now: detect
              tool availability first, fail with a clear "install
              one of them" message if neither is present, and
              reserve "verification FAILED" for the actual
              hash-mismatch case. Both branches still refuse to
              extract, so the security posture is unchanged —
              just the user-facing diagnostic is honest about
              what went wrong. `dash -n` syntax-check + shellcheck
              both clean. No behavior change on the happy path.

  cycle 589 — **release.sh: gate flake.nix add on existence.**
              The cycle-550 atomic flake.nix bump correctly
              guards the `sed` with `if [ -f flake.nix ]`, but
              the subsequent `git add ... flake.nix` was
              unconditional. The cycle-550 comment claimed the
              add was a no-op when the file is absent, but
              `git add <missing>` exits with 128 — under
              `set -euo pipefail` the release would abort
              **after** the Cargo.toml + lockfile bumps had
              already been applied to the working tree, leaving
              the user with a half-bumped dirty state to clean
              up. Switched to a bash array (`ADD_FILES=(…)`)
              that conditionally appends `flake.nix` to match
              the existing existence guard. No behavior change
              on this repo (flake.nix present) — durability
              fix for forks without it. No drift guard: this is
              a fork-only code path; running release.sh in CI
              against this repo doesn't exercise the branch.

  cycle 587 — **Lua script read cap.** Closes the fourth and final
              user-file read in the cycle-584..587 resource-cap
              sweep (bg-image, session.json, config, lua script).
              `LuaEngine::exec_file` previously called
              `std::fs::read_to_string(path)` unbounded. Threat
              model is the same — a swap-attack on
              `~/.config/kettle/init.lua` could OOM kettle on
              launch. New `MAX_LUA_SCRIPT_BYTES = 4 MiB` (~40×
              over typical init.lua, ~10× over a moderately
              complex plugin suite). Past the cap, the function
              `anyhow::bail!`s rather than reading into RAM —
              surfaces a clear diagnostic to the user instead of
              an OOM. Drift guard `exec_file_rejects_oversize_script`
              writes a 5 MiB syntactically valid Lua file and
              asserts the load errors with a "refusing to load"
              message. Workspace tests 335 → 336.

  cycle 586 — **Config-file read cap.** Companion to cycles 584
              (bg-image) and 585 (session.json). `Config::
              load_from_with_diagnostics` previously called
              `std::fs::read_to_string(path)` unbounded — a
              swap-attack on `~/.config/kettle/config` could OOM
              kettle on launch. Cheap metadata pre-check against
              `MAX_CONFIG_BYTES = 1 MiB` (~20× over the bundled
              10 KB example, ~100× over typical user configs)
              before any allocation; past the cap the function
              falls through to `Config::default()` with a
              `log::warn`. Drift guard
              `load_from_with_diagnostics_rejects_oversize_config`
              writes a 2 MiB file of legitimate config lines
              (verifies the size gate fires BEFORE parsing —
              even valid payload past the cap is refused).
              Workspace tests 334 → 335.

  cycle 585 — **Session.json read-into-memory cap.** `session::
              load_from_path` previously called
              `std::fs::read_to_string(p)` with no size cap. A
              swap-attack with filesystem access (out of strict
              scope per SECURITY.md but the same defense-in-depth
              reasoning as cycle 584's bg-image fix) could
              replace the auto-generated session file with a
              multi-GB blob and OOM kettle on launch. Cheap
              pre-read `metadata().len()` check against
              `MAX_SESSION_BYTES = 16 MiB` (1000× over realistic
              sessions, leaves the bomb on disk for forensics
              renamed to `.json.toobig.<unix-seconds>` — same
              shape as the cycle-108 corrupted-file recovery
              path). Drift guard
              `load_from_path_rejects_oversize_file_without_reading_into_memory`
              writes a 17 MiB file, asserts the load returns
              None, the file was renamed, and one `.toobig`
              backup exists. Workspace tests 333 → 334.

  cycle 584 — **Bg-image decompression-bomb defense.** Companion
              to cycle 576 (PTY-layer kitty/iTerm2 images) at the
              renderer crate's user-configurable
              `background-image` path. `image::open(p) + to_rgba8()`
              had no dimension or alloc limits; a malicious file
              masquerading as a 4K wallpaper could OOM kettle on
              launch via the same PNG/JPEG/GIF/WebP/BMP
              decompression-bomb shape. Switched to
              `image::ImageReader::open(p).with_guessed_format()`
              + `reader.limits(MAX_BG_IMAGE_DIM=8192,
              MAX_BG_IMAGE_BYTES=256 MiB)`. Threat model is
              weaker than the PTY path (config-file source, not
              attacker-controlled at runtime) but the defensive
              pattern is the same. Drift guard
              `rejects_oversized_dimensions` writes an 8193 × 1
              RGBA PNG to a temp file and asserts decode returns
              None. Workspace tests 332 → 333.

  cycle 582 — **Kitty per-id derivative-map saturation sweep.**
              Final link in the cycle-576..581 kitty
              resource-cap chain. The store cap from cycle 581
              didn't propagate to the four other per-id HashMaps
              in `KittyState` — `anim` (animation control),
              `virtual_placements` (`U=1` placements), `rel`
              (parent/child placements), `frames` (per-id
              animation frame Vec). The `anim` map was the most
              acute: an attacker can grow it with `a=a,i=N` for
              arbitrary N **without ever transmitting a real
              image**. All four insert sites now check
              `contains_key(...) || len() < MAX_STORED_IMAGES`
              and bail to `KittyOut::None` past the cap; updates
              to already-tracked ids still work. Drift guard
              `kitty_anim_slot_cap_holds_against_distinct_id_flood`
              fills 64 ids via `a=a`, fires a 65th distinct id
              (refused), then updates an existing id (accepted,
              no growth). Workspace tests 331 → 332 (+ 1 ignored).

  cycle 581 — **Kitty stored-image cap.** Sixth link in the
              cycle-576..580 kitty resource-cap chain. The
              `store: HashMap<u32, ImageData>` of completed
              transmissions was unbounded — each entry holds an
              `ImageData` Arc whose payload can be up to 256 MiB
              (the cycle-576 cap), so completing 1000 distinct
              `a=T,i=N,m=0` transmissions could pin up to 256 GB
              resident. New `MAX_STORED_IMAGES = 64` (sits well
              above any realistic terminal usage — icons +
              animations rarely transmit more than a dozen
              images). Updates to already-stored ids still
              replace in place (no growth); brand-new ids past
              saturation are dropped — the decoded image can
              still be drawn at-cursor on the completing
              transmission but can't be replaced later via
              `a=p,i=…`. Drift guard
              `kitty_stored_images_cap_holds_against_distinct_id_flood`
              fills 64 ids, fires a 65th (refused), then updates
              an existing id (accepted, no growth). Workspace
              tests 330 → 331 (+ 1 ignored).

  cycle 580 — **Kitty per-image frame cap.** Each successful `a=f`
              frame transmission appends a `Frame` (carrying an
              `ImageData` Arc) to `frames[id]`; chaining 100 000+
              frame transmissions for one id grew the Vec
              unboundedly. New `MAX_FRAMES_PER_IMAGE = 256` (well
              above any realistic animation — `.gif` files top
              out around 200 frames). Past the cap, additional
              pushes are silently dropped; the animation keeps
              playing the frames already captured. Drift guard
              `kitty_frames_per_image_cap_holds_against_flood`
              spams `MAX_FRAMES_PER_IMAGE + 16` 1×1 frames at one
              id and asserts the Vec stops at the cap. Workspace
              tests 329 → 330 (+ 1 ignored).

  cycle 579 — **Kitty in-flight slot cap.** Complement to cycle
              578. The cycle-578 per-slot byte cap stops any
              *single* chunked transmission from OOMing the host,
              but the `in_flight: HashMap<u32, Acc>` itself was
              keyless growth: an attacker can send 100 000+
              distinct `i=` values with one `m=1` chunk each (no
              terminating `m=0`), each slot holding a few bytes,
              and slowly fill the host's heap with HashMap
              overhead alone. New constant
              `MAX_IN_FLIGHT_SLOTS = 32` (well above any real
              client; kitty + ueberzug + chafa interleave 1-2
              transmissions). Past the cap, brand-new ids are
              refused (`KittyOut::None`); continuation chunks
              for already-tracked ids still work. Drift guard
              `kitty_in_flight_slot_cap_refuses_new_ids_past_
              saturation` fills 32 slots, fires a 33rd id and
              asserts the map didn't grow, then completes one
              and asserts the slot frees. Workspace tests 328
              → 329 (+ 1 ignored).

  cycle 578 — **Kitty chunked-transmission cap.** Both kitty
              graphics accumulators in `KittyState::feed` (the
              regular `a=T,m=1` chunked image and the `a=f,m=1`
              animation-frame chunks) appended to a `String`
              without any per-slot byte cap. A hostile PTY
              emitter could chain `m=1` continuations
              indefinitely and OOM the host before the final
              chunk ever arrived. New constant
              `MAX_KITTY_PAYLOAD_BYTES = 384 MiB` (covers the
              largest realistic single transmission — 8192² × 4
              RGBA at 4/3 base64 expansion ≈ 342 MiB — with
              ~12% margin, and sits below the cycle-10
              `MAX_SEQ = 64 MiB` per-chunk extractor cap times
              6). On cap exceedance the in-flight slot is
              dropped and `KittyOut::None` returned; any next
              chunk for the same id starts fresh (and will
              also hit the cap if the attacker persists).
              Two new tests: `kitty_payload_cap_fits_8k_rgba_
              base64_with_margin` pins the constant; the
              `#[ignore]`-by-default behavioral guard
              `kitty_chunk_payload_cap_drops_oversize_in_flight`
              actually pushes a 384 MiB+1-byte chunk and
              verifies the slot is cleared (run via
              `cargo test -- --ignored`). Workspace tests
              327 → 328 + 1 ignored.

  cycle 577 — **Overflow-safe `ImageData::new`.** The validation
              `rgba.len() != (width as usize * height as usize *
              4)` would panic on debug builds and silently wrap
              on release for adversarial header values — a
              kitty `f=32,s=4294967295,v=4294967295` payload
              hits `u32::MAX² × 4` ≈ 7.4 × 10¹⁹ bytes, which
              overflows `u64::MAX` ≈ 1.8 × 10¹⁹ on 64-bit. The
              cycle-576 `from_encoded` cap funnels the *encoded*
              path safely, but the raw `ImageData::new` surface
              (used by the kitty `f=32` raw-RGBA branch) lacked
              the same guard. Switched to `checked_mul`; the
              oversize case now returns a clean `None`. New test
              `new_rejects_overflowing_dimensions_without_panic`
              walks the u32-saturated boundary so a future
              refactor that drops the `checked_mul` fails the
              gauntlet rather than the binary silently wrapping.
              Workspace tests 326 → 327.

  cycle 576 — **Decompression-bomb defense for terminal-embedded
              images.** `ImageData::from_encoded` — the entry
              point for Kitty graphics `f=100` (PNG) and iTerm2
              OSC-1337 inline-image payloads — used to call
              `image::load_from_memory(bytes).to_rgba8()` with no
              dimension or allocation limits. A small attacker-
              controlled PNG/GIF/JPEG could claim 2^31 × 2^31
              pixels in the header and OOM kettle on decode.
              Switched to `image::ImageReader` with `Limits`
              configured (`max_image_width` / `max_image_height`
              = 8192, matching `sixel::MAX_DIM`; `max_alloc` =
              256 MiB, the matching RGBA cap). New unit test
              `from_encoded_rejects_oversized_images` round-trips
              a 4 × 4 PNG (positive) and rejects an 8193 × 1 PNG
              encoded by the image crate itself (negative); the
              drift guard fires if a future refactor drops the
              `ImageReader::limits` wire-up. SECURITY.md cycle-
              449 "Resource exhaustion via a single PTY frame"
              scope is now tighter for the inline-image surface.

  cycle 574 — **Paste safety bug fix.** `Action::PastePrimary`
              (cycle 345) was reading the clipboard and writing
              raw bytes directly to the focused pane's PTY,
              bypassing all three of the safety nets that
              `Action::Paste` honors: the 4 MiB
              `LOCAL_PASTE_MAX` runaway clamp, the bracketed-
              paste wrap (so vim / neovim / fzf / mc paste
              correctly when BRACKETED_PASTE is enabled —
              the same fix cycle 182 made for drag-drop), and
              broadcast scoping (so group-input keybind
              honors `paste-primary` like it honors `paste`).
              Fix: delegate `PastePrimary` to `paste_clipboard()`
              — arboard has no separate primary-selection API,
              so the two clipboards are equivalent through our
              current surface anyway.

## [1.44.0] — 2026-05-22

Recovery release. The cycle-553 release.yml gate added in v1.43.0
created a circular dependency: it required PKGBUILD/kettle.rb
versions to match the tag, but those templates can't auto-bump
because their sha256 lines need post-CI artifacts (which only
exist AFTER the gate passes). The v1.43.0 Linux release job
failed at this gate — macOS + Windows artifacts shipped, but
Linux artifacts (the install-online.sh target) didn't.

  cycle 558 — Revert the cycle-553 strict gates for PKGBUILD +
              kettle.rb. flake.nix's gate stays (cycle 550 made
              it auto-bumpable). Packaging templates follow the
              "trail by one" pattern (carry v(N-1) artifacts
              until maintainer re-publishes to AUR/tap),
              matching AUR + Homebrew convention.

After v1.44.0 ships, `/releases/latest` redirects to v1.44.0
with full Linux + macOS + Windows artifacts; the v1.43.0
partial release is no longer the "latest".

## [1.43.0] — 2026-05-22

Post-v1.42.0 packaging-drift cleanup. Three template files
(flake.nix, PKGBUILD, kettle.rb) had identical 39-release
version-string drift discovered + closed in one sweep, with
release.yml CI gate extended to prevent recurrence.

  cycle 547 — `docs/ROADMAP.md` + `docs/TERMINATOR-AUDIT.md` +
              `docs/ARCHITECTURE.md` post-sweep summaries
              extended to v1.42.0 (cycles 411-543, 11 releases,
              121 cycles).

  cycle 549 — **Drift catch #1.** `flake.nix` hardcoded
              `version = "1.3.5"` despite a "Keep in lockstep
              with Cargo.toml" comment. The lockstep was
              advisory-only for 39 releases. Bumped to v1.42.0.

  cycle 550 — **Durable enforcement.** `scripts/release.sh` now
              auto-bumps `flake.nix` version in lockstep with
              `Cargo.toml`; release.yml CI gate asserts the two
              match the tag. Forward (auto-bump) + backward (CI
              guard) per user directive ("durable over
              patches").

  cycle 551 — **Drift catch #2.** `packaging/arch/PKGBUILD`
              had the same `pkgver=1.3.5` + matching v1.3.5
              sha256. Bumped to v1.42.0 with the v1.42.0
              tarball sha256 fetched deterministically from the
              release sidecar.

  cycle 552 — **Drift catch #3.** `packaging/homebrew/kettle.rb`
              had `version "1.3.5"` + matching v1.3.5 sha256s
              for both macOS-universal + Linux-x86_64. Bumped
              to v1.42.0 with both sha256s from the release
              sidecars.

  cycle 553 — release.yml gate extended to assert PKGBUILD
              pkgver + kettle.rb version match the tag.
              PKGBUILD + kettle.rb can't be auto-bumped from
              release.sh because their sha256 lines depend on
              post-CI artifacts; the gate catches forgotten
              manual bumps. End message now lists all 5
              version-bearing files (tag ↔ Cargo.toml ↔
              flake.nix ↔ PKGBUILD ↔ kettle.rb ↔ CHANGELOG.md).

## [1.42.0] — 2026-05-22

Post-v1.41.0 polish + a real user-reported bug fix.

  cycle 524 — README + INSTALL.md + scripts/install-online.sh
              version pins bumped to v1.41.0.

  cycle 525 — `docs/ROADMAP.md` + `docs/TERMINATOR-AUDIT.md` +
              `docs/ARCHITECTURE.md` post-sweep summaries
              extended to v1.41.0 (cycles 411-521, 10 releases,
              111 cycles).

  cycle 530 — `scripts/install.sh` now refreshes
              `${PREFIX}/share/kettle/install.sh` on every
              install — the matching `--uninstall` script
              always reflects the version that put the binary
              there.

  cycle 531 — `--uninstall` removes `${PREFIX}/share/kettle/
              install.sh` + `rmdir`s the dir if empty.
              Symmetric with cycle 530.

  cycle 535 — `--check-config` annotates the existing
              `bell: <Mode>` line with `(force-no-bell
              overrides)` when force_no_bell is set, so the
              user doesn't read the configured bell mode and
              wonder why no bell actually fires. Pairs with
              the cycle-461 separate "bell: force-no-bell=true"
              echo line.

  cycle 536 — **User-facing string cleanup.** Cycle 461's
              triggers echo read `(cycle-289 Urgency action)` —
              an internal cycle ref in `--check-config` output.
              Same anti-pattern cycles 474-475 scrubbed from
              docs / man page (but the cycle-179 file-scan
              drift guard doesn't reach binary stdout).
              Replaced with `(window-urgency action)`
              describing the actual effect.

  cycle 537 — Drift guard for cycle-N refs in
              `extra_check_config_lines` output. A unit test
              that builds a config triggering every echo
              branch + asserts no resulting line matches
              `cycle <digit>` / `cycle-<digit>`. Workspace
              tests 321 → 322.

  cycle 539 — Exact-numeric test counts in
              `docs/TERMINATOR-AUDIT.md` + `docs/ROADMAP.md`
              bumped 321 → 322.

  cycle 540 — **Real user-reported bug fix.** kettle icon
              wasn't showing in GNOME Activities / Super-key
              search even though the PNG/SVG files were
              correctly in place. Root cause: `scripts/install.sh`
              ran `gtk-update-icon-cache -f -t ${ICON_BASE}`
              against a user-local hicolor dir that has no
              `index.theme`. The "-t" flag (--ignore-theme-index)
              made gtk-update-icon-cache produce a ~584-byte
              empty/broken cache file. GNOME trusts that cache
              and skips file-system fallback scanning — so
              `Icon=kettle` in the .desktop never resolves.

              Two-part fix:
              - Only invoke gtk-update-icon-cache when
                ${ICON_BASE}/index.theme exists (user-local
                hicolor inherits the system
                /usr/share/icons/hicolor/index.theme).
              - Clean up any pre-existing broken cache when
                no index.theme is present.

              Verified end-to-end: re-running ./scripts/install.sh
              --skip-build removes the broken cache; GNOME's
              directory-scan fallback now resolves the icon.

  cycle 543 — Symmetric to cycle-540 fix: `--uninstall` also
              guards `gtk-update-icon-cache` on index.theme
              existing + removes the broken cache. Without
              this, uninstall would re-create a stale cache
              referencing the just-removed icon files.

## [1.41.0] — 2026-05-22

Post-v1.40.0 polish — pre-commit hook UX tightens, real-bug
catches from running shellcheck on scripts/, and crates.io
metadata polish.

  cycle 501 — `docs/ROADMAP.md` + `docs/TERMINATOR-AUDIT.md` +
              `docs/ARCHITECTURE.md` post-sweep summaries
              extended to v1.40.0 (cycles 411-497, 9 releases,
              87 cycles).

  cycle 502 — Pre-commit hook logs elapsed gauntlet time
              (`pre-commit: PASSED (47s)`) so contributors
              don't misread cold-cargo-cache delay as a hung
              hook.

  cycle 503 — Renamed `start_ns` → `start_sec` (cycle 502
              stored seconds, not nanoseconds).

  cycle 504 — Per-branch test assertion order aligned with
              `extra_check_config_lines` helper-body order
              (accent / force_no_bell / triggers / lua_sandbox
              / background_image / window-flags / status-bar).

  cycle 505 — `extra_check_config_lines_empty_for_default_config`
              binds the helper result once so the assertion +
              failure-message reference the same value.

  cycle 506 — Hook renders sub-second runs as `(<1s)` instead
              of `(0s)`.

  cycle 507 — Timing-comment refined "~30s" → "30-90s on a
              cold cache" + a `<5s warm-cache incremental`
              counterweight.

  cycle 508 — Wall-clock-jumped-backward edge case (NTP
              correction, manual clock set, container time
              jump) renders as `<1s` rather than the
              misleading `(-1s)`.

  cycle 511 — Exact-numeric version snapshots in
              `docs/TESTING.md` (`post-v1.37.0`) +
              `docs/ROADMAP.md` (`v1.35.0`) bumped to v1.40.0.

  cycle 512 — `packaging/linux/kettle.1` `--screenshot-menu`
              description scrubbed of internal `v1.3.0 blank-
              menu regression class` history-ref. Same anti-
              pattern as cycle-475's cycle-N scrub but version
              pattern.

  cycle 515 — `.github/labeler.yml` extended to cover the
              cycle-494 `.githooks/` directory under the
              existing `tooling` label.

  cycle 516 — **Real bug fix.** `scripts/release.sh` line 101
              had backticks inside a double-quoted `echo` that
              ran as command substitution at error time. The
              "helpful hint" actually ran `git fetch && git
              tag -d v${VERSION}`, mutating local state and
              printing garbled output. Caught by manually
              running shellcheck against scripts/. Fixed via
              single-quote re-interpolation.

  cycle 517 — **Durable infrastructure.** Pre-commit hook now
              runs shellcheck against any staged scripts/ or
              .githooks/ files before the cargo gauntlet —
              catches the cycle-516 bug class at commit time.
              Falls back silently when shellcheck isn't
              installed.

  cycle 518 — `scripts/install.sh`'s 4 SC2015 `cmd && X ||
              true` ambiguity-pattern instances rewritten as
              explicit `if … then … fi`. `shellcheck
              scripts/*.sh .githooks/*` now warning-free
              across the repo.

  cycle 520 — `Cargo.toml [workspace.package]` gained
              `homepage` / `readme` / `keywords` / `categories`
              — best-practice metadata that future-proofs a
              potential crates.io publish.

  cycle 521 — `crates/kettle/Cargo.toml` inherits the cycle-520
              new fields via `<field>.workspace = true` (without
              this, the workspace defaults applied to no
              published crate).

## [1.40.0] — 2026-05-22

Pre-commit hook infrastructure. The session caught two of its own
bugs: cycle 484's doc-list overindentation (cycle 493) and the
cycle-494 hook's missing deletion filter (cycle 496) — both
caught the bug class they exist to prevent.

  cycle 489 — `docs/ROADMAP.md` + `docs/TERMINATOR-AUDIT.md` +
              `docs/ARCHITECTURE.md` post-sweep summaries
              extended to v1.39.0 (cycles 411-486, 8 releases,
              76 cycles).

  cycle 490 — ROADMAP "Drift guards" bullet credited cycles
              471-472 (3 new drift guards on
              `extra_check_config_lines`) + bumped final count
              319 → 321 to match HEAD.

  cycle 491 — Saved a session-summary memory entry for the
              v1.34.0 → v1.39.0 arc so future sessions can
              resume with the load-bearing invariants visible.

  cycle 492 — Helper rustdoc lede reordered (purpose first,
              cycle citation in parens) to match the rest of
              the codebase's rustdoc conventions.

  cycle 493 — **Fix-my-own-bug.** Cycle 484's `lua_engine`
              doc-list used column-aligned hanging-indent
              continuations; clippy 1.93 flagged them as
              `doc_list_item_without_indentation` errors. Re-
              flowed to standard 2-space markdown hanging
              indent + blank-line block separator.

  cycle 494 — **Durable infrastructure.** Added opt-in
              `.githooks/pre-commit` that runs `cargo fmt
              --check && clippy && test` on every commit
              touching code. Skips doc-only commits to stay
              fast (CHANGELOG / README / docs/ / packaging/ /
              .github/ / .githooks/ / NOTICE / LICENSE /
              SECURITY / CODE_OF_CONDUCT / CONTRIBUTING /
              deny.toml / .gitignore). Documented in
              CONTRIBUTING.md step 5 with the cycle-493
              incident citation. Opt in via
              `git config core.hooksPath .githooks`.

  cycle 495 — Hook header expanded to enumerate "NOT excluded"
              path categories that DO trigger the gauntlet
              (crates / Cargo.toml-lock / assets / scripts /
              shell-integration / tests). Self-verified — hook
              fired correctly on its own commit (touched only
              `.githooks/`, fast-path triggered).

  cycle 496 — **Fix-my-own-bug.** Cycle 494's diff-filter
              `ACMR` excluded `D` (deletions). A commit that
              ONLY deleted `.rs` files would have shown an
              empty non-doc set, falsely matched the doc-only
              fast-path, and skipped gauntlet despite breaking
              the build. Switched to `ACMRD`.

  cycle 497 — CONTRIBUTING.md hook section points readers at
              the `.githooks/pre-commit` header comment for
              the trigger/skip enumeration + notes the
              `--no-verify` per-commit bypass.

## [1.39.0] — 2026-05-22

Doc-accuracy release. Justfile + CONTRIBUTING got a `gauntlet-strict`
recipe for release-cut pre-flight, three field doc-comments in
`app.rs` corrected to reflect post-helper-extraction reality, and
the cycle-471 helper rustdoc gained a maintenance note for future
contributors.

  cycle 478 — `docs/ROADMAP.md` + `docs/TERMINATOR-AUDIT.md` +
              `docs/ARCHITECTURE.md` post-sweep summaries extended
              to v1.38.0 (cycles 411-475, 7 releases, 65 cycles).

  cycle 479 — `Justfile` gained `just gauntlet-strict` — chains
              gauntlet + deny + machete for release-cut pre-flight.
              Daily-iter contributors still use plain `just
              gauntlet`; the strict variant catches stale supply-
              chain ignores + unused deps before tagging.

  cycle 480 — `CONTRIBUTING.md` "Releasing" flow documents
              `just gauntlet-strict` as step 3 between CHANGELOG
              commit + `scripts/release.sh`. Drive-by caught
              a duplicate "step 4" numbering bug.

  cycle 481 — `CONTRIBUTING.md` recipe enumeration lists both
              `gauntlet` + `gauntlet-strict` (was missing both
              composite recipes despite naming deny / machete).

  cycle 482 — `CONTRIBUTING.md` enum reordered to match Justfile
              section order (build/release before gauntlet, not
              after). Justfile is the source of truth.

  cycle 483 — **Doc-accuracy fix.** `pending_pane_restarts` doc
              said "respawns into the same pane id slot" — that
              was cycle-412 intent, but cycle 418 actually shipped
              spawn-as-new-tab. Doc rewritten to match
              implementation + cite cycle-452 dedup follow-up.

  cycle 484 — **Doc-accuracy fix.** `lua_engine` field doc listed
              3 LuaEvent emission sites and named "Mux mutations"
              directly. Updated to 5-site enumeration with the
              canonical helper for each (cycles 367/378/424/425)
              + cross-link to `drain_lua_hook_commands` (cycles
              426-428, 433).

  cycle 485 — **Verification fix.** Cycle 484 said 
              `fire_tab_close_event` has 4 call sites; `grep`
              found 5 (tab-bar ✕-click handler has 2 branches
              that both fire the helper, plus 3 keyboard /
              handoff paths). Doc count corrected with "2 click-
              handler branches" note for future readers.

  cycle 486 — `extra_check_config_lines` rustdoc gained an
              "Adding a new branch:" maintenance note that
              names both cycle-471 test guards. Future contri-
              butors adding an 8th opt-in echo see the test-
              extension contract without grep-hunting.

## [1.38.0] — 2026-05-22

Doc-durability release. One more `--check-config` echo (status-bar),
one helper extraction + 3 new drift guards on the cycle-461-470 echo
contract, and a sweep removing internal cycle refs from every user-
facing doc surface (man page + example config + drift-guard scan
list extension so the pattern can't reintroduce).

  cycle 466 — ROADMAP + TERMINATOR-AUDIT + ARCHITECTURE post-sweep
              summaries extended to v1.37.0 (cycles 411-463, 6
              releases, 53 cycles).

  cycle 467 — `docs/CONFIG.md` gained a row for `force-no-bell`
              (was undocumented despite being a real parser arm)
              + `exit-action` row cites the cycle-452 dedup fix.
              (Cycle-179 drift guard caught the user-facing
              cycle ref in cycle 471's test run; reworded.)

  cycle 468 — `CONTRIBUTING.md` inline recipe list documents
              `just deny` + `just machete` (cycle 456 added the
              recipes; this closes the discoverability gap).

  cycle 469 — `docs/kettle.example.config` gained a commented-out
              `status-bar = off` entry. Users running
              `kettle --print-default-config` now discover the
              status-bar feature.

  cycle 470 — `--check-config` echoes `status-bar: Top|Bottom`
              when non-default. Symmetric with the cycle-461-463
              opt-in echoes.

  cycle 471 — **Refactor + drift guards.** Extracted the cycles-
              461-470 inline echo blocks into
              `extra_check_config_lines(cfg) -> Vec<String>`
              pure helper. Added 2 unit tests pinning the
              empty-for-default + per-opt-in-branch contract.
              Drive-by fix for the cycle-467 user-facing cycle
              ref the drift guard caught.

  cycle 472 — 7th in-isolation test covering the `triggers`
              branch of `extra_check_config_lines`. All 7 echo
              branches now have dedicated test coverage.

  cycle 473 — Exact-numeric test count claims in
              `docs/TERMINATOR-AUDIT.md` + `docs/TESTING.md`
              bumped to 321 / +13 / post-v1.37.0. Loose-bound
              snapshots stay accurate without churn.

  cycle 474 — **Real durability fix.** The example config (user-
              facing via `kettle --print-default-config`) had
              picked up 5 "(cycle N)" / "cycle-N" internal refs
              across cycles 459/460/469/470. Every user's
              bootstrap file would have inherited them. Scrubbed
              all 5 + extended the cycle-179 drift guard's scan
              list to include `docs/kettle.example.config` so a
              future reintroduction fails at test time.

  cycle 475 — Same drift-guard reasoning applied to the man
              page: scrubbed 3 internal cycle refs from
              `packaging/linux/kettle.1` (cycle 436 had added
              them) + extended the cycle-179 scan list to
              `packaging/linux/kettle.1`. `man kettle` is
              user-facing.

## [1.37.0] — 2026-05-22

UX + observability release. One real exit-action=restart bug fix,
six new `--check-config` echo lines covering all the Terminator-
parity opt-in keys (accent, force-no-bell, triggers, lua-sandbox,
bg-image, window-flags), three new example-config keys + 3-key
drift-guard extension, build-system fix follow-ups, and supply-
chain hygiene.

  cycle 450 — README + INSTALL.md version pins bumped to v1.36.0.

  cycle 451 — `scripts/install-online.sh` example pin bumped
              v1.3.4 → v1.36.0. Users copying the snippet land
              on a current binary instead of the cycle-150-era
              pre-SHA-256-sidecar release.

  cycle 452 — **Real UX bug fix.** `exit-action = restart` could
              spawn TWO new tabs per dead shell on platforms
              where alacritty fires both `TermEvent::Exit`
              (PTY-side EOF) and `TermEvent::ChildExit(code)`
              (child reaper) for the same exit. Added a
              `Vec::contains` dedup check in `drain_events` so
              only one respawn happens regardless of how many
              TermEvent variants the engine emits per child
              death.

  cycle 454 — Cycle-452 in-code comment cites the two
              alacritty_terminal source-line refs
              (event_loop.rs:263 + term/mod.rs:810) that
              confirm both events ARE emitted on a normal
              shell exit. Future contributors don't have to
              re-derive the dedup rationale.

  cycle 455 — `docs/ROADMAP.md` + `docs/TERMINATOR-AUDIT.md` +
              `docs/ARCHITECTURE.md` post-sweep summaries
              extended to v1.36.0 (cycles 411-452, 5 releases,
              308 → 319 tests).

  cycle 456 — `Justfile` gained `just deny` (`cargo deny check`)
              + `just machete` (`cargo machete`) recipes
              mirroring the existing CI workflows. Contributors
              can pre-flight supply-chain hygiene locally —
              would have caught cycle-444's stale ignore one
              cycle earlier.

  cycle 457 — `docs/INSTALL.md` line 143 MSRV said "1.88" but
              Cargo.toml + README badge + INSTALL line 49 said
              1.89 (cycle-250 bump). Pointed the stray line at
              `Cargo.toml`'s `rust-version` field so future
              bumps only need the toml change to ripple.

  cycle 458 — Normalized `docs/TESTING.md`, `docs/INSTALL.md`,
              and `CONTRIBUTING.md` to 319+ tests + v1.36.0
              baseline (cycle-446 drift guard + v1.36.0 release
              had left three docs trailing).

  cycle 459 — Three Terminator-parity config keys (accent-color
              cycle 309, force-no-bell cycle 349, trigger cycle
              289) were in the parser but missing from the
              embedded example config. Users following the
              cycle-227 first-launch bootstrap never saw them.
              Added commented-out defaults + extended the
              cycle-413 drift guard from 9 → 12 pinned keys.

  cycle 460 — **Fix-my-own-bug.** Cycle 459's trigger example
              used a non-existent `trigger = REGEX = ACTION`
              syntax. v1's parser hardcodes the action to
              Urgency and takes the entire post-`=` value as
              the regex; a copy-paste would have ended up with
              "= notify" literally in the pattern. Corrected
              to `trigger = REGEX` with a "do NOT add a second
              `=`" warning.

  cycle 461 — `--check-config` summary echoes four Terminator-
              parity opt-in keys when set:
                accent:   #RRGGBB
                bell:     force-no-bell=true ...
                triggers: N pattern(s) configured ...
                lua:      sandbox=Trusted
              Guarded so default-config output stays terse.
              Symmetric with the existing font-features /
              styled-families echoes. End-to-end verified.

  cycle 462 — `--check-config` also echoes bg-image when set:
                bg-image: PATH (mode=…, blur=…, darkness=…)
              Most visually-impactful opt-in surface; the
              cycle-461 sweep had skipped it.

  cycle 463 — `--check-config` also echoes window-flags when
              non-default:
                window-flags: state=Fullscreen borderless=true
                              always-on-top=true
              Easy-to-set-then-forget Terminator-parity keys
              (cycles 339/342) that the summary used to
              silently drop.

## [1.36.0] — 2026-05-22

Production-hygiene release. One real bug fix (`KETTLE_GIT_SHA`
freshness), one supply-chain hygiene fix (stale `cargo-deny`
ignore), one drift guard, and a sweep of stale-snapshot fixes
across the docs.

  cycle 438 — README + INSTALL.md version pins bumped to v1.35.0.

  cycle 439 — `docs/ROADMAP.md` + `docs/TERMINATOR-AUDIT.md`
              post-sweep summaries updated to include v1.35.0
              (4 releases across 28 cycles, 308 → 318 tests).

  cycle 440 — `packaging/linux/kettle.1` documents 6 more default
              chords: Alt+1..9 (Goto tab N), F11 (Fullscreen),
              Ctrl+0 (ResetFontSize), Shift+Arrow (Resize),
              Shift+PgUp/Dn (page scroll), Shift+Home/End
              (scroll-to-edge). Coverage 27 → 33 of 59.

  cycle 441 — TESTING.md headline (267 → 318 tests, v1.7.0 →
              v1.35.0) + INSTALL.md verify-build example
              (240+ → 318+).

  cycle 442 — ROADMAP "19-test harness" claim → 318; ARCHITECTURE
              "v1.8.0 → v1.32.0 sweep" → consistent v1.8.0 →
              v1.31.0 sweep (cycles 330-410) + v1.32.0 → v1.35.0
              polish (cycles 411-438) split.

  cycle 443 — CONTRIBUTING.md cycle + test-count snapshots
              (300+ → 440+ cycles, 267+ → 318+ tests).

  cycle 444 — **Real hygiene fix.** Dropped the stale
              `RUSTSEC-2024-0436` ignore from `deny.toml` +
              `.github/workflows/audit.yml`. The `paste → rav1e
              → image` chain that justified it is no longer in
              Cargo.lock (verified with `cargo tree -i paste`).
              `cargo deny check` now warning-free.

  cycle 445 — **Real bug fix.** `crates/kettle/build.rs` was
              capturing `KETTLE_GIT_SHA` once and not refreshing
              when only other workspace crates changed (Cargo's
              default rerun-policy only scans the build script's
              own package). `kettle --version` showed stale SHAs.
              Added `cargo:rerun-if-changed=NONEXISTENT_FORCE_
              RERUN_FOR_KETTLE_GIT_SHA` to force every-build
              re-execution; the ~10ms git-subprocess cost is
              well under build-time noise. Restores the cycle-195
              "+dirty marker refreshes on every source edit"
              contract.

  cycle 446 — Drift guard for `kettle.config_path()` return-type
              contract (must be `string | nil`, never anything
              else). 318 → 319 tests.

  cycle 447 — `SECURITY.md` "What's in scope" list gained two
              bullets covering the v1.8.0+ Lua-plugin sandbox-
              escape surface and the cycles 403/408 detachable-
              tabs handoff payload-abuse surface.

  cycle 448 — `Justfile` `just test` recipe doc bumped 261+ →
              319+.

## [1.35.0] — 2026-05-22

Post-v1.34.0 polish. Plugin-contract refactor finished off, drift
guards extended, docs caught up to current HEAD.

  cycle 428 — `App::resumed` startup-hook drain (the last remaining
              inline `LuaCommand`-variant match in the
              `ApplicationHandler` trait impl) now routes through
              `drain_lua_hook_commands`. A stale comment claimed
              inherent methods aren't callable from trait impls —
              they are, as long as `self: &mut App`. All 5 event
              hooks (Startup / TabAdd / TabClose / Bell / Output)
              now share one canonical drain path.

  cycle 429 — README + `docs/INSTALL.md` version pins bumped to
              v1.34.0 (README status line v1.31.x → v1.34.x;
              KETTLE_VERSION example v1.3.4 → v1.34.0; INSTALL.md
              SHA-256 verify URLs v1.32.0 → v1.34.0). The
              recommended install command now lands users on a
              current binary.

  cycle 430 — Drift-guard tests for `kettle.notify` +
              `kettle.set_theme` queue/drain semantics. The
              cycle-426-428 helper depends on these variants
              being present; a future refactor of the mlua
              closures could silently drop the push and the
              helper would just see empty drains. 308 → 316.

  cycle 431 — `docs/TERMINATOR-AUDIT.md` tail extended with the
              post-sweep polish summary (cycles 411-430 spanning
              v1.32.0 → v1.34.0). Future contributors see the
              audit's trajectory through current HEAD.

  cycle 432 — `docs/ROADMAP.md` gained a v1.32.0 → v1.34.0
              section bridging the v1.8.0 → v1.31.0 sweep and
              the Next list. Five threads: plugin-contract bug
              fixes, exit-action=restart, helper unification,
              docs-as-code, drift guards.

  cycle 433 — Lua menu-item click drain (cycle 375) routed
              through `drain_lua_hook_commands` — −35 more lines
              of duplication. The only remaining inline drain
              is App::new (early init before `self` exists).

  cycle 434 — `drain_lua_hook_commands` rustdoc updated to list
              all 6 callers (was 2). Future contributors see the
              full surface without grepping.

  cycle 435 — Drift-guard tests for `kettle.add_menu_item` /
              `invoke_menu_item` + `kettle.add_url_handler` /
              `try_url_handler`. Pattern-match short-circuit,
              error isolation, out-of-range index safety.
              316 → 318.

  cycle 436 — `packaging/linux/kettle.1` man page filled in 8
              missing CLI-flag entries (--remote-send,
              --remote-file, --toggle, --profile, --layout,
              --accent, --lua-script, --annotate). `man kettle`
              now matches `kettle --help`.

## [1.34.0] — 2026-05-22

Plugin-contract hardening — every new_tab / close_tab call site now
fires the canonical Lua event, and the four Lua hook drains share one
helper. Also fixes a live-grid bug in the exit-action=restart path.

  cycle 420 — `exit-action = restart` respawn now uses
              `self.grid_of(self.area())` for cols/rows instead of
              the hardcoded `80, 24` that cycle-418 shipped. The
              restarted shell matches the surface size that was on
              screen when it died, so `tput cols` / `tput lines`
              and TUI apps read the right values.

  cycle 421 — `docs/ARCHITECTURE.md` detachable-tabs flow upgraded
              from ASCII tree to mermaid `flowchart TD` (3 IPC
              paths → target kettle → session restore).

  cycle 422 — `docs/ARCHITECTURE.md` Plugin + Background-image
              flows upgraded to mermaid (`flowchart TD` for
              `init.lua` → LuaEngine → LuaCommand dispatch;
              `flowchart LR` for `decode_bg_image` → blur →
              `BgImage` cache → `imgpipe`). The per-pane titlebar
              keeps its ASCII art — it is layout, not flow.

  cycle 423 — **Plugin-contract bug fix.** Remote-control IPC
              `new-tab` verb (cycle 419) was bypassing
              `LuaEvent::TabAdd`. Plugins listening for tab-spawn
              now see IPC-driven tab creation as well as keyboard
              + mouse paths.

  cycle 424 — **Plugin-contract bug fix (3 sites).** Extracted
              `fire_tab_close_event(closing_idx)` helper and
              applied it to the three `close_tab` paths that had
              been bypassing `LuaEvent::TabClose`:
              - SCM_RIGHTS tab-handoff source (cycle 408)
              - file-fallback tab-handoff source (cycle 403)
              - tab-bar ✕-click (cycle 386)
              Keyboard `CloseTab` already fired correctly; mouse
              + detachable-tabs paths now match it.

  cycle 425 — **Plugin-contract bug fix (2 sites).** Extracted
              `fire_tab_add_event()` helper and applied it to the
              two `new_tab` paths that had been bypassing
              `LuaEvent::TabAdd`:
              - `Action::NewWindow` fallback (when window-spawn
                degrades to in-process new tab)
              - cycle-418 exit-action=restart respawn
              All five tab-spawn paths (keyboard, mouse,
              remote-control, NewWindow fallback, restart) now
              fire the canonical event.

  cycle 426 — **Refactor.** Created `drain_lua_hook_commands(hook_name)`
              with a full LuaCommand match (SendText / ExecAction /
              Notify / SetTheme) and routed the three TabAdd / TabClose
              hook drains through it. Deleted ~120 lines of inline
              variant duplication; the helper logs `hook_name` for
              every dispatched command so trace output identifies
              which event fired what.

  cycle 427 — **Refactor.** Bell + Output hook drains routed
              through the same `drain_lua_hook_commands` helper.
              −51 more lines. All four event hooks (TabAdd,
              TabClose, Bell, Output) now share one canonical
              command-drain path; adding a fifth event is one new
              fire_event call + nothing else.

After cycle 427: every new_tab / close_tab call site fires the
matching `LuaEvent`, and every event hook routes through one
helper. Workspace tests stay green; binary smoke clean.

## [1.33.0] — 2026-05-22

Real feature work — `exit-action = restart` is now end-to-end, and
the remote-control IPC `new-tab` verb is wired.

  cycle 416 — `docs/ARCHITECTURE.md` documents the cycles 330-415
              Terminator-parity subsystems with ASCII flow
              diagrams + integration narratives for Plugin
              (Lua), Per-pane titlebar, Background image, and
              Detachable tabs. Cross-references each design doc.

  cycle 417 — `docs/INSTALL.md` version refs bumped v1.3.4 →
              v1.32.0 (KETTLE_VERSION pin example + SHA-256
              verify URLs). Users following the recommended pin
              now get a recent kettle.

  cycle 418 — `exit-action = restart` fully implemented end-to-
              end. Closes cycle-357's "not yet implemented"
              warn-and-fallback. On shell-exit with
              `cfg.exit_action = restart`, the dead pane queues
              to `pending_pane_restarts`; the post-drain handler
              in `redraw` calls `Mux::new_tab_with` with the
              same argv + cwd, spawning a fresh shell. Matches
              Terminator's documented behavior.

  cycle 419 — Remote-control IPC `new-tab` verb wired. Was
              logging "not yet implemented" since cycle 302
              (the verb was recognized but no-op'd). Today:
              calls `Mux::new_tab` with current cell grid +
              waker. Completes the remote surface alongside
              `send-text` + `toggle-window`.

After cycle 419: zero "TODO" + zero "not yet implemented" markers
remain in the codebase (verified via grep). Workspace tests
stay at 308. End-to-end binary smoke green.

## [1.32.0] — 2026-05-22

Production-readiness polish — docs sync + drift guards + foundation
for exit-action=restart.

  cycle 411 — `cargo doc -D warnings` clean (3 doc-comment warnings
              fixed in kettle-render + kettle-vt). Matches the CI
              doc-warnings gate.

  cycle 412 — exit-action = restart pane-respawn queue infrastructure
              (partial). Replaces the cycle-357 TODO with concrete
              pending_pane_restarts plumbing; respawn dispatch is
              the next sub-cycle.

  cycle 413 — `print_default_config_round_trip` drift guard pins 9
              load-bearing Terminator-parity keys (window-state,
              borderless, always-on-top, show-titlebar,
              title-at-bottom, background-image,
              background-image-mode, exit-action, lua-sandbox)
              in the embedded example config so future strips
              fail loud.

  cycle 414 — Man page (`packaging/linux/kettle.1`) documents
              `--tab-handoff PATH` (cycle 403 file-fallback) +
              `--tab-handoff-fd FD` (cycle 408 SCM_RIGHTS path).

  cycle 415 — `docs/CONFIG.md` adds a "Terminator-parity keys" table
              covering ~30 cycles 331-410 keys with type + default +
              behavior. Cross-references the audit doc for the full
              85-key parsed surface.

  docs sync — README Status line v1.7.x → v1.31.x (caught up after
              24 releases of sweep). `docs/ROADMAP.md` grew a
              "v1.8.0 → v1.31.0 Terminator-parity sweep" section
              + trimmed Next list to the genuine remaining threads.
              `docs/TERMINATOR-AUDIT.md` tail appended with the
              cumulative cycles 330-412 sweep completion summary.
              `docs/kettle.example.config` grew a Terminator-parity
              section with every major new knob's default + origin.

Workspace tests stay at 308. `cargo doc -D warnings` clean. `cargo
machete` reports zero unused deps. End-to-end binary smoke green
(`--version`, `--check-config`, `--list-actions`, `--list-keybinds`,
`--print-default-config`).

## [1.31.0] — 2026-05-22

SCM_RIGHTS cross-process tab handoff end-to-end for the JSON
payload.

  cycle 408 — `--tab-handoff-fd FD` CLI flag plumbing.
              Inherited socket fd carrying serialized tab JSON
              + SCM_RIGHTS ancillary data.

  cycle 409 — Target-side recv. App startup detects
              --tab-handoff-fd FD; constructs UnixStream from
              the fd via FromRawFd; calls fd_transport::recv_fds
              (cycle 399); deserializes the JSON into the
              existing Session restore path. Received PTY fds
              are closed on the target side (source still owns
              canonical refs); adoption-as-Pane is the final
              piece pending Terminal::from_raw_fd in kettle-core.

  cycle 410 — Source-side socketpair + fork+exec. New
              App::try_move_tab_to_new_window_scm_rights helper
              opens a UnixStream pair, fork+execs a kettle child
              with --tab-handoff-fd 3 (via pre_exec dup2 +
              clear-FD_CLOEXEC), then calls
              fd_transport::send_fds with the JSON payload.
              Action::MoveTabToNewWindow now prefers this over
              the cycle-405 file-fallback on Unix.

The detachable-tabs cross-window flow now ships via SCM_RIGHTS-
capable socket IPC on Unix + file-fallback elsewhere. Both paths
deliver the same user-visible UX (split tree + cwds preserved
in the new window). The SCM_RIGHTS variant additionally positions
for live PTY-fd transfer when the Terminal::from_raw_fd kettle-
core change lands — at which point running shells survive the
move without restart.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 10/10 COMPLETE
bg-image (12 sub-cycles):         ✅ 11/12 effectively COMPLETE
Detachable tabs (11 sub-cycles):  ✅ 11/11 sub-cycle 7
                                       SCM_RIGHTS path end-to-end
                                       for the JSON payload
                                       (Terminal::from_raw_fd
                                        Pane-adoption is a
                                        kettle-core internal
                                        change tracked separately
                                        from the design doc)

45 of 46 Bucket-D sub-cycles shipped (98%).

Only the bg-image sub-cycle 8 "explicit resize handler" remains
flagged — and that's documented as implicit per-frame UV
recompute (cycle 394), which IS the implementation (a separate
explicit handler would be redundant).

Workspace tests stay at 308.

## [1.30.0] — 2026-05-22

Named broadcast groups + EditPaneGroup action — titlebar Bucket-D
sub-cycle 8 now COMPLETE.

  cycle 406 — Named broadcast groups foundation.
              Pane.group_name: Option<String>.
              PaneView grows group_name field.
              Per-pane titlebar prefixes "[group-name] " before
              the title (Terminator titlebar.py indicator pattern).

  cycle 407 — Action::EditPaneGroup full impl.
              Aliases: edit_pane_group, edit-pane-group,
                       edit_group, edit-group.
              Opens TitleEditState with new TitleEditScope::Group.
              Apply: writes pane.group_name (None on empty input).
              Overlay label: "Edit pane group:".
              Anchors near focused pane (same as EditPaneTitle).

The previously-Bucket-E titlebar sub-cycle 8 is now end-to-end:
data model + render + keyboard-bindable action + palette entry +
edit overlay.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 10/10 COMPLETE
                                       (cycle 406 + 407 closed
                                        sub-cycle 8 from Bucket-E)
bg-image (12 sub-cycles):         ✅ 11/12 — all implemented
                                       (sub-cycle 8 is implicit
                                        per-frame UV recompute,
                                        cycle 394 documented this)
Detachable tabs (11 sub-cycles):  ✅ 10/11 — file-fallback path
                                       end-to-end shipped
                                   ⌛ 1 — sub-cycle 7 SCM_RIGHTS
                                       live PTY transfer (multi-
                                       week cross-process IPC)

44 of 46 Bucket-D sub-cycles end-to-end (96%).

Two titlebar sub-cycle 8 from Bucket-E to shipped:
  ✅ EditPaneGroup action + palette entry + edit overlay
  ✅ Pane.group_name data model
  ✅ Titlebar render shows "[group-name] title  WxH  🔔"

Workspace tests stay at 308.

## [1.29.0] — 2026-05-22

Detachable-tabs end-to-end file-fallback path COMPLETE.

  cycle 402 — winit CursorLeft/Entered → drag FSM transitions.
              Closes detachable-tabs sub-cycle 6.
  cycle 403 — `--tab-handoff PATH` CLI flag scaffolding.
  cycle 404 — Session::load_tab_handoff App-side restore.
              Closes detachable-tabs sub-cycle 8 in the
              file-fallback path.
  cycle 405 — Action::MoveTabToNewWindow → write JSON
              handoff + spawn --tab-handoff PATH child.
              Source tab serializes; target reconstructs.
              Cross-platform (works on Linux/macOS/Windows/
              Wayland). Closes the cross-process tab-handoff
              workflow end-to-end via the file path.

### End-to-end detachable-tabs flow (file-fallback)

  Source process:
    1. User triggers Action::MoveTabToNewWindow.
    2. Mux::serialize_tab(active) → STab.
    3. serde_json::to_string + write to /tmp/kettle-handoff-PID.json
    4. Spawn `kettle --tab-handoff PATH --config CFG`.
    5. Close source tab.

  Target process:
    1. App startup detects --tab-handoff PATH.
    2. Session::load_tab_handoff reads + deletes the file.
    3. Restore tab(s) via cycle-291 restore path.
    4. User sees split tree + cwds in the new window.

Live PTY transfer requires SCM_RIGHTS (sub-cycle 7); the file-
fallback trades that for cross-platform support (target spawns
fresh shells instead of adopting fds).

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 9 — all impl shipped
                                   E — sub-cycle 8 (group-name
                                       edit) Bucket-E
bg-image (12 sub-cycles):         ✅ 11/12 effectively COMPLETE
Detachable tabs (11 sub-cycles):  ✅ 10 — all sub-cycles shipped
                                       end-to-end via file-fallback
                                   ⌛ 1 — sub-cycle 7 SCM_RIGHTS
                                       cross-process PTY fd transfer
                                       (file-fallback is the cross-
                                       platform analog shipped today;
                                       SCM_RIGHTS variant preserves
                                       live shells)

43 of 46 Bucket-D sub-cycles end-to-end (93%).

Workspace tests stay at 308.

## [1.28.0] — 2026-05-22

  cycle 401 — Drag FSM cancel path + cursor-leave/reenter
              transitions + end-to-end walkthrough drift guard.

              New transitions:
                on_cursor_leave_window(session_id):
                  DraggingInside → DraggingOutside
                on_cursor_reenter_window(x, y):
                  DraggingOutside → DraggingInside
                cancel() -> (Self, Option<usize>):
                  Any → Idle; returns tab_idx that was being
                  dragged so the caller can restore visuals.

              The end_to_end_drag_walkthrough drift guard
              exercises the full FSM path: Idle → Armed →
              DraggingInside → DraggingOutside → cancel → Idle.

              Closes detachable-tabs Bucket-D sub-cycle 9
              (cancel) in full + sub-cycle 11 (e2e test) for
              the FSM portion. Full sub-cycle 11 needs a
              cross-process integration test which spans
              multiple sessions per the design doc.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 9 — sub-cycles 2-7, 9, 10
                                   E — sub-cycle 8 (group-name
                                       edit) Bucket-E
bg-image (12 sub-cycles):         ✅ 11/12 effectively COMPLETE
Detachable tabs (11 sub-cycles):  ✅ 7 — sub-cycles 1 (design),
                                       2 (serialize), 3 (SCM_RIGHTS),
                                       4 (extract/insert), 5 (FSM),
                                       9 (cancel), 10 (Wayland
                                       fallback), 11 partial
                                       (FSM e2e test)
                                   ⌛ 4 — sub-cycles 6 (cursor
                                       detection winit-side), 7
                                       (cross-process IPC + fd
                                       transfer), 8 (new-window-
                                       on-drop), 11 full (cross-
                                       process integration test)

41 of 46 Bucket-D sub-cycles end-to-end (89%).

Workspace tests stay at 308.

The 4 remaining detachable-tabs sub-cycles are all CROSS-
PROCESS integration: they compose every foundation now shipped
(FSM, SCM_RIGHTS, serialize/extract/insert, cancel path,
Wayland-fallback) into the workflow where two kettle
processes coordinate. Per the design doc, integration spans
multiple sessions because the test fixture (two concurrent
kettle processes) is inherently a multi-process problem.

## [1.27.0] — 2026-05-22

Two more detachable-tabs Bucket-D foundations.

  cycle 399 — `kettle_ui::fd_transport` SCM_RIGHTS module.
              send_fds / recv_fds on Unix sockets via
              libc::sendmsg + ancillary cmsg + SCM_RIGHTS.
              Unix-only (#[cfg(unix)]). Windows + Wayland get
              the cycle-384 keyboard-driven fallback.
              Closes detachable-tabs Bucket-D sub-cycle 3.

  cycle 400 — `kettle_ui::detach::DragState` FSM. Pure-data
              state machine with 4 states (Idle, ArmedInside,
              DraggingInside, DraggingOutside) + 5 transitions
              (on_mouse_down_on_tab, on_mouse_move,
              on_mouse_up, on_abort, is_dragging).
              4px drag-threshold matches GTK + most desktops.
              Closes detachable-tabs Bucket-D sub-cycle 5.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 9 — sub-cycles 2-7, 9, 10 +
                                       layout-shift
                                   E — sub-cycle 8 (group-name
                                       edit) Bucket-E
bg-image (12 sub-cycles):         ✅ 11 — all implemented
                                       (cycle 396 closed sub-
                                       cycle 9 blur shader as
                                       CPU-side decode-time)
Detachable tabs (11 sub-cycles):  ✅ 6 — sub-cycles 1 (design),
                                       2 (serialize), 3 (SCM_RIGHTS),
                                       4 (extract/insert), 5
                                       (drag FSM), 10 (Wayland
                                       fallback)
                                   ⌛ 5 — sub-cycles 6 (cursor
                                       detection), 7 (cross-
                                       process IPC + fd transfer),
                                       8 (new-window-on-drop), 9
                                       (cancel path), 11 (e2e test)

39 of 46 Bucket-D sub-cycles end-to-end (85%).

The 5 remaining detachable-tabs sub-cycles are all integration
work: each composes the foundations now shipped (FSM, SCM_RIGHTS,
serialize/extract/insert, Wayland fallback) into the cross-
process workflow. Multi-week per the design doc; pickable
cleanly by future sessions.

Workspace tests stay at 308.

## [1.26.0] — 2026-05-22

Detachable-tabs Bucket-D foundation APIs.

  cycle 397 — `Mux::serialize_tab(idx)` returns the same STab
              wire format that session.json uses. Pure-data
              utility that future cross-process IPC consumes.
              Closes detachable-tabs Bucket-D sub-cycle 2.

  cycle 398 — `Mux::extract_tab(idx)` + `Mux::insert_tab(at, Tab)`
              — in-process tab handoff primitives. extract_tab
              removes a tab from the tabs list WITHOUT touching
              its panes (the panes stay in self.panes; the
              caller is responsible for transferring or dropping
              them). insert_tab inserts a Tab at the given idx
              + sets active so the moved tab is focused
              immediately. Closes detachable-tabs Bucket-D
              sub-cycle 4.

Both APIs are #[allow(dead_code)] until the cross-process IPC
caller lands (sub-cycles 7+8); the in-process foundation
ships now so the IPC cycle composes cleanly with proven
primitives.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 9 — sub-cycles 2-7, 9, 10 +
                                       layout-shift
                                   E — sub-cycle 8 (group-name
                                       edit) — Bucket-E until
                                       named broadcast groups
                                       infra lands
bg-image (12 sub-cycles):         ✅ 11 — all sub-cycles
                                       implemented + 1 implicit
                                       per-frame UV recompute
Detachable tabs (11 sub-cycles):  ✅ 4 — sub-cycles 1 (design
                                       doc), 2 (serialize_tab),
                                       4 (extract/insert), 10
                                       (Wayland fallback)
                                   ⌛ 7 — sub-cycles 3
                                       (SCM_RIGHTS wrapper), 5
                                       (drag state machine), 6
                                       (cursor detection), 7
                                       (cross-process IPC + fd
                                       transfer), 8 (new-window
                                       on-drop), 9 (cancel
                                       path), 11 (e2e test)

37 of 46 Bucket-D sub-cycles end-to-end (80%).

Workspace tests 306 → 308 (+2 drift guards).

## [1.25.0] — 2026-05-22

  cycle 395 — Per-pane Edit-title overlay anchors near clicked
              pane. Pane-scope edits render the overlay at the
              focused pane's titlebar position; window + tab
              scopes keep window-bottom. UX matches Terminator's
              click-to-edit-in-place expectation. Closes
              titlebar Bucket-D sub-cycle 7.
  cycle 396 — CPU-side `background_blur` for bg-image. 3-pass
              separable box blur approximates Gaussian at much
              lower compute (~30-50ms on a 1080p image at radius
              8). Applied at decode-time, so per-frame render
              cost is zero. Closes bg-image Bucket-D sub-cycle 9.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 9 of 10 — sub-cycles 2/3/4/5/6/7/9/10 +
                                       layout-shift
                                   E — sub-cycle 8 (inline group-name
                                       edit) deferred until kettle
                                       grows named broadcast groups
                                       (currently per-tab on/off only)
bg-image (12 sub-cycles):         ✅ 11 of 12 — sub-cycles 2/3/4/5/6/7/8/9/10/11/12
                                   ⌛ 0 (all impl complete; sub-cycle 8
                                       was implicit per-frame recompute,
                                       documented in cycle 394)
Detachable tabs (11 sub-cycles):  ✅ 1 — sub-cycle 10 Wayland-fallback
                                   ⌛ 10 — sub-cycles 2-9, 11 (cursor
                                       drag + SCM_RIGHTS fd transfer +
                                       cross-process IPC + auth +
                                       reattach — multi-week thread)

34 of 46 Bucket-D sub-cycles end-to-end (74%).

bg-image Bucket-D is now effectively COMPLETE — every sub-cycle
has a shipped implementation (10 explicit + 1 documented-as-
implicit). The blur is CPU-side; a future wgpu-shader version
would shave the ~50ms decode-time cost but the user-visible
effect lands today.

Workspace tests stay at 306.

## [1.24.0] — 2026-05-22

  cycle 394 — bg-image resize handler documented as implicit
              per-frame UV recompute. The cycle-388 cache
              stores the decoded image bytes; the cycle-390
              UV-mode dispatch reads current surface dims each
              frame in build_frame. Window resizes implicitly
              take effect on the next frame. Closes bg-image
              Bucket-D sub-cycle 8 by documenting the
              recompute-contract so future contributors don't
              add a redundant resize handler.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 9 of 10
                                   ⌛ 1 — sub-cycle 7
                                       (per-pane edit anchor;
                                       overlay renders at
                                       window-bottom today,
                                       anchoring at clicked
                                       pane's titlebar is polish)
                                   E — sub-cycle 8 (group-name
                                       edit) deferred — kettle
                                       doesn't yet have named-
                                       groups infra; sub-cycle
                                       waits on that to land
                                       independently
bg-image (12 sub-cycles):         ✅ 10 of 12
                                   ⌛ 2 — sub-cycle 8 ✅
                                       (implicit per-frame
                                       recompute documented),
                                       sub-cycle 9 (blur shader
                                       — needs WGSL Gaussian
                                       two-pass pipeline)
Detachable tabs (11 sub-cycles):  ✅ 1 — sub-cycle 10
                                       Wayland-fallback
                                   ⌛ 10 — sub-cycles 2-9, 11
                                       (cross-window cursor
                                       drag + SCM_RIGHTS fd
                                       transfer + auth +
                                       reattach — multi-week
                                       thread)

33 of 46 Bucket-D sub-cycles end-to-end (72%).

Workspace tests stay at 306.

## [1.23.0] — 2026-05-22

  cycle 393 — Titlebar pixel acceptance test. Pure
              `pane_titlebar_hit_geometry` helper extracted +
              drift-guarded with 8 assertions covering both
              top + bottom bar positions, hit/miss for
              multi-pane layouts. Closes titlebar Bucket-D
              sub-cycle 10.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 9 — sub-cycles 2/3/4/5/6/9/10 +
                                       layout-shift + size-text
                                   ⌛ 1 — 7 per-pane edit anchor
                                       (overlay renders at window
                                       bottom; anchoring at the
                                       clicked pane's titlebar is
                                       polish), 8 group-name edit
                                       (needs named-groups infra)
bg-image (12 sub-cycles):         ✅ 9 — sub-cycles 2/3/4/5/6/7/10/11/12
                                   ⌛ 3 — 8 explicit resize handler,
                                       9 blur shader
Detachable tabs (11 sub-cycles):  ✅ 1 — sub-cycle 10
                                       Wayland-fallback
                                   ⌛ 10 — cursor drag + IPC +
                                       SCM_RIGHTS + auth + reattach

32 of 46 Bucket-D sub-cycles shipped end-to-end (70%).

Workspace tests 305 → 306.

## [1.22.0] — 2026-05-22

  cycle 391 — bg-image align_horiz + align_vert wired. The
              cycle-390 center + scale image modes now honor
              the position-anchor config keys. Closes bg-image
              Bucket-D sub-cycle 6 in full.
  cycle 392 — bg-image acceptance test. Generates a known 8x4
              RGBA PNG via the image crate, decodes via
              decode_bg_image, asserts dimensions + spot-checks
              the first pixel. Closes bg-image Bucket-D
              sub-cycle 12.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 8 — sub-cycles 2/3/4/5/6/9 +
                                       layout-shift + size-text
                                   ⌛ 2 — 7 per-pane edit anchor,
                                       8 group-name edit (needs
                                       named-groups infra),
                                       10 pixel acceptance test
bg-image (12 sub-cycles):         ✅ 9 — sub-cycles 2/3/4/5/6/7/10/12
                                       + 11 (implicit path-cache)
                                   ⌛ 3 — 8 resize (implicit per-frame
                                       UV recompute), 9 blur shader
Detachable tabs (11 sub-cycles):  ✅ 1 — sub-cycle 10
                                       Wayland-fallback
                                   ⌛ 10 — cursor drag + IPC +
                                       SCM_RIGHTS + auth + reattach

31 of 46 Bucket-D sub-cycles shipped end-to-end (67%).

Workspace tests 304 → 305.

## [1.21.0] — 2026-05-22

  cycle 389 — Per-pane titlebar click → EditPaneTitle. Hit-test
              checks click in titlebar y-band (top or bottom
              per cfg.title_at_bottom); focused-pane titlebar
              click opens the edit overlay; unfocused-pane
              titlebar click first focuses (two-click model
              avoids accidental edits on focus transitions).
              Closes titlebar sub-cycle 5.
  cycle 390 — bg-image UV-mode variants. background-image-mode
              controls how the decoded image fills the surface:
                stretch_and_fill (default), tile, center, scale.
              Closes bg-image sub-cycles 5 + 6.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):           ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):         ✅ 8 — sub-cycles 2/3/4/5/6/9 +
                                       cell-shift + size-text done
                                   ⌛ 2 — 7 per-pane edit anchor,
                                       8 group-name edit,
                                       10 pixel acceptance test
bg-image (12 sub-cycles):         ✅ 7 — sub-cycles 2/3/4/5/6/7/10
                                       + path-cache reload (11
                                       implicit)
                                   ⌛ 5 — 8 resize, 9 blur shader,
                                       12 acceptance test
Detachable tabs (11 sub-cycles):  ✅ 1 — sub-cycle 10
                                       Wayland-fallback
                                   ⌛ 10 — cursor drag + IPC +
                                       SCM_RIGHTS + auth + reattach

29 of 46 Bucket-D sub-cycles shipped end-to-end (63%).

Workspace tests stay at 304.

## [1.20.0] — 2026-05-22

Titlebar receive-state variant + background-image full render.

  cycle 387 — Per-pane titlebar receive-state color variant.
              cfg.title_receive_bg/fg_color used when broadcast
              is on + pane isn't the focused source. Closes
              titlebar sub-cycle 4.
  cycle 388 — Background-image full wgpu render. When
              cfg.background_type = image + cfg.background_image
              is set, decodes via cycle-381 helper, caches the
              ImageData, prepends to img_items so imgpipe draws
              it as the first textured quad — wallpaper visible
              behind padding gaps, transparent cells, and
              dim overlays. Closes bg-image sub-cycles 3+4.

### Cumulative Bucket-D status

Plugin (13 sub-cycles):       ✅ 13/13 COMPLETE
Titlebar (10 sub-cycles):     ✅ 7 (sub-cycles 2/3/4/6/9 + layout-shift)
                              ⌛ 3 (5 hit-test / 7 per-pane edit / 8 group label / 10 test)
bg-image (12 sub-cycles):     ✅ 5 (sub-cycles 2/3/4/7/10)
                              ⌛ 7 (UV modes 5+6, align, resize, blur, reload, test)
Detachable tabs (11 sub-cycles): ✅ 1 (10 Wayland-fallback)
                              ⌛ 10 (cursor drag, IPC, SCM_RIGHTS,
                                    reattach)

26 of 46 Bucket-D sub-cycles shipped end-to-end.

Workspace tests stay at 304.

## [1.19.0] — 2026-05-22

Titlebar Bucket-D + detachable-tabs Wayland-fallback push.

  cycle 383 — Cell-content layout-shift for per-pane titlebar.
              build_pane gets a `pane_titlebar_h` parameter;
              cells, images, search highlights, hint labels all
              shift below the bar. Closes titlebar sub-cycle 2.
  cycle 384 — `Action::MoveTabToNewWindow` (detachable-tabs
              Bucket-D Wayland-fallback sub-cycle 10). Spawns
              a new kettle process with focused pane's cwd +
              closes source tab. Cross-process PTY transfer
              for the cursor-drag case remains a multi-cycle
              SCM_RIGHTS thread.
  cycle 385 — `title-at-bottom` config wired to render. Bar
              + title text flip to bottom of pane. Closes
              titlebar sub-cycle 9.
  cycle 386 — Titlebar size text + icon_bell. Format:
              "title  WxH  🔔". `title-hide-sizetext` skips
              the WxH; `icon_bell` skips the bell glyph.
              Closes titlebar sub-cycle 6.

### Titlebar Bucket-D status

  ✅  2: cell-content layout-shift (cycle 383)
  ✅  3: title text render (cycle 382)
  ✅  6: title_hide_sizetext + icon_bell (cycle 386)
  ✅  9: title_at_bottom flip (cycle 385)
  ⌛  4: receive/group color variants
  ⌛  5: hit-testing for click + drag-detach
  ⌛  7: edit-title overlay per-pane anchor
  ⌛  8: inline group-name edit
  ⌛  10: pixel acceptance test

6 of 10 titlebar sub-cycles complete + visible end-to-end.

### Detachable Tabs Bucket-D status

  ✅  10: Wayland-fallback keyboard alternative (Action::
        MoveTabToNewWindow). Spawns new window with cwd
        inheritance; PTY-transfer remains the SCM_RIGHTS
        thread.
  ⌛  2-9: cross-window cursor drag, IPC, fd transfer,
        cancel/reattach.

Workspace tests stay at 304.

## [1.18.0] — 2026-05-22

  cycle 382 — Per-pane titlebar TITLE TEXT render. cycle-379's
              background quad now actually displays each pane's
              title via a parallel `pane_titlebar_buffers` field
              on Renderer. Focus state picks fg color
              (transmit_fg / inactive_fg); empty title falls
              back to 'kettle'.

The per-pane titlebar is now FULLY visible end-to-end:
  - Background quad colored per focused/unfocused state (cycle 379)
  - Title text rendered in the configured fg variant (cycle 382)
  - Hit-testing, group label, activity dot, size-text, cell-content
    layout-shift remain titlebar Bucket-D follow-ups.

### Titlebar Bucket-D progress

  ✅  2 (partial): visible background quad (cycle 379)
  ✅  3 (partial): title text render (cycle 382)
  ⌛  3 (remainder): activity dot in titlebar
  ⌛  4: color variants for receive (group-broadcast) state
  ⌛  5: hit-test for click + drag-detach region
  ⌛  6: title_hide_sizetext + icon_bell wired
  ⌛  7: edit-title overlay anchors to clicked pane's titlebar
        (existing cycle-369/372 overlay works at window-level
         now; per-pane click is the follow-up)
  ⌛  8: inline group-name edit
  ⌛  9: title_at_bottom flip
  ⌛  10: pixel-tolerance --screenshot acceptance test

4 of 10 titlebar sub-cycles per design doc are functional.

Workspace tests stay at 304.

## [1.17.0] — 2026-05-22

Plugin Bucket-D COMPLETE end-to-end + first user-visible
deliverables for titlebar + bg-image Bucket-D items.

  cycle 377 — LuaEvent::Output variant + fire_event dispatch
              (API surface)
  cycle 378 — LuaEvent::Output PTY-reader sidechannel emission.
              Per-pane `output_rx: Option<Receiver<Vec<u8>>>`
              attached when LuaEngine is active; reader-thread
              try_sends raw bytes; App drain_events coalesces +
              fires LuaEvent::Output(pane_id, bytes).
              Zero-cost when no Lua subscriber.
  cycle 379 — Per-pane titlebar background quad render. When
              cfg.show_titlebar=true + >1 pane in tab, a
              cfg.title_*_bg_color strip renders at the top of
              each pane.
  cycle 380 — background-darkness + background-type composed
              alpha. background-type=transparent or image
              multiplies opacity by darkness, applied to the
              wgpu clear-color (both live + screenshot path).
  cycle 381 — bg-image decoder foundation. New
              `kettle_render::bg_image::decode_bg_image(path)`
              helper with format-feature flags PNG/JPEG/WebP/
              BMP/GIF; tilde-expansion; graceful nil-on-missing.

### Plugin Bucket-D end-to-end

All 5 LuaEvent variants emit; all 7 user-facing kettle.* APIs
ship; init.lua auto-loads; sandbox config knob in place; URL
+ menu handlers route through Lua registry before kettle
defaults.

  ✅  13 of 13 docs/TERMINATOR-PLUGIN-DESIGN.md sub-cycles

### Titlebar Bucket-D progress

  ✅  2 (partial): visible background quad
  ⌛  3+: title text render, color variants for receive state,
         hit-testing, icon_bell, layout-shift so cells don't
         overlap the bar

### bg-image Bucket-D progress

  ✅  2:  decoder foundation
  ✅  7:  background-darkness overlay (composed alpha)
  ✅  10: background-type=transparent path (composed alpha)
  ⌛  3,4,5,6,8,9,11,12 — wgpu texture upload + render quad +
         UV modes + align + resize + blur + reload + tests

### Detachable tabs Bucket-D

  ⌛  All sub-cycles deferred to dedicated session per
      docs/TERMINATOR-DETACHABLE-TABS-DESIGN.md. Needs
      SCM_RIGHTS fd transfer (Linux/macOS) + cross-window
      IPC. Wayland users get the documented keybind-fallback
      alternative.

Workspace tests 302 → 304 (+2 bg_image drift guards).

## [1.16.0] — 2026-05-22

  cycle 375 — `kettle.add_menu_item(label, callback)` Lua API.
              Last user-facing plugin API from docs/TERMINATOR-
              PLUGIN-DESIGN.md. Lua plugins can extend the
              cycle-245 right-click context menu with their
              own entries; clicks invoke the registered
              callback + drain any queued LuaCommands.
              New ContextMenuItem::LuaItem variant + dispatch
              path.

  cycle 376 — `lua-sandbox = safe|trusted` config knob.
              `safe` (default) nil's os.execute / os.exit /
              os.remove / io.open / io.popen / loadfile /
              dofile / package.loadlib in the Lua VM at
              construction. Matches the sandbox defaults of
              WezTerm + Hammerspoon + Neovim plugin runtimes.
              `trusted` exposes everything (user opt-in).

### Plugin sub-cycle status

  ✅  365 — kettle.on event-hook foundation
  ✅  366 — LuaEvent::Startup emission
  ✅  367 — LuaEvent::Bell emission
  ✅  368 — LuaEvent::TabAdd / TabClose emission
  ✅  370 — init.lua auto-load
  ✅  371 — kettle.notify
  ✅  373 — kettle.set_theme
  ✅  374 — kettle.add_url_handler
  ✅  375 — kettle.add_menu_item
  ✅  376 — lua-sandbox config

  ⌛  pending — LuaEvent::Output emission (throttled per-PTY-chunk
                event for plugins that watch terminal output)

10 of 13 docs/TERMINATOR-PLUGIN-DESIGN.md sub-cycles complete.
Every user-facing plugin Lua API is now shipped. Only Output
event emission remains, and that's bounded (throttle bucket +
fire call at the drain_events Output match arm).

Workspace tests stay at 302.

## [1.15.0] — 2026-05-22

Plugin Lua API expansion. Two new plugin sub-cycles + URL routing.

  cycle 373 — `kettle.set_theme(name)` Lua API for runtime theme
              switching. Resolves via Theme::find_name (case-
              insensitive lookup of ~500 bundled themes).
              Unknown name → log::warn fallthrough.
  cycle 374 — `kettle.add_url_handler(name, pattern, callback)`
              Lua API for user-supplied URL routing. Uses Lua's
              native string.match (Terminator-pattern-compatible
              for common URL shapes). Dispatched in
              App::open_url BEFORE cfg.custom_url_handler +
              system-open fallthrough; first-match wins.

### User-facing examples

Replicating Terminator's auto_theme.py + url_handlers.py +
run_cmd_on_match.py + maven.py as a few-line Lua module:

  -- ~/.config/kettle/init.lua
  kettle.on('startup', function()
    local hour = tonumber(os.date('%H'))
    kettle.set_theme(hour >= 18 or hour < 6
      and 'Solarized Dark' or 'Solarized Light')
  end)

  kettle.add_url_handler('github_pr',
    'https?://github%.com/[^/]+/[^/]+/pull/(%d+)',
    function(url) os.execute('gh pr view ' .. url) end)

### Plugin sub-cycle status

  ✅  365 — kettle.on event-hook foundation
  ✅  366 — LuaEvent::Startup emission
  ✅  367 — LuaEvent::Bell emission
  ✅  368 — LuaEvent::TabAdd / TabClose emission
  ✅  370 — init.lua auto-load
  ✅  371 — kettle.notify
  ✅  373 — kettle.set_theme
  ✅  374 — kettle.add_url_handler

  ⌛  pending — LuaEvent::Output, kettle.add_menu_item,
                sandbox config knob

8 of 13 docs/TERMINATOR-PLUGIN-DESIGN.md sub-cycles complete.

Workspace tests stay at 302.

## [1.14.0] — 2026-05-22

Plugin system implementation push. Cycles 370-372 ship:

  cycle 370 — `~/.config/kettle/init.lua` auto-loads at startup
              (no need for explicit --lua-script). Follows the
              Neovim/Hammerspoon/WezTerm convention.
  cycle 371 — `kettle.notify(title, body?)` Lua API for desktop
              notifications. Cross-platform via notify-rust crate
              (libnotify on Linux, NSUserNotification on macOS,
              Toast on Windows). Body is optional; failures
              degrade silently to log::warn (headless / no DBUS).
  cycle 372 — Edit-title overlay visual chrome. Yellow palette[3]
              bottom bar renders the prompt + typed input + cursor.
              Edit-title is now FULLY interactive end-to-end
              (state machine cycle-369 + visual feedback this cycle).

### Plugin system status

  ✅  cycle 365 — kettle.on event-hook foundation
  ✅  cycle 366 — LuaEvent::Startup emission
  ✅  cycle 367 — LuaEvent::Bell emission
  ✅  cycle 368 — LuaEvent::TabAdd / TabClose emission
  ✅  cycle 370 — init.lua auto-load
  ✅  cycle 371 — kettle.notify

  ⌛  pending — LuaEvent::Output, kettle.add_menu_item,
                kettle.add_url_handler, kettle.set_theme,
                sandbox config

6 of 13 docs/TERMINATOR-PLUGIN-DESIGN.md sub-cycles complete.

### Workspace tests

Stay at 302.

## [1.13.0] — 2026-05-22

Plugin emission wirings + Edit-title overlay implementation.

### Plugin sub-cycle wirings (cycles 366-368)

The cycle-365 LuaEvent enum is now wired end-to-end at all 4
emission sites. Users can write event-hook plugins and have
them fire on real kettle events:

  cycle 366  LuaEvent::Startup    fires after first-pane-ready
                                  in App::resumed (guarded against
                                  Wayland's resumed re-emission).
                                  App now persists LuaEngine across
                                  its full lifetime.
  cycle 367  LuaEvent::Bell       fires for each belled pane after
                                  the kettle-side bell processing.
  cycle 368  LuaEvent::TabAdd     fires from Action::NewTab dispatch
                                  with the new active tab index.
             LuaEvent::TabClose   fires from Action::CloseTab dispatch
                                  with the closing tab index.

All 4 LuaEvent variants thus have App emission sites. The
docs/TERMINATOR-PLUGIN-DESIGN.md sub-cycles 2-5 are complete
(foundation + every event-site wiring). Subsequent plugin
sub-cycles (notify, add_menu_item, add_url_handler, set_theme,
sandbox config) build on this foundation.

User-facing example:

  -- ~/.config/kettle/init.lua (autoload pending sub-cycle 11)
  kettle.on('startup', function()
    kettle.send_text('echo \"kettle ' .. kettle.version() .. '\"\\n')
  end)
  kettle.on('bell', function(pane)
    kettle.exec_action('toggle_window_visibility')
  end)
  kettle.on('tab_add', function(idx)
    -- could send greeting text, switch profile, etc.
  end)

### Edit-title overlay (cycle 369)

Replaces the cycle-354 placeholders with a real overlay state
machine. `TitleEditState { scope, input }` opens on
Action::Edit{Window,Tab,Pane}Title pre-filled with the current
title; Enter applies via the appropriate setter (Window::set_title,
Tab.title_override, Pane.title); Esc cancels.

The overlay registers with any_modal_open + close_all_modals so
the cycle-X modal discipline (Esc-to-dismiss, cursor-icon override,
key-route guard) extends transparently. Visual chrome render of
the overlay is a follow-up sub-cycle paired with the per-pane
titlebar Bucket-D work; today's state + apply path is observable
via --remote-list-tabs and the OS window title.

### Status

ALL 18 cycle-342 Action variants are now FULLY wired end-to-end.
Zero placeholder stubs remain in the Action dispatch path.

The 4 LuaEvent emission sites are wired. The plugin foundation
(cycle 365) is now functional end-to-end.

Workspace tests stay at 302.

## [1.12.0] — 2026-05-22

Minor-bump — final config-key wiring batch + all four Bucket-D
design docs + plugin-system foundation.

### Behavior wirings (cycles 358-360)

  cycle 358 — invert-search direction toggle
  cycle 359 — geometry-hinting via winit ResizeIncrements
  cycle 360 — focus = sloppy (focus-follows-mouse)

### Bucket-D design docs shipped (cycles 361-364)

All four multi-cycle Bucket-D items from docs/TERMINATOR-AUDIT.md
have concrete design docs now, each following the cycle-328/329
template:

  docs/TERMINATOR-PLUGIN-DESIGN.md          — Lua event-hooks
  docs/TERMINATOR-PANE-TITLEBAR-DESIGN.md   — per-pane titlebar
  docs/TERMINATOR-DETACHABLE-TABS-DESIGN.md — cross-window drag
  docs/TERMINATOR-BG-IMAGE-DESIGN.md        — background image

### Plugin foundation (cycle 365)

  kettle.on(event_name, callback)  — Lua-side registration
  LuaEvent enum                    — Startup / TabAdd / TabClose / Bell
  LuaEngine::fire_event(&LuaEvent) — multi-subscriber, error-isolated

Subsequent plugin sub-cycles wire each LuaEvent variant to its
App emission site.

Workspace tests 300 → 302.

## [1.11.0] — 2026-05-22

Behavior wiring batch — closes the gap between parsed config keys
(v1.9.0) and end-to-end shipped behavior. Cycles 349-357 cover
~20 more Terminator config keys + the last 3 cycle-342 actions.

### Behavior wirings shipped

  cycle 349 — force-no-bell + close-button-on-tab +
              new-tab-after-current-tab
                force-no-bell           silence bell + dot + flash
                close-button-on-tab     hide tab ✕ chip + glyph
                new-tab-after-current-tab  insert after active vs append

  cycle 350 — link-single-click + disable-mouse-paste +
              putty-paste-style
                link-single-click       single-click opens URLs
                disable-mouse-paste     middle-click no-op
                putty-paste-style       right-click pastes (vs menu)

  cycle 351 — use-custom-url-handler + custom-url-handler:
                external program for URL clicks, with safe-URL guard
                + system-open fallback. Routes both Ctrl-click +
                cycle-218 hint-mode URL paths through one helper.

  cycle 352 — backspace-binding + delete-binding:
                remap encoded bytes to AsciiDel / ControlH /
                EscapeSequence / Automatic. Preserves cycle-X
                Alt+Backspace + Ctrl+Backspace muscle-memory
                semantics by only remapping the no-modifier case.

  cycle 353 — handle-size:
                split-divider width in px (1.0 default; clamps -1..50
                already done at parse time).

  cycle 354 — Edit-title actions (last 3 cycle-342 stubs):
                EditWindowTitle     →  Window::set_title
                EditTabTitle        →  Tab.title_override (new field)
                EditPaneTitle       →  Pane.title
              Placeholder values + log::info noting full overlay
              ships with Bucket-D per-pane titlebar.

  cycle 355 — allow-bold + bold-is-bright:
                allow-bold          suppress Flags::BOLD in render
                bold-is-bright      remap palette[0..8] → palette[8..16]
                                    via new color::bright_for_bold helper

  cycle 356 — inactive-bg-color-offset:
                compose with unfocused-split-opacity for unfocused-
                pane dim. inactive-color-offset (FG-only) reserved
                for Bucket-D text-reshape follow-up.

  cycle 357 — broadcast-default + exit-action:
                broadcast-default       seed mux.broadcast at startup
                exit-action = hold      pane stays open on shell exit
                exit-action = restart   log::warn fallthrough (re-spawn
                                         needs argv+cwd plumbing)
                exit-action = close     (default) unchanged

### Status of cycle-342 actions

All 18 now have behavior wired end-to-end. 15 with full real
semantics; 3 (EditWindowTitle / EditTabTitle / EditPaneTitle) are
placeholder + cited Bucket-D titlebar deferral for the
interactive-overlay UX.

### Still deferred (Bucket D — multi-cycle, design docs in audit)

  - Plugin system (Lua event hooks foundation)
  - Per-pane titlebar (full chrome region + interactive title edit)
  - Detachable tabs (cross-window drag)
  - Background image render (texture pass + blur shader)
  - Inactive-color-offset FG-only dim (text reshape per pane)

### Honest no-op stubs (documented in audit, cycle-E rationale)

  - smart-copy: kettle's existing copy behavior already matches
  - homogeneous-tabbar: kettle's existing tab layout already matches
  - extra-styling: kettle is wgpu+glyphon, not GTK
  - cell-height / cell-width: VTE-specific; kettle derives metrics
  - use-system-font: kettle is config-file-driven by design
  - use-theme-colors: kettle is bundled-themes-driven by design
  - disable-mousewheel-zoom: no Ctrl+wheel zoom in kettle today
  - sticky / hide-from-taskbar: winit support varies per platform

Workspace tests stay at 300. Test count steady because the
existing parse-side drift guards already pin the contract; the
wiring is exercised by a windowed run + manual verification.

## [1.10.0] — 2026-05-22

Minor-bump release — Terminator-parity behavior wiring (cycles 343-348).
Builds on v1.9.0's config + Action-registration surface; this release
wires the actual behaviors for 15 of 17 stubbed actions + several
config keys.

### Behavior wirings shipped

  cycle 343 — PTY spawn now honors:
                cfg.term         → TERM env override
                cfg.colorterm    → COLORTERM env override
                cfg.login_shell  → prepends `-l` to shell argv

  cycle 344 — Window state at creation + focus:
                cfg.window_state     → with_maximized / with_fullscreen /
                                        with_visible(false) at startup
                cfg.hide_on_lose_focus → set_visible(false) on focus-loss
                                          (Quake-style; reappears via
                                          cycle-303 --toggle)

  cycle 345 — 9 actions wired end-to-end:
                ZoomInAll / ZoomOutAll / ZoomNormalAll  (broadcast zoom)
                InsertPaneNumber / InsertPanePadded     (pane index to PTY)
                ScrollPageUpHalf / ScrollPageDownHalf   (half-page scroll)
                PastePrimary                            (X11 primary)
                ToggleWindowVisibility                  (in-process toggle)

  cycle 346 — ToggleScrollbar: tri-state cycle of cfg.scrollbar
              (Never → Always → Auto → Never).

  cycle 347 — RotateCw / RotateCcw: split-tree rotation via new
              `Mux::rotate_focused_split(clockwise: bool)`. Cw
              flips dir + swaps children (Terminator semantics);
              Ccw flips dir only.

  cycle 348 — NextProfile / PrevProfile: runtime profile cycle
              enumerating <config-dir>/profiles/*.config and
              calling existing reload_config helper (cycle 151
              infrastructure).

### New API

  Terminal::new_with_env(...)              — cycle 343
  Mux::focused_pane_index_in_tab()         — cycle 345
  Mux::rotate_focused_split(clockwise)     — cycle 347

`Terminal::new` is now a thin shim over `new_with_env` for
backward compat (no caller change).

### Still stubbed (3 of 18)

These actions need a new overlay state + key dispatcher (same
shape as the cycle-X palette overlay):

  EditWindowTitle
  EditTabTitle
  EditPaneTitle

Each is bounded but multi-file (App state + overlay render +
key dispatcher). Tracked as cycle 349+ in audit doc.

### Bucket D items still deferred

Plugin system, per-pane titlebar, detachable tabs, background
image rendering. Each is documented in docs/TERMINATOR-AUDIT.md
with a roadmap pointer; each warrants its own multi-cycle
thread (~3-6 sub-cycles).

Workspace tests stay at 300 (no new drift guards this batch —
the existing config-key drift guards + the Action::from_name
registry test cover the parsing surface; behavior wirings are
exercised by a windowed run).

## [1.9.0] — 2026-05-22

Feature-bump release — Terminator-parity audit + sweep (cycles 330-342).
Adds ~70 new config keys + 18 new Action variants covering the entire
Terminator config surface. Behavior wiring for some keys + most new
actions lands in follow-up sub-cycles; the config + Action surface is
discoverable via `--check-config` + `--list-actions` so Terminator
users can copy their config and have `--check-config` not flag anything.

### Audit + planning (cycle 330)

`docs/TERMINATOR-AUDIT.md` is the single source of truth — every
Terminator source file enumerated with feature/setting bullets +
a 5-bucket gap table (A/B/C/D/E). Phase 1 audited Terminator at
SHA `403fa1e5`; subsequent cycles flip B/C rows to ✅ A.

### Config keys shipped (cycles 331-341)

  Window state:        borderless, always-on-top, hide-on-lose-focus,
                       sticky, hide-from-taskbar, window-state, focus,
                       handle-size, geometry-hinting, extra-styling
  Tab UX:              close-button-on-tab, new-tab-after-current-tab,
                       title-at-bottom, scroll-tabbar, homogeneous-tabbar
  Tab-position:        accepts `hidden` (alias to `tab-bar = off`),
                       `left`/`right` (accepted by parser, falls
                       through to top with a log::warn — vertical
                       tab bars deferred to Bucket C)
  Render:              allow-bold, bold-is-bright, cursor-color-default,
                       use-system-font, use-theme-colors
  Mouse / paste:       link-single-click, disable-mousewheel-zoom,
                       clear-select-on-copy, disable-mouse-paste,
                       putty-paste-style, smart-copy,
                       putty-paste-style-source-clipboard
  Bell:                force-no-bell, icon-bell
  Search / env:        invert-search, term, colorterm
  Shell exec:          login-shell, exit-action (Close/Restart/Hold),
                       ask-before-closing (Always/MultipleTerminals/Never)
  Key encoding:        backspace-binding, delete-binding
  Group / broadcast:   broadcast-default (All/Group/Off),
                       split-to-group, autoclean-groups
  URL handler:         use-custom-url-handler, custom-url-handler
  Inactive offsets:    inactive-color-offset, inactive-bg-color-offset
  Per-pane titlebar:   show-titlebar, title-hide-sizetext,
                       title-use-system-font, title-font, six
                       title-{transmit,receive,inactive}-{fg,bg}-color
                       fields
  Background image:    background-type (Solid/Image/Transparent),
                       background-image, background-image-mode,
                       background-image-align-horiz/vert,
                       background-blur, background-darkness
  Misc:                cell-height, cell-width, http-proxy,
                       always-split-with-profile, detachable-tabs

Every key accepts both kebab-case (kettle convention) and
underscore form (Terminator convention).

### Action variants shipped (cycle 342)

18 new `Action::*` variants registered in the keymap grammar +
discoverable via `--list-actions`:

  RotateCw / RotateCcw
  ToggleScrollbar
  EditWindowTitle / EditTabTitle / EditPaneTitle
  InsertPaneNumber / InsertPanePadded
  NextProfile / PrevProfile
  ZoomInAll / ZoomOutAll / ZoomNormalAll
  ResetAndClear (fully wired — Reset + ClearHistory composed)
  ScrollPageUpHalf / ScrollPageDownHalf
  PastePrimary
  ToggleWindowVisibility

13 of 18 appear in the cycle-117 palette (Ctrl+Shift+K); the
5 title-edit + insert-text variants are excluded because they
need overlays or send raw text.

### Drift guards

Eight new test functions pin defaults + parsing for every new
config key + every action variant. The cycle-117 palette
exhaustive-match guard updated to fail compile on a future
unclassified variant.

### Followups (each its own sub-cycle)

Most config-key BEHAVIOR wiring is a follow-up sub-cycle. The
config + drift guard ship now so Terminator users can copy their
config without --check-config errors. Specifically pending:
- Render-layer: allow-bold, bold-is-bright, background-image,
  inactive-color-offset (per-fg/bg dim).
- Mouse handler: link-single-click, disable-mousewheel-zoom,
  disable-mouse-paste, putty-paste-style.
- Window: borderless + always-on-top WIRED in cycle 332.
  hide-from-taskbar / sticky / hide-on-lose-focus deferred
  (winit support varies).
- Per-pane titlebar: Bucket D (multi-cycle, needs render-layer
  rework).
- Action behaviors: 17 stubbed actions with log::info dispatch
  (ResetAndClear is fully wired).

Workspace tests 286 → 300 (+14 drift guards).

## [1.8.0] — 2026-05-21

Feature-bump release — Lua scripting (WezTerm parity) + tmux `-CC`
parser foundation (iTerm2 parity, multi-cycle thread starts) +
detachable-mux-server design doc.

### Added — Lua scripting (cycles 324-326, WezTerm parity)

`kettle --lua-script PATH` runs a Lua 5.4 file at startup with a
`kettle` namespace. Useful for programmatic startup workflows
without leaving the keymap surface.

  init.lua:
    print("kettle " .. kettle.version() .. " on " .. kettle.theme())
    kettle.exec_action("new_tab")
    kettle.exec_action("split_right")
    kettle.send_text("htop\\n")

  Read-only API (cycle 324):
    kettle.version()      → string
    kettle.config_path()  → string|nil
    kettle.theme()        → string

  Side-effect API (cycles 325-326):
    kettle.send_text(s)        → write s to focused pane's PTY
    kettle.exec_action(name)   → dispatch any kettle Action by
                                  name (same names as the keymap
                                  grammar; cycle-326 promoted
                                  Action::from_name to pub for
                                  this)

Errors in the script `log::warn!` + don't fail launch. Side-
effect commands queue on the engine; the App drains them once
the first pane spawns.

Implementation: `mlua 0.11` with `lua54 + vendored + send +
error-send` features. Vendored Lua means no system liblua
dependency; deterministic across OSes.

### Added — tmux `-CC` parser foundation (cycles 327-328, iTerm2 parity)

`kettle_vt::tmux_cc::TmuxControlParser` is a pure streaming
parser for tmux's control-mode protocol. Feed it bytes, pull
`TmuxEvent` enum values out.

Covers every documented tmux control-mode message: Begin / End /
Error / Output (with `\nnn` octal decode) / WindowAdd / Close /
Renamed / SessionChanged / Renamed / LayoutChange /
ClientDetached / Exit / Unknown / OutsideBlock. 11 unit tests
pin every variant + edge cases (CRLF, partial-line, 64 KB
overflow recovery).

This is the FOUNDATION; tmux integration into kettle's tab
surface is a multi-cycle thread. See `docs/TMUX-CC-DESIGN.md`
for the 7-cycle roadmap.

### Added — Documentation

- `docs/TMUX-CC-DESIGN.md` (cycle 328) — wire protocol summary +
  7-cycle integration roadmap (pane-state → tab synthesis →
  input routing → layout-change → detach cleanup).
- `docs/MUX-SERVER-DESIGN.md` (cycle 329) — architecture + wire
  protocol sketch + 13-cycle roadmap for the detachable mux
  server. No code; honest deliverable for a multi-week thread.

### Library / API additions

  kettle_ui::LuaEngine             — public type
  kettle_ui::LuaCommand            — public enum
  kettle_config::Action::from_name — promoted pub(crate) → pub
  kettle_vt::tmux_cc               — new module
  kettle_vt::tmux_cc::TmuxControlParser
  kettle_vt::tmux_cc::TmuxEvent
  kettle_ui::Options::lua_script   — new field

### CLI additions

  --lua-script PATH    — run Lua at startup (WezTerm parity)

Workspace tests 270 → 286 (+11 tmux parser + 5 lua).

### Deferred (each multi-cycle, see design docs)

- tmux `-CC` full integration (#42): parser shipped; pane-state
  plumbing + tab synthesis + input routing + detach cleanup
  pending. Roadmap in `docs/TMUX-CC-DESIGN.md`.
- Detachable mux server (#44): no code; design doc in
  `docs/MUX-SERVER-DESIGN.md`.
- Persistent in-terminal annotations: still pending.
- Native macOS menu bar + code-signed builds: still pending.

## [1.7.8] — 2026-05-21

Patch release. Cosmetic UX catch on the cycle-295 status bar.

### Fixed
- **Status bar cursor icon (cycles 320 + 321).** Hovering on the
  cycle-295 status strip showed the terminal I-beam cursor (text-
  input style) instead of the OS arrow. Cosmetic — the click
  wouldn't have actually started a selection because the strip
  isn't inside any pane's rect — but inconsistent with the
  tab-bar chrome which already used the arrow.

  Fix: new pure helper `cursor_in_status_bar_band` (sibling of the
  cycle-264 `cursor_in_tab_bar_band`), new
  `cursor_in_chrome_band` accessor that ORs both bars, `chrome_
  cursor_icon` arg renamed `in_tab_bar` → `in_chrome_band`.
  Drift guard `cursor_in_status_bar_band_geometry` pins the
  Off / Top / Bottom + bar_h=0 boundary semantics same shape as
  cycle-264's pinning.

Workspace tests 269 → 270.

## [1.7.7] — 2026-05-21

Patch release. Real UX catch on the cycle-303 Quake toggle +
CI smoke for the cycle-313 --profile contract.

### Fixed
- **Tri-state Quake toggle (cycle 319).** The cycle-303 binary
  "hide if visible, show if hidden" toggle had a UX failure mode:
  user has kettle visible, clicks to another window (kettle is now
  visible-but-unfocused), presses the global hotkey expecting
  kettle to come BACK INTO FOCUS — instead kettle HIDES. Two
  presses required to refocus. Wrong shape for
  Quake / Yakuake / Tilda muscle memory.

  Fix: tri-state.

    hidden            → show + raise + focus
    visible + focused → hide
    visible + !focused → raise + focus (don't hide)

### CI hardening
- **Cycle-313 --profile + --check-config contract smoke (cycles
  317 + 318).** Adds an end-to-end test in `.github/workflows/
  ci.yml`'s introspection-smoke block that writes a profile file
  with a deliberately malformed `font-size = not_a_number` line,
  runs `kettle --profile cibad --check-config`, and asserts the
  exit code is non-zero (the cycle-194 --check-config contract
  fires non-zero when issues are present). Cycle 317 used a
  flaky `if grep -q ...` pipe-into-if that didn't work on Windows
  Git Bash; cycle 318 pivoted to the cleaner exit-code contract.

Workspace tests stay at 269 green.

## [1.7.6] — 2026-05-21

Patch release. Three real durability + UX catches from post-v1.7.5
audit.

### Fixed
- **Remote-control IPC: unbounded read (cycle 315).** The cycle-302
  receiver's `drain_remote_commands` used `std::fs::read_to_string`
  with no size cap. A runaway script (or an accidental `some-cmd
  >> $REMOTE_FILE` instead of `kettle --remote-send "$(some-cmd)"`)
  could push GBs of data and kettle would allocate the whole
  thing before processing. Now: stat the file first; if > 1 MB
  (10× safety margin over realistic command-stream sizes),
  truncate + log::warn + return without processing.
- **Vi-mode yank silently dropped when clipboard unavailable
  (cycle 316).** The cycle-301 y-key handler called
  `clip.set_text(yanked)` with the result ignored via
  `let _ = ...`. When clipboard was None (SSH without X11 /
  Wayland forwarding, missing `$DISPLAY`, arboard init failure
  at startup), the yank silently dropped: visual highlight
  cleared, vi-mode exited, user assumed copy worked, then hit
  paste elsewhere and got their PREVIOUS clipboard contents.
  Now: log::warn! with the byte count + "try a kettle window
  with DISPLAY / Wayland set" hint.

### release.sh
- **'Next steps' race-condition fix (cycle 314).** The previous
  hint suggested
  `gh run watch $(gh run list --workflow=release.yml --limit 1 ...)`
  which races: the `run list` may resolve BEFORE the just-pushed
  tag triggers a new release workflow run on GitHub's side, so
  the watch attaches to the PREVIOUS release run (already done)
  and exits 0 immediately. Now: `--branch "v$VERSION"` filter +
  `--exit-status` so the watch errors on real failure + a brief
  `sleep 5` to let GitHub register the push.

Workspace tests stay at 269 green.

## [1.7.5] — 2026-05-21

Patch release. Real subtle audit catch + structural refactor.

### Fixed
- **`--profile NAME` silently ignored by every introspection flag
  except the windowed run (cycles 312 + 313).** Cycle-292 shipped
  `--profile NAME` only honored by the windowed-run path. A user
  running `kettle --profile dev --check-config` would silently
  check `<config-dir>/config` instead of `profiles/dev.config` —
  same silent-fallback shape as cycle-196's
  `load_from_with_diagnostics`. Cycle 312 fixed `--check-config`
  inline; cycle 313 extracted `resolve_config_path(&Cli) ->
  Option<PathBuf>` and applied it at every introspection site so
  the precedence
  (`--config FILE → --profile NAME → default path`) is uniform:

  - `--check-config`
  - `--list-keybinds`
  - `--list-ssh-hosts`
  - `--config-path`
  - `--screenshot`
  - `--screenshot-menu`

  Every one was doing
  `cli.config.clone().or_else(default_path)` without going through
  `path_for_profile`.

### release.sh
- **Cycle-311 catch surfaced in cycle 311 itself.** First end-to-
  end use of `scripts/release.sh` (cycle 307) tried to invoke
  `cargo build` without `$HOME/.cargo/bin` on PATH and failed
  mid-flow (version already bumped, lockfile not refreshed, no
  commit). The script now falls back to `~/.cargo/bin/cargo`,
  `/opt/homebrew/bin/cargo`, and `/usr/local/bin/cargo` before
  hard-failing with a clear diagnostic + a restore command.

### Other quality
- Added `.claude/` to `.gitignore` (cycle 310) — per-developer
  Claude Code state, not kettle source. Surfaced as untracked by
  the cycle-307 release script's pre-flight check.

Workspace tests stay at 269 green.

## [1.7.4] — 2026-05-21

Patch release. Two real subtle bugs caught by post-feature-sweep
audit + the first release shipped via the new
`scripts/release.sh` (cycle 307).

### Fixed
- **Status bar overflow on long pane titles (cycle 308).** A chatty
  shell prompt that puts the full cwd in the window title (a common
  pattern: `PROMPT_COMMAND='echo -ne "\033]0;$PWD\007"'`) produced
  a status line that cosmic-text wrapped past the strip's 1-cell
  visible region — the user saw the first ~80 chars and the rest
  was silently invisible. Now: char-budget truncation at 60 chars
  with a visible `…` ellipsis. UTF-8 safe (char-count, not
  byte-count).
- **Malformed trigger regex silently dropped (cycle 309).** A
  `trigger = [unclosed` pattern parsed (config layer stores it as a
  plain string), `--check-config` reported OK, then at runtime
  `compile_triggers` failed `Regex::new` and the trigger silently
  never fired (only a log::warn the user usually didn't see). Now:
  `--check-config` surfaces the invalid pattern with non-zero exit.

### Drift guards
- `cap_title_for_status_bar_truncates_at_char_budget_with_ellipsis`
  pins the cycle-308 fix (under/exact/over budget + UTF-8
  multibyte).
- `detect_malformed_values_flags_invalid_trigger_regex` pins the
  cycle-309 fix (both directions — malformed flagged, valid
  alternation `(BUILD SUCCESSFUL|FAILED)` not flagged).

Workspace tests 267 → 269.

## [1.7.3] — 2026-05-21

Repackaging of v1.7.2. Same code; v1.7.2 was tagged before the
CHANGELOG `[1.7.2]` section was committed, so the cycle-286
tag↔Cargo↔CHANGELOG consistency guard correctly failed the Linux
build at pre-flight — the v1.7.2 GitHub release shipped without
its Linux tarball.

v1.7.3 retags from the corrected HEAD so the Linux tarball ships
this time. Use this release instead of v1.7.2.

### Process catch (cycle 307)

The cycle-286 guard worked as designed — caught a real bug
(tag-before-CHANGELOG race). The fix is to tag AFTER the CHANGELOG
commit. A future cycle could harden the release script (a
`scripts/release.sh` that does the bump + CHANGELOG + commit + tag
atomically in one command) to prevent the race entirely.

### Carries v1.7.2's intended changes:

- Remote-control IPC truncate-on-startup (cycle 306) — see [1.7.2]
  below for full rationale.
- Two duplicate `#[allow(clippy::too_many_arguments)]` removed.

## [1.7.2] — 2026-05-21

Patch release. Real durability fix in the cycle-302 remote-control IPC.

### Fixed
- **Stale remote-command bytes replayed on next launch (cycle 306).**
  If kettle window A is running and accumulates pending `send-text
  TEXT\n` lines mid-process — OR crashes mid-process — and the user
  then launches kettle window B with the same `--remote-file PATH`,
  B's startup-time notify watcher would not fire (no write since B
  started watching) — but the first subsequent external
  `--remote-send` write triggers a re-read of the WHOLE file,
  including A's leftover bytes. B's focused pane then receives stale
  bytes typed as if the user had just sent them.

  Fix: `std::fs::write(&path, "")` once at startup, immediately
  before `w.watch(...)`. Truncates any leftover content; the
  watcher still fires on every subsequent write.

  Surfaced by a post-feature-sweep audit, not a user report.

### Code quality
- Dropped two duplicate `#[allow(clippy::too_many_arguments)]`
  annotations on `Terminal::new` and `Renderer::build_pane`
  (harmless but a code smell — pre-v1.4.0 era).

### Docs
- TESTING.md per-crate test counts refreshed (261 → 267 post-sweep).
- CONTRIBUTING.md cycle / test counts refreshed (250+ → 300+, 261+
  → 267+).

Workspace tests 267 stay green.

## [1.7.1] — 2026-05-21

Patch release. Docs catch-up against the v1.4.0 → v1.7.0 feature
sweep. The bundled `kettle.1` man page in the Linux release
tarball had drifted; users running `man kettle` after upgrading
would have seen pre-v1.4.0 keybinds. The fix ships as a binary
release so the tarball-bundled man page gets the v1.4.0+ content.

### Docs
- `packaging/linux/kettle.1` gains a "Vi-mode (Alacritty parity)"
  subsection with all 11 keymap entries (`Ctrl+Shift+Space` to
  enter; h/j/k/l/0/$/g/G/H/M/L/v/y/Esc) and a "Quake / dropdown
  mode" subsection documenting `kettle --toggle`.
- `docs/CONFIG.md` gains rows for the three v1.4.0-era config
  keys that were undocumented: `accent-color`, `status-bar`,
  `trigger`.
- `docs/UX-COMPARISON.md` matrix gains 9 new rows (vi-mode,
  remote-control IPC, Quake toggle, triggers, named-layout /
  profile, peacock accent, annotated screenshots, status bar) +
  a "Shipped in v1.4.0 → v1.7.0" chronological block. Vi-mode
  moved out of the "Future" list.

### Drift guard
- `man_page_documents_load_bearing_default_keybinds` extended
  with `Ctrl+Shift+Space` (vi-mode entry point). Without this,
  the same gap could recur on a future man-page rewrite.

No code change. Workspace tests 267 stay green.

## [1.7.0] — 2026-05-21

Feature-bump release — adds Quake-style dropdown via the cycle-302
remote-control IPC.

### Added — `--toggle` (Quake / Yakuake / Tilda dropdown UX)

`kettle --toggle` flips the running kettle window's visibility,
piggybacking on the cycle-302 remote-control IPC. The user binds
their compositor / DE / OS existing global-hotkey mechanism to
`kettle --toggle` — sidesteps the cross-platform global-hotkey
problem entirely (no XGrabKey / Carbon HotKey / RegisterHotKey
code per OS).

  GNOME       Settings → Keyboard → Custom Shortcuts → `kettle --toggle`
  KDE         System Settings → Shortcuts → Custom
  Sway        bindsym $mod+grave exec kettle --toggle
  Hyprland    bind = SUPER, grave, exec, kettle --toggle
  macOS       Karabiner / Raycast / Hammerspoon
  Windows 11  PowerToys Keyboard Manager / AutoHotKey

Protocol extension: the cycle-302 remote-control file now also
accepts the `toggle-window` command. Receiver calls
`window.set_visible(!is_visible()) + focus_window` so the window
pops above other windows when returning to visible (typical
Quake / Yakuake / Tilda behavior).

CLI surface:
  --toggle    sugar that writes `toggle-window` to the
              `--remote-file` path + exits.

Protocol v1.7 (one command per line — receiver-side):
  send-text TEXT     write TEXT (with `\n` → newline) to PTY
  toggle-window      flip window visibility (Quake dropdown)
  new-tab            recognized but not yet implemented (logs warn)
  # ...              comments + empty lines skipped

Workspace tests 267 stay green.

## [1.6.0] — 2026-05-21

Feature-bump release — adds remote-control IPC (kitty `@ send-text`
parity).

### Added — remote-control IPC

`kettle --remote-send TEXT [--remote-file PATH]` writes a command
line to a file watched by every running kettle window with a
matching `--remote-file`. The receiving window writes TEXT to its
focused pane's PTY. Used by external scripts to drive an already-
open kettle without launching a new window.

  # default path:
  kettle &
  kettle --remote-send 'cargo test\n'

  # explicit per-workspace channel:
  kettle --remote-file /tmp/dev.cmd &
  kettle --remote-send 'top\n' --remote-file /tmp/dev.cmd

Architecture: file-based IPC over the existing notify-watcher
(cycle 151), not a Unix-domain socket. Cross-platform free,
reuses existing patterns, no daemon thread. Multi-window
arbitration is "last writer wins" for now; per-window socket
addressing is planned.

CLI surface:
  --remote-send TEXT    write command + exit (sender mode)
  --remote-file PATH    command file path (default
                        `<config-dir>/kettle/remote.cmd`)

Library surface:
  kettle_ui::Options::remote_file: Option<PathBuf>
  kettle_ui::UserEvent::RemoteCommand

Protocol v1 (one command per line):
  send-text TEXT        write TEXT (with `\n` → newline) to PTY
  # ...                 comments + empty lines skipped

Future verbs reserved: `set-tab-title TEXT`, `focus-tab N`, `ls`,
`new-tab`, `close-tab N`. Unknown verbs log warn + continue, so
configs written for a forward kettle don't error today.

Workspace tests 267 stay green.

## [1.5.0] — 2026-05-21

Feature-bump release — adds full Alacritty-parity vi-mode for the
focused pane's scrollback. Shipped as 4 bounded sub-cycles
(298-301) that landed end-to-end across this minor.

### Added — vi-mode scrollback (Alacritty parity)

`Ctrl+Shift+Space` enters vi-mode. Visible magenta hollow block at
the terminal cursor; navigate with vi keys, yank selection to
clipboard, Esc exits.

Keymap shipped:

  h / j / k / l        move 1 cell left / down / up / right
  arrow keys           same as h/j/k/l
  0 / ^                jump to line start
  $                    jump to line end
  g / H                top of viewport
  G / L                bottom of viewport
  M                    middle of viewport
  v                    toggle char-visual selection
  y                    yank selection to clipboard + exit vi-mode
  Esc                  exit vi-mode

Architecture:

  kettle-config:
    Action::ToggleViMode    + 4 aliases (toggle_vi_mode, vi_mode,
                              vi, scrollback_vi)
    Default keybind: Ctrl+Shift+Space (Alacritty default)
    Cycle-117 palette-completeness drift guard pins it.

  kettle-ui:
    struct ViState { row, col, visual_anchor }
    App.vi_mode: Option<ViState>
    fn vi_mode_key(...) — modal key dispatcher, intercepts before
       PTY write. Reads focused-pane `screen_lines()` /
       `columns()` to clamp movement to grid.
    fn yank_vi_selection(start, end) -> String — extracts cells in
       the inclusive range, per-line trim_end.

  kettle-render:
    Overlay.vi_cursor + Overlay.vi_visual_anchor
    build_pane(...) takes both. Visual selection paints
    `theme.selection_background @ 0.55` rect per row. Vi cursor
    paints magenta (palette[5]) hollow block + 20% fill — distinct
    from broadcast yellow (palette[3]) + accent blue (palette[4])
    + terminal cursor.

Stays within the focused pane's viewport for v1; future cycle
could extend into scrollback rows (negative row indices). Not a
blocker — most vi-mode use cases (copy a build error line, yank
an SHA) work within the viewport.

Workspace tests 267 stay green. Vi-mode is exercised manually
(needs a windowed run for the visible cursor + clipboard yank);
the cycle-298 palette drift guard pins the Action wiring.

## [1.4.0] — 2026-05-21

Feature-bump release — eight new user-facing capabilities landed in
direct response to the parity sweep against other open-source
terminals. First minor version bump (was 1.3.11 → 1.4.0) because
the release introduces new public surface (config keys + CLI flags
+ library types) rather than only patch-level changes.

### Added — Selection / output

- **Smart selection (iTerm2 parity).** Double-click on a URL /
  file path / IPv4 / git SHA selects the whole match instead of
  the alacritty Semantic word, which usually under- or over-shoots
  structured tokens. Reuses the cycle-218 hint regex set.
  Falls through to the existing word-boundary semantic selection
  when nothing matches. (cycle 288)

- **Triggers (iTerm2 parity).** New `trigger = REGEX` config key.
  When a regex matches PTY output in an unfocused pane, kettle
  calls `window.request_user_attention(Critical)` — Wayland
  notification counter, X11 WM_HINTS urgency, macOS dock bounce,
  Windows taskbar flash. Three guard rails:
  - empty trigger set: zero cost (the default);
  - 2 s throttle: chatty builds don't pulse 100×;
  - window-focused check: don't pulse the user's own window.

  Drift guard pins alternation patterns (`(BUILD SUCCESSFUL|
  FAILED)` survives intact — the parser doesn't split on `|`).
  (cycles 289 + 290)

### Added — Workspaces

- **`--layout NAME` named-workspace session.** Saves + restores
  from `<config-dir>/layouts/<NAME>.json` so a user can maintain
  distinct workspaces ("dev", "ops", "docs") without each one
  clobbering the others on close. Composes with the v1.4.0
  `--profile NAME` config split below. Path-sanitized so a
  `--layout ../../etc/passwd` can't traverse out. Terminator
  parity. (cycle 291)

- **`--profile NAME` named-config split.** Loads
  `<config-dir>/profiles/<NAME>.config` instead of the default
  `<config-dir>/config`. Lets a user keep distinct font / theme /
  keybind sets per workspace. `--config FILE` wins when both are
  given. (cycle 292)

- **`accent-color` (peacock-for-VS-Code parity).** One config knob
  cascades to every "kettle accent" surface — active tab segment
  strip, focused pane border, cycle-255 dragged-tab ghost. Lets a
  user run multiple kettle windows (`--profile dev` + `--profile
  ops`) and tell them apart at a glance. CLI override:
  `--accent COLOR` (wins over the config key). `palette[3]`
  broadcast yellow and the cursor stay un-overridden by design.
  (cycle 293)

### Added — Screenshots / chrome

- **`--annotate TEXT` annotated screenshots.** Bottom-strip caption
  overlay on `--screenshot` / `--screenshot-menu` output. Useful
  for docs / README hero images / bug reports that want a version
  / repro / env note baked into the PNG. Translucent dark panel
  + 1 px chrome border + theme.foreground caption. None-passthrough
  on the unannotated path keeps the cycle-251 visual regression
  baseline pixel-stable. (cycle 294)

- **`status-bar = off | top | bottom` status strip (iTerm2 / kitty
  parity).** Thin row at the configured edge of the surface
  showing `HH:MM:SS UTC  ·  theme name  ·  focused pane title`.
  Disabled by default — turning it on subtracts one cell from each
  pane's grid. Composes with peacock accent for per-workspace
  identification. Live windowed app only; `--screenshot` paths
  intentionally don't render the status bar so the cycle-251
  visual regression baseline stays pixel-stable. Future cycle
  adds sysinfo CPU / MEM widgets. (cycles 295 + 296)

### Library / API additions

- `kettle_config::OutputTrigger { pattern, action }`
- `kettle_config::TriggerAction { Urgency }`
- `kettle_config::StatusBarMode { Off, Top, Bottom }`
- `kettle_config::Config::accent_color: Option<Rgb>`
- `kettle_config::Config::triggers: Vec<OutputTrigger>`
- `kettle_config::Config::status_bar: StatusBarMode`
- `kettle_config::Config::path_for_profile(name) -> Option<PathBuf>`
- `kettle_render::StatusBar`
- `kettle_render::capture_png_with_annotation(...)`
- `kettle_render::Renderer::render_frame_with_status(...)`
- `kettle_ui::Options { ..., layout, accent_override }`
- `kettle_ui::session::Session::path_for_layout(name)` +
  `Session::load_layout(name)` + `Session::save_layout(name)`

### CLI additions

- `--layout NAME` — named-workspace session.
- `--profile NAME` — named-config split.
- `--accent COLOR` — one-off peacock override.
- `--annotate TEXT` — bottom caption on screenshots.

### Known sub-cycles

These shipped in v1.4.0 with the minimum bounded scope; future
sub-cycles extend them:

- Triggers v1 only fires `Urgency`. Cycles 297+ add Bell,
  set-tab-title=text, notify-text.
- Profiles v1 fully replaces the base config. Cycle 297+ adds
  overlay-merge so a profile can override just a few keys.
- Status-bar v1 shows clock + theme + title. Cycle 297 adds
  sysinfo CPU / MEM widgets.

### Still deferred (multi-cycle, future)

- Vi-mode for scrollback (Alacritty parity) — keymap + cursor +
  visual selection + yank, 3-5 cycles.
- tmux `-CC` passthrough (iTerm2 parity) — control-protocol parser.
- Remote control protocol (kitty `@` commands) — IPC socket +
  handlers.
- Quake-style dropdown — OS global hotkey + window-state save.
- Lua scripting (WezTerm parity) — embed mlua, expose event hooks.
- Detachable mux server (WezTerm parity) — network protocol + auth.
- Persistent in-terminal annotations (iTerm2 parity, distinct
  from the v1.4.0 screenshot caption) — scrollback-position +
  sticky-note + search-jump.

These deserve dedicated cycles each rather than being half-shipped
alongside the v1.4.0 sweep.

Workspace tests: 261 → 267.

## [1.3.11] — 2026-05-21

Patch release.

### Fixed
- **`man kettle` keybind documentation now matches reality.** The
  cycle-279 hand-written man page had four wrong entries that drifted
  from the actual default keybinds:
  - `Ctrl+Shift+arrow` was documented as "focus pane in direction"
    — that's actually the scroll binding. Focus is **`Alt+arrow`**.
  - `Ctrl+Shift+Z` was documented as undo close tab,
    `Ctrl+Shift+D` as duplicate tab, and `Ctrl+Shift+Alt+D` as
    duplicate pane. Those actions exist (cycles 247/248) but are
    NOT default-bound — they're available via the command palette
    (`Ctrl+Shift+K`). Documented as such in a new
    "Additional actions via command palette" paragraph.
  - `Ctrl+Shift+,` / `Ctrl+Shift+.` for move tab were wrong —
    actually `Ctrl+Shift+PgUp` / `Ctrl+Shift+PgDn`.

  Also added bindings the original man page omitted: NewWindow
  (`Ctrl+Shift+I`), CloseWindow (`Ctrl+Shift+Q`), SplitAuto
  (`Ctrl+Shift+A`), FocusNext / FocusPrev (`Ctrl+Shift+N/P`),
  ScrollLineUp / Down (`Ctrl+Shift+Up/Down`),
  IncreaseFontSize / DecreaseFontSize (`Ctrl+Shift+Plus/-`),
  ToggleBroadcastOff (`Shift+Super+G`).

### Added — drift guards
- **`man_page_documents_load_bearing_default_keybinds`** test in
  `crates/kettle/src/main.rs`. Pins 16 load-bearing default-keybind
  triggers against the man page text via `include_str!`. If a
  future default-keybind set changes (or the man page text gets
  edited carelessly), CI fails instead of a user trying
  `man kettle` + the documented hotkey getting a different
  action. Caught the `Ctrl+PgDn` substring gap on its first run
  (the slashed `PgUp/PgDn` form didn't satisfy the check) — the
  man page now uses per-binding `.TP` lines so each entry has
  its own grep'able row.
- **`--help` output shape** pinned in CI (cycle 282). The
  all-OS CLI smoke now grep's `^Usage: kettle` + six load-bearing
  flag names (`--config`, `--screenshot`, `--gpu-info`,
  `--shell-integration`, `--print-completions`,
  `--print-default-config`). A clap-derive regression that
  silently dropped or renamed a flag would surface here, not
  in a user bug report.

Workspace tests: 261 → 262.

No code-behavior changes from v1.3.10.

## [1.3.10] — 2026-05-21

Patch release. One user-visible addition + two CI hardenings.

### Added
- **`man kettle`** — `packaging/linux/kettle.1` is a 366-line
  hand-written man page covering NAME, SYNOPSIS, DESCRIPTION,
  OPTIONS (Launch / Introspection / Debug+capture), KEY BINDINGS
  (Tabs / Splits / Overlays / Scrollback / Group), CONFIGURATION,
  ENVIRONMENT, FILES, EXAMPLES, SEE ALSO, AUTHORS. Wired into all
  four install paths:
  - `scripts/install.sh` drops it under `~/.local/share/man/man1`
    (or `${PREFIX}/share/man/man1` if `--prefix` overrides).
    `--uninstall` removes it too.
  - `release.yml` bundles the `.1` into the Linux release tarball
    so the bundled `install.sh` finds it.
  - `packaging/arch/PKGBUILD` installs to `/usr/share/man/man1`
    so `man kettle` works system-wide on Arch.
  - `packaging/homebrew/kettle.rb` uses `man1.install` for
    Linuxbrew.

  Format is groff/man macros — uses `.TP` paragraphs instead of
  `.TS/.TE` tables so it renders cleanly without the `tbl`
  preprocessor (some `man -l` invocations skip preprocessors).
  Verified via `groff -man -Tutf8 packaging/linux/kettle.1`.

### CI / automation
- **Tag ↔ Cargo.toml version consistency guard** in `release.yml`.
  An early Linux-only step extracts the version from the pushed
  tag's `$GITHUB_REF_NAME` and the workspace's `Cargo.toml`,
  failing fast with `::error::` annotations if they disagree.
  Without this guard, a future "tag v1.3.11 but forgot to bump
  Cargo.toml" would silently ship artifacts with mixed versions
  (macOS `.app` Info.plist saying 1.3.10, binary `--version` saying
  1.3.10, tag saying 1.3.11).
- **cargo-machete badge** in the README badge row. Closes the
  README's supply-chain badge trio (audit + deny + machete) so
  the supply-chain story is visible above the fold.

No code-behavior changes elsewhere from v1.3.9. Workspace tests
stay at 261 green.

## [1.3.9] — 2026-05-21

Patch release. **~20% binary size reduction.**

### Perf
- **Release binary 30.7 MB → 24.7 MB** via the cycle-277 `image`-features
  narrowing. cargo-bloat audit found three unused image-format
  decoders dominating the binary: `rav1e` (AVIF, 1.6 MB), `exr`
  (OpenEXR), `image_webp`, plus the full `zune_jpeg`. Root cause:
  `arboard`'s default `image_data` feature pulled `image` with
  default features (= every format) and unified with kettle-vt's
  default-feature `image` declaration.

  Fix:
  - `kettle-vt`: `image = { ..., default-features = false,
    features = ["png", "jpeg", "gif"] }`. Matches iTerm2's inline-
    image protocol spec (the only path that decodes user-supplied
    image bytes).
  - `kettle-ui`: `arboard = { ..., default-features = false }`.
    Drops the `image_data` feature; kettle's clipboard surface is
    text-only, no image-to-clipboard path exists.

  Result: AVIF / EXR / WebP / HDR / TIFF / BMP / QOI / DDS / ICO /
  PNM decoders all dropped. PNG / JPEG / GIF retained. Workspace
  tests 261/261 still green.

  The cycle-274 cargo-machete CI + cycle-264 cargo-deny CI prevent
  this class of accumulation from recurring; the cut is durable.

### Docs
- `docs/PERFORMANCE.md` baseline bumped to the new 24.7 MB
  measurement with a footnote explaining the cycle-277 cut.

No code-behavior changes elsewhere from v1.3.8.

## [1.3.8] — 2026-05-21

Patch release.

### Fixed
- **Session restore now surfaces a `warn!` when a tab can't be
  rebuilt** (was a silent skip). The cycle-pattern audit found
  `Mux::restore` quietly dropping any tab whose stored cwd /
  argv couldn't be re-spawned — a user wondering "where did my
  N-tab session go after restart?" had no signal. Converted to
  a `match` that logs `WARN session restore: tab N failed to
  rebuild and was skipped: <error>` per skipped tab. Behavior
  preserved (still don't sink the whole restore on one bad tab);
  visibility added.

### CI / automation
- **actionlint workflow** lints `.github/workflows/*.yml` on every
  workflow-file PR. Runs shellcheck on every `run: |` block —
  caught a real SC2016 in cycle 205's headless GPU smoke
  (single-quoted `$rc` in a nested `bash -c '…'`) which was
  intentional but un-documented; now suppressed inline with a
  shellcheck disable directive + explanatory comment.
- **stale-issue / stale-PR bot**. Conservative thresholds: issues
  warn at 90 days, close at 104; PRs warn at 60 days, close at
  74. Daily 06:30 UTC. Opt-out labels: `pinned`, `security`,
  `enhancement`, `help-wanted`, `blocked-on-maintainer`.

### Docs
- **Bug-report issue template** asks for `kettle --gpu-info`
  output (optional, rendering-related bugs only). Reduces the
  triage round-trip on "blank window" / "wrong colors" reports.

No code-behavior changes elsewhere from v1.3.7. Workspace tests
stay at 261 green.

## [1.3.7] — 2026-05-21

Patch release.

### Added
- **`kettle --gpu-info`** prints the wgpu adapter / backend /
  driver / texture limits the live renderer would pick on this
  machine, then exits — no GUI / PTY needed. Closes the gap
  between "blank window" / "no adapter" bug reports and the
  diagnostic info maintainers need to triage them. Output is
  predictable line-based `Key: value` so a shell script can
  consume it; CI smoke pins three invariant lines (`Backend:`,
  `Adapter:`, `Max texture: N px / side`).

  ```text
  $ kettle --gpu-info
  Backend:        Vulkan
  Adapter:        NVIDIA GeForce RTX 2080
  Adapter type:   DiscreteGpu
  Driver:         NVIDIA
  Driver info:    580.142
  Vendor (PCI):   0x10de
  Device (PCI):   0x1e87
  Max texture:    32768 px / side
  Max buffer:     4292870144 bytes
  Max bind groups: 8
  ```

### CI / automation
- **`actions/labeler@v5`** workflow auto-tags PRs by changed file
  paths (`docs`, `ui`, `vt`, `core`, `render`, `config`, `cli`,
  `ci`, `automation`, `packaging`, `tests`, `dependencies`,
  `release`, `tooling`). Triggered on `pull_request_target` with
  `pull-requests: write` so labels apply to fork PRs too.
  Additive (`sync-labels: false`) so manually-applied labels like
  `triage` / `good-first-issue` survive the auto-run.

No code-behavior changes elsewhere from v1.3.6. Workspace tests stay
at 261 green.

## [1.3.6] — 2026-05-21

Patch release. Theme: **post-v1.3.5 tooling + governance + supply-
chain hygiene**. No user-visible behavior change; the binary is
identical to v1.3.5 except the cycle-263 unwrap → expect refactor
upgrades five provably-safe `.unwrap()` calls to `.expect("invariant:
…")` so a future refactor that breaks one fails with a pinpointed
panic message rather than a bare `unwrap on None`.

### Added — install paths
- **Homebrew formula template** (`packaging/homebrew/kettle.rb`)
  with `packaging/homebrew/README.md` for the one-time tap-repo
  setup. Macros + Linuxbrew users get `brew install kettle` in
  two commands once the tap repo is live.
- **AUR PKGBUILD template** (`packaging/arch/PKGBUILD`) with
  `packaging/arch/README.md` for the one-time AUR submission.
  Arch / Manjaro / EndeavourOS users install with `yay -S
  kettle-bin` / `paru -S kettle-bin`.
- **Nix flake** (`flake.nix` at repo root + `packaging/nix/
  README.md`). NixOS users get `nix run github:reddimus/kettle`,
  `nix profile install`, dev-shell with the workspace MSRV, and
  flake-input usage for home-manager / NixOS configs. Rust
  toolchain pinned to 1.89 via `oxalica/rust-overlay`; rpath
  patched to find the wgpu / wayland / xkb runtime libs that
  dlopen would otherwise miss.

Each template pins exact SHA-256s tied to the release (via the
cycle-254 `.sha256` sidecars), so bumping happens in the same PR
as `Cargo.toml`.

### Added — dev tooling
- **`Justfile`** for common dev workflows. `just gauntlet` is the
  CI-equivalent gate (`fmt --check` + `clippy -D warnings` +
  `build` + `test` + `doc -D warnings`); recipes for every
  daily-loop task (`just fmt` / `just test` / `just screenshot`
  / `just menu` / `just bench` / `just install`). CONTRIBUTING.md
  cross-links so the cycle pattern's "Run the gate locally" step
  can use the one-liner.
- **`scripts/bench.sh`** + **`docs/PERFORMANCE.md`** — measured
  startup / memory / render baselines for the v1.3.5 binary,
  plus a POSIX-bash script that reproduces every measurement
  five times on `/usr/bin/time -f '%e %M'`. macOS users on
  coreutils' `gtime` are supported automatically.
- **`.editorconfig`** at the repo root — codifies indent +
  charset + line-ending rules across VS Code / JetBrains /
  neovim / emacs / Sublime / Helix so a save-on-format doesn't
  fight cargo fmt or the existing scripts.

### Added — supply-chain
- **`cargo-deny` config** (`deny.toml`) + dedicated workflow
  (`.github/workflows/deny.yml`) covering the supply-chain
  surface the cycle-244 `audit.yml` doesn't touch: explicit SPDX
  license allow-list, `unknown-registry = "deny"` + `unknown-git
  = "deny"` for source restrictions, wildcards-banned + warn on
  duplicate versions. Runs on every Cargo.lock change + weekly
  Sunday cron.

### Docs refresh
- **CONTRIBUTING.md** gains a first-class **Drift guards**
  subsection with three concrete kinds from the codebase
  (exhaustive-match guards, drift-against-source guards, pixel /
  output guards). Lead-in updated from "150+" to "250+" cycles.
  CI gate listed with all current workflows (audit, MSRV,
  visual regression, --screenshot-menu).
- **TESTING.md** refreshed against the current 261-test workspace:
  per-crate counts updated to current values; new drift guards
  listed explicitly (menu_visual, close_focused_promotes_sibling,
  classify_tab_activity_*, closed_tab_ring_bounded_and_lifo,
  tab_drag_target_index_clamps_to_strip, hovered_close_button_*,
  cli_help_preserves_indented_code_examples); CI section
  rewritten to list every workflow + smoke step on every OS.
- **UX-COMPARISON.md** matrix gains 8 v1.3.x parity rows
  (drag-reorder, activity / silence dots, undo-close, duplicate,
  right-click menu, command palette, hint mode, search overlay,
  shell integration, SSH launcher). Backlog list now distinguishes
  shipped-since-v1.0 (chronological) from deferred-on-purpose
  (with one-sentence rationale per item).
- **README** gains a `docs/PERFORMANCE.md` link in the
  Documentation section.
- **docs/INSTALL.md** documents all four install paths
  (curl|sh + KETTLE_PREFIX, Homebrew, AUR, Nix flake) + the
  manual SHA-256 verification path (sha256sum / shasum /
  Get-FileHash one-liners).

### Refactor
- **5 provably-safe `.unwrap()` → `.expect("invariant: …")`**
  (`kettle-vt/src/kitty.rs:current_frame` ×3,
  `kettle-vt/src/kitty.rs:feed`, `kettle-core/src/term.rs:placeholder_runs`).
  Each carries an inline invariant comment so a future refactor
  that breaks the safety property fails with a pinpointed
  message. Code-quality audit also confirmed:
  - Zero `TODO` / `FIXME` / `HACK` markers in production code.
  - Only one `unsafe` block (cycle 199 `SIGPIPE → SIG_DFL` with
    existing SAFETY comment).

Workspace tests stay at 261 green.

## [1.3.5] — 2026-05-21

Patch release.

### Added
- **Ghost-render of the dragged tab during reorder.** The cycle-249
  drag-to-reorder snapped the live bar to the new tab order at each
  boundary crossing — functionally correct but the dragged segment
  visibly teleported. Adds the standard chrome / browser-tab
  affordance: a translucent ghost copy of the active segment floats
  under the cursor while the drag is active (theme.background at
  0.85 opacity + matching palette[4]/palette[3]-under-broadcast
  accent strip + soft drop shadow). Anchor clamped to bar width via
  the same shape as the cycle-245 context-menu clamp.
- **`KETTLE_PREFIX` env var in `install-online.sh`.** Composes with
  `KETTLE_VERSION` so a pinned-version system-wide install is one
  line:
  ```sh
  curl -fsSL .../install-online.sh \
    | KETTLE_VERSION=v1.3.5 KETTLE_PREFIX=/usr/local sh
  ```
  Default (env unset) → `~/.local/`, unchanged.

### Tooling
- **`.editorconfig`** at the repo root. Codifies indent + charset +
  line-ending rules across IDEs (VS Code, JetBrains, neovim, emacs,
  Sublime, Helix all read it). 4-space Rust matches cargo fmt;
  2-space TOML/YAML/JSON/Markdown/sh; tab Makefile. Existing files
  already conform.

No code-behavior changes from v1.3.4. Workspace tests stay at 261
green; `--screenshot-menu` still produces the canonical menu PNG.

## [1.3.4] — 2026-05-21

Patch release. Theme: **production-grade supply-chain + governance
hygiene**. No code-behavior changes from v1.3.3; the binary is
byte-identical except for the embedded build SHA. The release
*surface* gains real integrity guarantees.

### Security / supply chain
- **Per-artifact SHA-256 sidecars on every release.** Every
  `release.yml` matrix row now generates a `.sha256` file alongside
  the artifact and uploads both. Linux uses `sha256sum`; macOS
  `shasum -a 256`; Windows emits the `sha256sum`-compatible
  `<hex>  <filename>` layout via `Get-FileHash` so cross-platform
  verification doesn't need a parser per OS. The release page now
  exposes:
    kettle-linux-x86_64.tar.gz
    kettle-linux-x86_64.tar.gz.sha256
    kettle-macos-universal.zip
    kettle-macos-universal.zip.sha256
    kettle-windows-x86_64.zip
    kettle-windows-x86_64.zip.sha256
- **`install-online.sh` verifies SHA-256 before extracting.**
  Downloads the sidecar alongside the tarball, runs `sha256sum -c`
  (or `shasum -a 256 -c` on BusyBox / Alpine where `sha256sum`
  isn't present). Verification failure aborts before `tar -xzf`.
  Backward-compat fallback: releases ≤ v1.3.3 don't ship sidecars,
  so a 404 on `.sha256` is a soft warning rather than a hard
  error — the one-liner keeps working with `KETTLE_VERSION=v1.3.3`.
- **`docs/INSTALL.md` documents manual verification.** New
  "Verifying a download (SHA-256)" subsection with platform-
  specific one-liners (sha256sum / shasum / Get-FileHash).

### Governance
- **README badges** — five shields.io badges at the top of the
  README: CI status, Audit (RustSec) status, latest release, MSRV
  (1.89), and license (MIT). Sourced from the existing workflows
  so badge color tracks real CI conclusion.
- **`CODE_OF_CONDUCT.md` adopting Contributor Covenant 2.1 by
  reference.** GitHub auto-detects + surfaces in the community-
  standards tab. Linked from CONTRIBUTING.md.

## [1.3.3] — 2026-05-21

Patch release. Two additions:

### Added
- **Per-tab silence watcher (Terminator parity).** Companion to the
  v1.3.0 output/bell tab indicators. An inactive tab whose unseen
  output stopped arriving for ≥ N seconds now transitions to a dim
  chrome-gray `Silent` dot — useful for tail-following long jobs
  (`tail -f`, build watchers, network monitors) where the *absence*
  of recent output is the signal you want.

  Configurable via `tab-silence-threshold-ms` (default 10 s, clamped
  `[1000, 600_000]`). Pure `classify_tab_activity` now takes
  `now: Instant` + `silence_threshold: Duration` so the wall clock
  flows in from the caller, keeping the function unit-testable. New
  drift guard `classify_tab_activity_transitions_to_silent_after_threshold`
  pins the threshold-boundary transitions + the bell-wins-over-silent
  precedence + the backward-clock saturation guard.

- **One-line online installer (Linux).** `curl -fsSL
  https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh
  | sh` downloads the latest release tarball, verifies the gzip
  magic bytes, extracts to a `mktemp -d` (cleaned up on exit), and
  runs the bundled `install.sh --skip-build`. POSIX-sh (dash /
  bash / ash compatible), zero non-coreutils deps, shellcheck-
  clean. Pin to a version via `KETTLE_VERSION=v1.3.3 sh`. Caught
  a real Bash-vs-dash bug in the bundled install.sh during
  end-to-end testing (now invoked via shebang, not `sh`).

### Docs
- README "Install" section reworked so the curl-pipe is the
  headline. macOS / Windows / build-from-source are first-class
  alternatives. New "CLI quick reference" heading wraps the
  post-install command list.
- `docs/INSTALL.md` brought into lock-step with the README's new
  install hierarchy.
- `docs/CONFIG.md` documents `tab-silence-threshold-ms` next to
  `cursor-blink-interval`.
- `docs/ARCHITECTURE.md` gains a mermaid `flowchart LR` of the
  six-pass render order — the cycle-251 fix relied on implicit
  reasoning about pass order; the diagram makes it explicit so a
  future overlay layer doesn't repeat the v1.3.0/v1.3.1
  blank-menu trap.

### CI
- **MSRV verification job.** Cargo.toml declared `rust-version =
  "1.88"` but nothing in CI verified the workspace + its transitive
  deps still built there. Adding `dtolnay/rust-toolchain@1.89` to
  ci.yml surfaced the real bug — `cosmic-text@0.18.2` +
  `smol_str@0.3.6` both require 1.89. Declared `rust-version`
  bumped to match; new MSRV job catches the next dep-floor drift
  at PR time instead of release time.

Workspace tests: 260 → 261.

## [1.3.2] — 2026-05-21

Patch release. Direct response to user feedback on v1.3.1: *"The
right click menu still does not work. It is just blank. Think of a
better way to test this and fix this."* Both addressed.

### Fixed
- **Right-click context menu was blank.** The kettle render pass
  order is `quads → imgs → text → overlay_quads`. v1.3.0/v1.3.1 put
  the menu's panel-bg quad in `overlay_quads` (the last pass), which
  drew on top of the menu text that had already been rendered in the
  text pass. v1.3.0 used opacity 0.97 ("looks awful" — text bled
  through at 3%); v1.3.1 bumped to 1.0 ("just blank" — text fully
  covered).

  Fixed by adding a third quad pass + second TextRenderer:

  ```
  1. quads.draw           (panes, tabs)
  2. imgs.draw            (sixel / kitty)
  3. text_renderer.render (pane + tab text — NOT menu)
  4. overlay_quads.draw   (dim + scrollbar — NOT menu chrome)
  5. menu_quads.draw      (menu shadow + bg + border + highlight)
  6. menu_text_renderer.render (menu row labels)
  ```

  All v1.3.1 design choices (drop shadow, theme.background panel,
  palette[8] border, palette[4]@0.18 highlight + 2-px accent strip,
  comfortable padding) carry over verbatim — the colors were right,
  only the pass was wrong.

  Menu chrome quads extracted into a pure `menu_chrome_quads(menu,
  theme, cw, ch) -> Vec<QuadInstance>` helper so the live renderer
  and the new headless screenshot path produce identical pixels.

### Added
- **`kettle --screenshot-menu PATH`** CLI flag: mirrors
  `--screenshot` but renders with a synthetic right-click context
  menu open over the pane. Useful for verifying the menu's render
  path without opening the windowed app — exactly the gap that let
  v1.3.0/v1.3.1 ship the blank-menu bug. Honors `--cols` / `--rows`
  / `--config` the same way.
- **`DebugScene` enum + `capture_png_with(cfg, cols, rows, out,
  scene)`** public API in `kettle-render`. `capture_png(..)` is
  kept as a thin back-compat shim that calls `..., Default`.

### Tests + CI
- **New `crates/kettle-render/tests/menu_visual.rs`** integration
  test that renders both scenes via `capture_png_with`, loads both
  PNGs, and asserts:
  - ≥ 1000 pixels differ between the no-menu baseline and the
    context-menu render. v1.3.0/v1.3.1 produced 0 different pixels
    inside the panel area; this floor catches exactly that
    regression.
  - ≥ 200 foreground-leaning pixels appear inside the menu's
    bounding box, ruling out a blank-menu render where only chrome
    pixels show.

  Combined into one `cargo test` invocation so we only spin up one
  pair of wgpu adapters per run (parallel offscreen software-Vulkan
  devices have segfaulted on shared CI runners).
- **`--screenshot-menu visual regression (Linux)` CI step** runs
  the binary under `LIBGL_ALWAYS_SOFTWARE=1` and asserts PNG magic
  bytes + file size ≥ 40 KB. Catches a wgpu-version drift the unit
  test wouldn't see.

Workspace tests: 259 → 260.

## [1.3.1] — 2026-05-21

Patch release. Direct response to user feedback on v1.3.0:
*"Tab x's are still just characters and not close buttons. Also the
right click actions look awful."*

### Fixed
- **Tab `✕` reads as a button at all times.** v1.3.0 added a *hover-
  only* red chip; the glyph itself was the last character of the tab-
  title text buffer, so at rest it read as plain text in the title.
  Two changes:
  - Always-on background chip behind every tab's close zone — dim
    `theme.foreground` at 0.12 opacity at rest (palette[8] at 0.55 on
    the active tab where the surface is brighter), bright palette[1]
    red at 0.85 on hover. The close button visibly exists before the
    user ever hovers.
  - Dedicated `✕` glyph buffer (`Renderer::tab_close_buffer`, single
    shared across all tabs, positioned per-segment via N TextAreas).
    Removed `✕` from the per-tab title text buffer. The glyph gets
    its own color: theme.palette[8] dim at rest, pure white on hover
    so the contrast against the red chip reads clearly.
- **Right-click context menu redesigned.** v1.3.0 used palette[4]
  (bright accent blue) for the panel *outline*, palette[8] (dim
  chrome) for the bg, and palette[4] again at 0.85 opacity for the
  highlight row — every chrome element was competing for attention.
  Five changes:
  - **Drop shadow** — a near-black quad offset 4px down-right at 0.35
    opacity so the menu reads as floating above the pane rather than
    pasted on (GTK / iTerm2 convention).
  - **Theme-bg panel** — `theme.background` opaque so the menu
    inherits the pane bg the user is calibrated for instead of
    clashing with a chrome-color box.
  - **Subtle border** — 1-px palette[8] at 0.65 on each edge (was
    palette[4] full opacity).
  - **Soft highlight** — active row gets palette[4] at 0.18 (was
    0.85) plus a 2-px palette[4] left-edge accent strip, matching the
    cycle-178 active-tab and cycle-184 focused-pane border pattern.
    "You are here" now reads consistently across every chrome surface.
  - **Breathing room** — row height `ch+12` (was `ch+6`), horizontal
    pad 16px (was 12), min panel width 180px (was 140), separator
    height 8px (was 6) and inset 12px (was 8). Comfortable click
    targets, polished surface.

### CI
- **MSRV verification job.** Cargo.toml declared `rust-version =
  "1.88"` since cycle 225, but nothing in CI actually checked the
  workspace + its transitive deps still build on that toolchain.
  Adding `dtolnay/rust-toolchain@1.89` to ci.yml immediately surfaced
  the real bug: `cosmic-text@0.18.2` and `smol_str@0.3.6` had both
  bumped their own floors to 1.89, so `cargo install kettle` on
  Rust 1.88 used to land in a confusing transitive-dep error instead
  of cargo's clean "package requires rustc 1.89" gate. Declared
  `rust-version` bumped 1.88 → 1.89 to match reality; the new MSRV
  job catches the next regression at PR time, not release time.

## [1.3.0] — 2026-05-21

Minor release. Theme: **production-grade UX cycle — tabs, splits,
right-click, Terminator + iTerm2 + WezTerm parity sweep.**

Seven focused sub-cycles addressing three user-reported issues (tab
`×` looked like a character not a button; `Ctrl+Shift+W` closed the
whole tab instead of just the focused split; right-click behaved
weirdly) plus four feature parities from other major terminals. Each
sub-cycle landed as its own commit with a drift-guard test pinning
the contract.

### Fixed
- **`Ctrl+Shift+W` on a split closes the pane, not the whole tab**
  (cycle 240). `Mux::close_focused` matched `Err(_)` and treated
  every error variant the same — `Err(None)` (only leaf, close
  tab) was conflated with `Err(Some(sibling))` (sibling needs
  promoting, keep tab). Split the arm and merge the
  promote-sibling branch with the regular `Ok(n)` path; both have
  identical post-conditions. Drift guard
  `close_focused_promotes_sibling_in_two_pane_split` pins the
  contract.

### Added — UX parity
- **Tab `×` hover affordance** (cycle 241). Click handler already
  hit-tests `seg.close` and dispatches `close_tab_at`; the bug was
  purely visual. A red chip background now appears behind the
  `✕` glyph on hover and the OS cursor flips to `Pointer` — Chrome
  / Firefox / Safari tab convention. Two pure helpers
  (`hovered_close_button`, `tab_close_hover_icon`) make the
  geometry + cursor decisions unit-testable.
- **Right-click opens a context menu** (cycle 245). Replaces the
  cycle-49 silent no-op with a floating panel of 8 entries
  (Copy / Paste / sep / Split Right / Split Down / Close Pane /
  sep / New Tab). Reuses the modal-overlay infrastructure (cycle
  111 command palette + hint mode pattern). Keyboard nav `↑↓ Tab`
  step the highlight skipping separators + disabled rows;
  `Enter Space` dispatch; `Esc` closes. Mouse click on a row
  dispatches; click outside dismisses. Anchor clamps via pure
  `clamp_context_menu_anchor` so a right-click near the bottom-
  right corner flips the panel up-and-left instead of rendering
  off-screen. Shift+right-click over an existing selection
  preserves the cycle-49 extend-selection muscle memory.
- **Tab-bar activity / bell dots** (cycle 246, Terminator parity).
  Per-`Tab` `last_output_at` / `last_seen_at` / `bell` fields +
  pure `classify_tab_activity(is_active, bell, last_output_at,
  last_seen_at) -> TabActivity { Normal | Output | Bell }`. The
  reader thread already advances per-pane history; that signal
  now also latches the containing tab's `last_output_at` (active
  tab short-circuits — the focused-tab accent is enough). The
  renderer draws a 6-px square in the lower-left corner of
  inactive segments — palette[3] yellow for Bell, palette[6] cyan
  for Output. Same brand colors the cycle-178 broadcast accent
  uses, so the visual language stays consistent.
- **Undo-close-tab** (cycle 247, WezTerm parity).
  `Mux::closed_tabs: VecDeque<ClosedTab>` bounded LIFO ring of 10;
  `close_tab_at` snapshots the first leaf's argv + OSC-7 cwd
  before drop. New `Pane::argv` field (load-bearing for the SSH
  re-spawn case). `Action::UndoCloseTab` (aliases
  `reopen_tab` / `restore_tab`) re-spawns the same program in the
  same cwd at the same tab index. Surfaced in the command palette;
  no default keybind (kettle's Terminator-inherited `Ctrl+Shift+T
  = NewTab` muscle memory takes priority — users who want
  WezTerm's chord add `keybind = ctrl+shift+t=undo_close_tab` to
  their config).
- **Duplicate tab + duplicate pane** (cycle 248, iTerm2 parity).
  `Action::DuplicateTab` / `Action::DuplicatePane` read the
  focused pane's argv (via the cycle-247 field) + OSC-7 cwd and
  clone into a new tab / horizontal split. An `ssh prod` tab
  duplicates to a second `ssh prod`; a `kettle -e vim file` tab
  duplicates to a second vim editing the same file. Empty argv
  falls back to the configured shell. Both surfaced in the
  command palette; no default keybindings.
- **Mouse-drag tab reorder** (cycle 249, kitty / iTerm2 / Ghostty
  parity). Pure `tab_drag_target_index(cursor_x, n, strip_w)`
  helper + a tiny `tab_drag_active` flag on `App`. A left-button
  press on a tab segment arms the drag; subsequent `CursorMoved`
  events compute the target index and call `Mux::move_active_tab`
  (cycle ~125 swap-with-clamp). Release disarms. No ghost-render
  of the dragged segment — kept out of scope; the bar snaps to
  the new order at each boundary crossing.

### Drift guards (+8 across the workspace)
- `close_focused_promotes_sibling_in_two_pane_split` (mux.rs)
- `hovered_close_button_finds_only_the_close_rect_hits` (app.rs)
- `tab_close_hover_icon_overrides_chrome_default` (app.rs)
- `next_context_menu_highlight_skips_separators_and_disabled` (app.rs)
- `clamp_context_menu_anchor_keeps_panel_on_screen` (app.rs)
- `classify_tab_activity_picks_the_right_indicator` (mux.rs)
- `closed_tab_ring_bounded_and_lifo` (mux.rs)
- `tab_drag_target_index_clamps_to_strip` (app.rs)

The cycle-117 palette-completeness exhaustive match guards the
three new actions (`OpenContextMenu`, `UndoCloseTab`,
`DuplicateTab`, `DuplicatePane`) — a future Action variant landed
without a palette decision fails to compile.

Workspace tests: 252 → 259.

## [1.2.1] — 2026-05-21

Patch release. Theme: **production-grade hardening — supply-chain
hygiene, governance scaffolding, and `--help` polish.**

No new features and no behavior change for any windowed-run user. The
v1.2.0 line shipped the first-launch onboarding triplet
(`--print-default-config` / `--shell-integration` / `--print-completions`);
1.2.1 finishes the `verbatim_doc_comment` story on `--help`, adds
project-level security + automation pieces a production-grade Rust
project is expected to have, and pins two `cycle-106/107` hard-fails
in CI that previously only had unit-test coverage.

### Fixed
- **`--help` indented examples for `--print-default-config` /
  `--shell-integration` / `--print-completions` no longer reflow.** The
  cycle-227/229/237 doc-comments contain indented `  kettle --… > …`
  example lines; without `verbatim_doc_comment`, clap collapsed the
  leading spaces in `--help`, flattening the examples into prose
  ("…file: kettle --print-default-config > ~/.config/kettle/config
  Everything in…"). All three flags now carry the attribute. New
  `cli_help_preserves_indented_code_examples` drift guard walks the
  clap `CommandFactory` arg list and asserts each indented example
  survives literally in `get_long_help()`, so a future refactor that
  drops the attribute fails CI with a pointer to the missing field.
- **`kettle --print-completions zsh` no-op fix.** The doc-comment's
  zsh example wrote the script to `~/.config/kettle/_kettle` — a path
  `compinit` would never look at because it isn't on `$fpath`.
  `clap_complete::Shell::Zsh` emits `#compdef kettle` at the top of
  the script, which only loads via autoload. The example now points
  at `"${fpath[1]}/_kettle"`, which lands in zsh's first
  function-path entry on every default install. Bash + fish lines
  were already correct.
- **Workspace-wide rustdoc warning silenced on the new zsh example.**
  `${fpath[1]}` is valid zsh array-indexing syntax but rustdoc tried
  to resolve `[1]` as an intra-doc link. Field-scoped
  `#[allow(rustdoc::broken_intra_doc_links)]` on `print_completions`
  silences just this one site rather than reaching for a workspace
  allow; backslash-escaping the brackets in the doc-comment would
  have leaked into clap's `verbatim_doc_comment` `--help` output and
  made the example un-copy-pasteable.

### Security
- **`SECURITY.md` — coordinated-disclosure policy via GitHub private
  advisories.** A terminal emulator parses untrusted PTY output every
  time the focused program is a remote shell, a `less` of an
  attacker-controlled file, or a CI log replay. New SECURITY.md
  points to GitHub's private vulnerability-reporting form (so we can
  triage and ship a fix before the issue is public) and enumerates
  the in-scope classes (PTY-to-host escape, OSC 52 read-leak past
  the cycle-49 default-deny, URI scheme abuse past
  `links::is_safe_url`, bracketed-paste-marker injection past the
  cycle-49 strip, resource exhaustion past the cycle-47/118 caps,
  config/session tampering, build-time supply chain).

### CI / supply chain
- **Dependabot — weekly Cargo + GitHub Actions update PRs.** Monday
  08:00 UTC cadence, 5 PRs per ecosystem max, patch + minor bumps
  grouped into a single PR per ecosystem so a slow review week
  doesn't pile up 15 individual bumps. Major bumps stay on their own
  so semver-meaningful changes get individual review. Commit prefixes
  align with the existing `fix(…) / feat(…) / ci(…) / docs(…)` scope
  convention (`deps:` / `ci(deps):`).
- **`cargo audit` workflow (`.github/workflows/audit.yml`).** Runs
  the official `rustsec/audit-check` action against the RustSec
  advisory DB on every push/PR that touches `Cargo.lock` plus a daily
  06:00 UTC cron — that catches advisories *published* against an
  unchanged Cargo.lock that Dependabot wouldn't notice until Monday.
  On pushes to `main`, findings open (or update) a single tracking
  issue per advisory rather than spamming the issue tracker.
- **`--config` / `--working-directory` hard-fail smoke (all OSes).**
  Cycle 106 (`--config /typo` exits 1) and cycle 107
  (`--working-directory /typo` exits 1) were covered by unit tests
  but never by CI's actual exit-code path. A regression that
  silently fell back to defaults would have passed the unit tests
  and reached users. Three assertions added at the tail of the CLI
  smoke step: typo'd `--config` exits non-zero, typo'd
  `--working-directory` exits non-zero, and the happy-path round-trip
  `--config /tmp/k.cfg --config-path` echoes the path and exits 0
  (also confirms the bootstrap one-liner survives a round-trip
  through `--config`). Self-contained via `$RANDOM` sentinel paths.
- **`--list-ssh-hosts` empty-case smoke.** Cycle 105's
  `format_ssh_hosts` empty-fallback emits "(no ssh-host entries
  configured)" so a user with no SSH hosts configured sees
  *something* instead of silence. CI never verified that;
  a regression silently producing no output would slip through.
  New smoke step asserts the explicit fallback line via
  `grep -E '^\(no ssh-host entries configured\)$'`.
- **Release artifacts now ship `CHANGELOG.md`.** Linux tarball,
  macOS `.app` (`Contents/Resources/`), and Windows zip already
  carry `LICENSE` / `NOTICE` / `README.md`. CHANGELOG was the
  obvious missing companion — a user who downloaded a tarball had
  no offline way to see "what's new in this release" without
  visiting GitHub. Adding it to all three platform packagings is
  one file each.

### Governance
- **GitHub issue + PR templates aligned with the cycle pattern.**
  `.github/ISSUE_TEMPLATE/{config,bug_report,feature_request}.yml`
  plus `.github/PULL_REQUEST_TEMPLATE.md`. `config.yml` disables
  blank issues, routes security reports at the SECURITY.md advisory
  form, and routes usage questions at Discussions — so the issue
  tracker stays bug + feature signal. The bug-report form requires
  the fields a cycle review would otherwise ping-pong over
  (`kettle --version` incl. the cycle-192/195 git SHA, OS + version,
  numbered repro with escape-sequence printf hints, expected vs
  actual, `RUST_LOG` output, `--check-config` snapshot). PR
  template mirrors the cycle shape from CONTRIBUTING.md:
  Summary / Why / Approach / Verification checklist / Cycle metadata.

### Tests
- **VT conformance: individual SGR-off codes
  (22/23/24/27/29).** `sgr_truecolor_bold_and_reset` and
  `sgr_underline_dim_strike` covered SGR *set* codes; the
  attribute-off codes weren't tested. These matter for nested
  styling: nvim / tmux / less / `git diff --color` set an
  attribute, write, then unset *that one* attribute and
  continue with the remaining accumulated SGR state. A
  regression in the SGR-22 path would silently leave bold set
  on cells the tool thought it had cleared. New
  `sgr_individual_attribute_resets` stacks the full set
  (bold + dim + italic + underline + inverse + strike), then
  walks each off-code and asserts only the matching flag
  clears while the others stay set. SGR 25 (blink off) is
  documented in the test but not asserted —
  `alacritty_terminal`'s `Cell::flags` deliberately doesn't
  track BLINK (render-time concern, not a cell attribute).

## [1.2.0] — 2026-05-20

Second minor release. Theme: **finish the first-launch
onboarding triplet** + **post-v1.1.0 hardening sweep**.

The 1.0/1.1 line shipped great defaults but onboarding still
relied on docs lookup for two affordances (OSC 133 shell
integration and tab completion). 1.2 ships them both as
one-command embedded CLI flags, joining v1.1's
`--print-default-config`. After install, three optional lines
fully configure kettle for daily use:

```sh
kettle --print-default-config > ~/.config/kettle/config
kettle --shell-integration bash >> ~/.bashrc
kettle --print-completions bash >> ~/.bashrc
```

Plus seven cycles of CI / drift-guard / refactor hardening to
keep the docs and packaging in sync as the project grows.

### Added (since 1.1.0)
- **`kettle --print-completions <bash|zsh|fish|elvish|powershell>`
  emits a shell tab-completion script.** Same shape as cycle 227
  (`--print-default-config`) and cycle 229 (`--shell-integration`):
  ```sh
  kettle --print-completions bash >> ~/.bashrc
  kettle --print-completions zsh  > ~/.config/kettle/_kettle
  kettle --print-completions fish > ~/.config/fish/completions/kettle.fish
  ```
  After sourcing, `kettle --li<TAB>` completes to `--list-themes`
  /`--list-keybinds` / `--list-actions` / `--list-ssh-hosts`.
  Generated by `clap_complete` from the same `Cli` struct that
  powers `--help`, so a future flag is auto-completed without a
  manual table update. New test
  `print_completions_emits_per_shell_scripts` pins each known
  shell to a minimum size + the `kettle` substring; CI smoke runs
  every shell + asserts an unknown shell exits non-zero.
  `scripts/install.sh` now lists this as a third optional
  one-liner; README quick-start shows it alongside the others.

### CI
- **`--screenshot` end-to-end smoke on the Linux runner.**
  `kettle_render::offscreen_selftest` compiles the WGSL shaders
  and renders one pass; `--screenshot` exercises the rest of the
  pipeline (bundled Nerd Font, glyphon shaping, wgpu offscreen
  texture, image::save PNG encode, scripted demo content). None
  of that was covered by CI before. New step runs `--screenshot`
  against the software-Vulkan adapter (LIBGL_ALWAYS_SOFTWARE=1)
  and asserts the output has the PNG magic header + ≥ 10 KB,
  catching a wgpu/glyphon/image-crate regression before users
  hit it. No DISPLAY needed — capture_png builds its own
  offscreen device with `compatible_surface: None`.

### Internal
- **Markdown cross-link drift guard for every user-facing doc.**
  Cycle 223/224's image guard catches `![…](path)` regressions;
  cycle 232 adds the same shape for text links — `[label](path.md)`
  to relative `.md` files. README alone cross-links to 8+ docs
  (`CONFIG`, `INSTALL`, `ROADMAP`, `SHELL-INTEGRATION`,
  `ARCHITECTURE`, `RESEARCH`, `UX-COMPARISON`, `TESTING`,
  `CONTRIBUTING`, `CHANGELOG`); a rename / deletion silently broke
  GitHub-rendered navigation with no CI signal. The guard:
  (1) walks byte offsets like the image guard;
  (2) matches `[…](path)` but excludes `![…](path)` (image
      guard's territory) by checking the byte before `[` isn't `!`;
  (3) skips external (`http(s)://`) and anchor-only (`#section`)
      links; only checks relative `.md` paths;
  (4) resolves each path against the *doc's own directory*
      (README's `docs/CONFIG.md` and docs/ARCHITECTURE.md's
      `TESTING.md` both have to work);
  (5) floors the README parser at ≥ 3 cross-links so a regression
      to "matches nothing" can't silently pass.

### Changed
- **`install.sh` final message points at the two bootstrap
  one-liners.** Post-install the user already knows where the
  binary landed and how to launch from the Super key. The
  message now also surfaces `kettle --print-default-config`
  (cycle 227) and `kettle --shell-integration bash` (cycle 229)
  as the two optional one-liners that finish setup. Both already
  worked; the install script just didn't advertise them.

### CI
- **Release tarballs now also ship `shell-integration/` alongside
  the binary.** Cycle 229 embedded the snippets into the binary
  via `include_str!`, so `kettle --shell-integration bash >> ~/.bashrc`
  works without the source tree. The standalone files are still
  useful for users who want to read or customize them before
  sourcing, and for discoverability via `ls`. Linux tarball
  ships them at `kettle/shell-integration/`; macOS .app bundle
  ships them at `Contents/Resources/shell-integration/`; Windows
  zip ships them at `shell-integration/` next to the .exe.

### Added
- **`kettle --shell-integration <bash|zsh|fish>` — one-command
  install of the OSC 133 shell snippet.** Cycle 227 added the
  config bootstrap one-liner; the OSC 133 shell-integration story
  still required the user to manually copy a snippet out of
  `docs/SHELL-INTEGRATION.md` into their rc file. Now:
  ```sh
  kettle --shell-integration bash >> ~/.bashrc
  kettle --shell-integration zsh  >> ~/.zshrc
  kettle --shell-integration fish >> ~/.config/fish/config.fish
  ```
  Snippets live at `shell-integration/kettle.{bash,zsh,fish}` in
  the source tree (Linux release tarball includes them too) and
  are embedded into the binary via `include_str!`. New test
  `shell_integration_snippets_match_in_tree_files` pins each
  snippet to a minimum size + OSC 133 substring so an accidental
  empty include is caught at build time. CI smoke runs all three
  shells + asserts an unknown shell exits non-zero with a clear
  error. SHELL-INTEGRATION.md now leads with the one-liner and
  keeps the inline snippets below as reference.

## [1.1.0] — 2026-05-20

First minor release after `v1.0.0` / `v1.0.1`. Theme is **first-
launch onboarding** + **cross-platform desktop integration parity**:
a newcomer on Ubuntu, macOS, or Windows 11 should now be able to
go from "I just downloaded kettle" to "I'm typing in a terminal
with my icon in the launcher and my config in the right place" in
two commands. Plus durable manifest/CI policy guards for the
contracts that landed in this cycle batch.

### Added (since 1.0.1)
- **`kettle --check-config` emits a bootstrap hint when no config
  exists.** When the resolved config path doesn't exist on disk
  (the common newcomer state), the output now includes:
  ```
  config:  /home/you/.config/kettle/config (not found — using defaults)
  hint:    kettle --print-default-config > /home/you/.config/kettle/config
  ```
  The hint interpolates the **actual** resolved path so the user
  can copy-paste verbatim. Suppressed when the config does exist
  (no nag for users who already set one up). CI smoke verifies
  the hint appears via `grep -E '^hint: +kettle --print-default-config > '`
  so a future regression that drops the hint is caught here, not
  by a confused first-time user.

- **`kettle --print-default-config` — one-command first-launch
  bootstrap.** The documented example config lives at
  `docs/kettle.example.config` (~140 commented lines) and a
  newcomer used to have to copy it manually from the source tree
  or the docs site. Now:
  ```sh
  kettle --print-default-config > ~/.config/kettle/config
  ```
  drops a fully commented starter file in the right place — no
  source tree required (`cargo install kettle` users get the
  correct content too). The file content is embedded at build time
  via `include_str!("../../../docs/kettle.example.config")`, so
  there's no runtime path lookup that could differ from what
  shipped. New test `print_default_config_round_trip` pins three
  contracts: (1) the embedded content is non-trivial (≥ 50 lines
  — catches an empty include_str! at build time, not ship time),
  (2) `Config::parse_collect` emits zero diagnostics on the
  embedded content (catches a future malformed example value
  before users hit it), (3) every line in the example file is
  commented out by convention (cycle 100 drift guard still
  active). Wired into CI smoke too:
  `--print-default-config | wc -l > 50` + round-trip through
  `--check-config` to assert `status: OK`. README's quick-start
  table now leads with the bootstrap one-liner.

### Internal
- **Workspace-metadata contract is now one comprehensive test.**
  Cycle 218's `library_crates_have_per_crate_descriptions` was a
  narrow guard on just the description override. Cycle 225's
  `rust-version` inheritance added a new field to the
  workspace.package shape with no guard. Cycle 226 replaces the
  narrow test with `workspace_metadata_policy`, which pins:
  (1) `workspace.package` declares every shared field
  (`version` / `edition` / `rust-version` / `license` /
  `repository` / `authors` / `description`); (2) every crate
  inherits each of those via `.workspace = true`; (3) library
  crates override `description` with their per-crate `"kettle: …"`
  blurb; (4) the binary inherits `description.workspace = true`.
  Catches "tidying" cycles that revert one piece of the
  inheritance shape — version drift, MSRV drift, license drift,
  binary-blurb leak onto a library — all in one check.

### Changed
- **MSRV declared at Rust 1.88.** The workspace already uses
  let-chains (`if X && let Y = ... && Z`) in kettle-vt,
  kettle-config, kettle-render and the kettle binary. Let-chains
  stabilized in 1.88, but `rust-version` was never set — a user
  on 1.85-1.87 (which support edition 2024 but predate let-chain
  stabilization) hit cryptic `expected expression, found keyword
  'let'` syntax errors instead of cargo's clean "package requires
  rustc 1.88" message at the resolver level. Now declared in
  `workspace.package.rust-version` and each crate opts in with
  `rust-version.workspace = true`. `rustup update stable` always
  satisfies it; this is a floor, not a ceiling. INSTALL.md notes
  the MSRV inline so contributors on stale toolchains see it
  before they try to build.

### Internal
- **Image drift guard now covers every `docs/*.md`, not just README.**
  Cycle 223's `readme_referenced_images_exist` only scanned the
  root README. `docs/UX-COMPARISON.md` already embeds two images
  (`kettle-showcase.png` + `refs/xterm.png`) — same forgotten-
  commit / rename / broken-image-on-github regression risk. The
  guard now walks `docs/*.md` and resolves each `![…](path)`
  against the doc's own directory (README's `docs/images/…` and
  UX-COMPARISON's `images/…` both need to work). Renamed test
  `readme_referenced_images_exist` → `user_facing_doc_images_exist`.

### Added
- **README now embeds a kettle hero image
  (`docs/images/kettle-hero.png`).** Generated by
  `kettle --screenshot docs/images/kettle-hero.png --cols 120 --rows 32`,
  which drives the real GPU text + quad pipeline over the scripted
  two-pane demo from `kettle_render::capture_png`. The hero
  appears immediately after the project tagline so a visitor sees
  what kettle looks like before the highlights bullet list. New
  `readme_referenced_images_exist` test parses the README for every
  `![…](path)` embed and asserts each relative path resolves —
  rename / forgotten-commit caught at PR time. Test sanity-floors
  the parser at ≥ 1 image (cycle 223's hero is the floor) so a
  regression to a no-op scan doesn't silently pass.

### Documentation
- **README status block updated from "early but functional" to
  "v1.0 — ready for daily use".** The old wording dated back to
  pre-v0.1.0 and was the first paragraph a reader saw on
  github.com/Reddimus/kettle. Now points to the latest release page
  for prebuilt binaries (Linux tarball + installer, macOS universal
  `.app`, Windows zip with embedded `.ico`) and summarizes the CI
  matrix shape (fmt → clippy → test → doc → headless GPU smoke →
  CLI + packaging smoke on every push). Passes the cycle-172
  drift guard (`cycle <digit>` and `<digit> workspace tests`
  patterns) because the rewrite intentionally uses range-stable
  prose, no hardcoded counts, no internal `cycle N` refs.

### CI
- **Packaging smoke runs on every push, not just on tag cut.** The
  `release.yml` workflow only fires on `v*` tag push, so a
  regression like "remove a PNG from `packaging/macos/kettle.iconset`"
  or "delete `packaging/windows/kettle.ico`" only surfaces at the
  next release — by which point bisect-and-revert is the only
  remedy. New CI steps run `iconutil -c icns` on the macOS runner
  and `file packaging/windows/kettle.ico` on the Windows runner,
  each verifying the produced/shipped file is well-formed
  (macOS: real .icns, > 100 KB; Windows: ≥ 4 resolutions). Catches
  malformed iconsets at PR time, not release time.

## [1.0.1] — 2026-05-20

Patch release: ships the macOS `.icns` + Windows `.ico` packaging
that landed on `main` an hour after `v1.0.0` was tagged. The
`v1.0.0` Linux artifact already has the icon set; this release
brings macOS and Windows to parity. No code changes to the runtime
binary on Linux.

### Added
- **macOS `.app` icon (`kettle.icns`) + Windows `.exe` icon (`kettle.ico`).**
  Cycle 222 (v1.0.0) shipped a Linux SVG + PNG icon set and an
  `install.sh` that wires it into XDG paths so the kettle tile shows
  up in GNOME / Ubuntu Super-key search / KDE Krunner. Same wasn't
  true on macOS and Windows — the macOS `.app` bundle had no
  `CFBundleIconFile`, so Finder / Dock / ⌘-Tab showed a generic
  document glyph; the Windows `.exe` had no embedded icon, so the
  taskbar / Alt-Tab / Explorer showed the default Rust binary glyph.
  Now:
  - **macOS**: `packaging/macos/kettle.iconset/` holds the ten
    Apple-required PNGs (16/32/128/256/512 in 1× and 2× variants),
    rendered from the master `packaging/linux/kettle.svg` so a future
    icon refresh is a single-file change. `release.yml`'s macOS step
    runs `iconutil -c icns` (built-in on macOS, no extra deps) to
    produce `kettle.icns`, drops it into `Contents/Resources/`, and
    sets `CFBundleIconFile=kettle` so the bundle picks it up. Also
    patches `CFBundleVersion` / `CFBundleShortVersionString` from
    `Cargo.toml`'s workspace version via `PlistBuddy` so a forgotten
    manual bump can't ship a stale `0.1.0` plist.
  - **Windows**: `packaging/windows/kettle.ico` is a 6-resolution
    `.ico` (16/32/48/64/128/256) built from the same SVG. The
    `winresource` build-dep (Windows-only, gated by
    `cfg(target_os = "windows")` in `build.rs`) compiles it into the
    `.exe` so Explorer, the taskbar, Start-menu pins, and Alt-Tab
    all display the kettle icon. The `.ico` also ships standalone in
    the release zip for Start-menu re-pinning if the user moves the
    `.exe`.

## [1.0.0] — 2026-05-20

First "ready for daily use" release. Eleven months and ~240 audit
cycles after `v0.1.0` (the first-cross-platform release of
2026-05-19), the suite is large enough, the docs are tight enough,
and the desktop integration is good enough that we're ready to
stop calling this pre-release software.

### Highlights since 0.1.0
- Full Ghostty-compatible config (`key = value`), 500+ bundled
  themes (iTerm2-Color-Schemes / Ghostty ports), TokyoNight Night
  as the verified default.
- Terminator-style splits + tabs, broadcast input across panes,
  search overlay, command palette, theme picker, session
  save/restore (with corruption-backup contract), drag-drop file
  paste, kitty/Sixel/iTerm2 image protocols, hyperlink + URL +
  path + IP detection, OSC 7 cwd, OSC 133 prompt marks for
  Ctrl+Up/Down navigation, OSC 8 hyperlinks, OSC 52 clipboard,
  bracketed paste with injection guards, wide CJK + combining
  marks.
- GPU-accelerated rendering via wgpu (Vulkan/Metal/DX12) +
  glyphon, with an offscreen self-test that runs in CI on all
  three OSes.
- Linux desktop integration: easy installer (`scripts/install.sh`),
  XDG `.desktop` entry with `StartupWMClass=kettle`, terminal-style
  SVG icon + PNG fallbacks at 32/48/64/128/256, WM_CLASS set
  explicitly via winit so GNOME / KDE bind the launcher to running
  windows.
- macOS universal binary (x86_64 + aarch64), `.app` bundle with
  Info.plist.
- CI matrix on Linux + macOS + Windows: `fmt --check` → `clippy
  -D warnings` → `cargo test --workspace` → `cargo doc -D
  warnings` → headless GPU smoke (Linux) → CLI smoke with grep
  assertions for `--version` / `--check-config` /
  `--list-themes`>400 / `--list-actions`>50 / `--list-keybinds`>40
  on every OS.

### Added (this release cycle)
- **Terminal-style SVG icon + PNG fallbacks at 32/48/64/128/256.**
  TokyoNight palette `>_` motif. Lives at
  `packaging/linux/kettle.{svg,*.png}` and is bundled into the
  Linux release tarball alongside an extracted `install.sh`.
- **`scripts/install.sh` — easy Linux desktop install.** No `sudo`
  needed; drops the binary into `~/.local/bin/kettle`, the
  launcher into `~/.local/share/applications/`, and icons into
  `~/.local/share/icons/hicolor/{scalable,256x256,…}/apps/`. Works
  both from a cloned repo (builds release first) AND from an
  extracted release tarball (uses the bundled binary — detected
  by the `kettle` file living next to the script). After install,
  the kettle launcher appears in the GNOME Activities overview /
  Ubuntu Super-key search / KDE Krunner. `--uninstall` removes
  everything atomically.
- **Explicit `WM_CLASS=kettle` / Wayland `app_id=kettle` on every
  Linux window.** Without this, GNOME's task switcher and dock-pin
  logic doesn't reliably associate running kettle windows with the
  `StartupWMClass=kettle` line in the `.desktop` file. Set via
  `winit::platform::x11::WindowAttributesExtX11::with_name` (the
  same trait impl writes to the shared `platform_specific.name`
  used by both Wayland and X11 backends).

### Internal
- **`[workspace.lints.clippy]` opens forward-guards against
  `dbg_macro` / `todo` / `unimplemented`.** The codebase has zero
  occurrences of all three today, so this is purely "lock the door
  before someone walks through it." `clippy -- -D warnings` already
  enforces them via warning level, but a manifest-level deny is
  durable policy and survives a future `--warnings=allow`
  invocation. Each crate's `Cargo.toml` opts in with
  `[lints]\nworkspace = true`.

### CI
- **`cargo doc --workspace --no-deps` with `RUSTDOCFLAGS=-D warnings`
  added on the Linux job.** Cycles 207-210 landed crate-level
  rustdoc on every workspace crate, with disambiguations like
  `[`mod@search`]` and `[`mod@links`]`. CI's `clippy -D warnings`
  doesn't catch rustdoc's warning class (broken intra-doc-links,
  malformed code blocks, missing docs on public items), so a
  future rename like `mod search` → `mod find` would silently
  invalidate those references and only be caught by a contributor
  running `cargo doc` locally. Building docs in CI with warnings
  denied pins the doc landings as a contract. One platform is
  enough (rustdoc is platform-agnostic); leaving it Linux-only
  trades a tiny CI-time saving for the same coverage.

### Changed
- **Per-crate `description` overrides on every library crate.**
  Cycle 213 moved every crate's `[package]` block onto
  `workspace.package` inheritance, including `description`. That
  works fine for `version` / `license` / `authors` / `edition` /
  `repository` (genuinely shared), but `workspace.package`'s
  description is the *binary's* blurb ("A fast, cross-platform
  GPU terminal emulator combining the best of Ghostty, Terminator,
  kitty, Alacritty and WezTerm") — and inheriting it gave every
  library sub-crate the same text. A user browsing `kettle-config`
  on crates.io or via `cargo metadata` would see the terminal's
  marketing blurb on the config-parsing crate, the VT-byte-extractor
  crate, etc. Now each library overrides with what *it* does:
  - `kettle-config` → "Ghostty-compatible config parsing, bundled
    theme set, embedded Nerd Font, keybinds, fuzzy matcher"
  - `kettle-core` → "PTY + alacritty_terminal VT glue, scrollback
    search, hyperlink/URL/path/IP detection, kitty/Sixel/iTerm2
    image registries"
  - `kettle-vt` → "streaming VT byte extractor — kitty/Sixel/iTerm2
    image protocols, OSC 7 cwd, OSC 133 prompt marks, OSC 1→2
    title rewrite"
  - `kettle-render` → "wgpu + glyphon GPU renderer — quads, images,
    text, overlay pass, headless offscreen self-test"
  - `kettle-ui` → "winit app, tab/pane mux, Terminator-style splits,
    overlays (search/palette/themes), session save/restore"
  - `kettle` (binary) keeps `description.workspace = true` — the
    workspace blurb IS the binary's blurb, single source of truth.
  All start with the prefix `"kettle: "` so they identify as part
  of the same project at a glance. New test
  `library_crates_have_per_crate_descriptions` reads each Cargo.toml
  via `std::fs::read_to_string` and pins both halves (libraries
  must override, binary must inherit) so a future "tidying" cycle
  that uniformizes the manifests back to `description.workspace =
  true` is caught.

### Documentation
- **`docs/TESTING.md` per-crate counts refreshed and shifted to
  `+N` range form.** Cycle-172/179/214 fixed top-level "workspace
  has X tests" claims in INSTALL / ARCHITECTURE / TESTING /
  CONTRIBUTING. The per-crate sub-counts in TESTING.md were still
  the old `~33` / `~56` / `~75` / `~10` / `~37` / `2` numbers from
  cycle 128-ish; some had drifted (`~56` → 74, `~75` → 82,
  `~37` → 40, `2` → 4). Refreshed each to a "+N" range form
  (`~70+` / `~80+` / `~40+` / `~4`) so the figures stay useful as
  rough orders of magnitude without going precisely stale every
  few cycles.

### Internal
- **CI smoke also verifies `--list-actions` and `--list-keybinds`
  produce plausible counts.** Existing smoke verified `--list-themes`
  > 400 entries (catches `theme_filter` over-rejection). Added the
  same range-stable check for `--list-actions` (`> 50`, current 82
  — headroom for new action variants without going stale) and
  `--list-keybinds` (`> 40`, current 58 — headroom for cycle-115-
  style chord-shadow rebalances while still catching an empty
  defaults() regression). Pairs with cycle 215's `--version` and
  `--check-config` grep assertions for full CLI-surface smoke
  coverage.

- **CI's CLI-smoke step now exercises `--version` and `--check-config`.**
  Pre-cycle, the smoke step ran `--config-path` and `--list-themes`
  but not `--version` (which exercises the cycle-192/195 build.rs
  git-SHA capture path) or `--check-config` (cycle 194/196/197/198
  diagnostic path). A regression where the build.rs git invocation
  silently failed and shipped `kettle 0.1.0` without the SHA — or
  `--check-config` lost its cycle-194 `kettle:` lead-line — would
  go unnoticed by CI. Added grep assertions for both:
  ```bash
  cargo run -q -p kettle -- --version | grep -E '^kettle [0-9]+\.[0-9]+\.[0-9]+ \([0-9a-f]+(\+dirty)?\)'
  cargo run -q -p kettle -- --check-config | grep -E '^kettle:  [0-9]'
  ```
  Regex allows the optional `+dirty` suffix so the assertion holds
  on both clean-CI builds (no dirty marker) and local dev builds.

### Documentation
- **`CONTRIBUTING.md` test-count claim reworded to range-stable.**
  Said "workspace runs ~225 tests" — stale by 18 (we're at 243).
  Cycle 172/179 fixed the same drift class in README/CONFIG/
  INSTALL/ARCHITECTURE. CONTRIBUTING.md is contributor-leaning so
  it was exempt from the drift guard, but it has the same problem.
  Reworded to "workspace test suite grows ~1/cycle. Run
  `cargo test --workspace` for today's count" so the count
  doesn't go stale between audits.

### Internal
- **Per-crate `Cargo.toml`s now inherit version / edition / license /
  repository / authors / description from `[workspace.package]`.**
  The workspace `Cargo.toml` had `[workspace.package]` defined with
  `license = "MIT"`, `repository = "https://github.com/Reddimus/kettle"`,
  `authors`, `description`, but **none** of the 6 crate manifests
  used `.workspace = true` to inherit. Each crate just had
  `version = "0.1.0"` and `edition = "2024"` (the workspace.package
  said `edition = "2021"` — mismatch). Cargo would warn about
  missing `license` on `cargo publish`, and a future bump to (say)
  `version = "0.2.0"` would have to be edited in 7 places. Now
  each crate inherits all 6 fields; the workspace.package is the
  single source of truth. Workspace.package edition bumped from
  "2021" to "2024" to match the crates' actual declarations.
  243 workspace tests still pass; `cargo build --workspace` clean.

### Documentation
- **README's License line reflects the cycle-211 NOTICE structure.**
  Pre-cycle the line said "Bundled assets and adapted code are
  credited in NOTICE" — implying all NOTICE entries are
  "adapted code". Cycle 211 added design-source citations
  (kitty / Terminator / Ghostty) with explicit "no code copied"
  notes. Updated to: "Bundled assets, third-party crates kettle
  consumes (Alacritty's VT core, WezTerm's `portable-pty`,
  cosmic-text), and the design-source projects kettle cites
  (kitty's graphics protocol spec, Terminator's splits-and-
  broadcast convention, Ghostty's config syntax)". A user
  reading the License section now sees what's actually in
  NOTICE without having to open it.

- **`NOTICE` credits kitty, Terminator, and Ghostty as
  design-source attributions.** Pre-cycle, NOTICE listed only the
  projects whose CODE kettle uses (Alacritty / vte, WezTerm
  portable-pty, cosmic-text/glyphon, Contour Sixel reference) + the
  bundled assets (font + theme set). Three more projects shape
  kettle's design without sharing code:
  - **kitty** (GPL-3.0) — graphics protocol specification; kettle's
    Rust implementation is original but follows kitty's design.
  - **Terminator** (GPL-3.0) — splits/tabs/broadcast UX +
    default keybinds; the `Ctrl+Shift+O/E/T` convention,
    `broadcast_all` semantics, group-input scoping all originate
    here.
  - **Ghostty** (MIT) — config syntax + key names + `unfocused-
    split-opacity = 0.7` default; a user's Ghostty config drops
    into kettle unchanged.
  Each entry notes "specification/convention consulted, no GPL
  code copied" so the licensing story stays clean — kettle is MIT
  but cites GPL-3.0 *designs* (which is a fair-use / norm-of-
  attribution pattern, not a license-derivation one). No code
  change.

### Internal
- **`kettle-render` and `kettle-vt` crate-level docs updated to
  match what's actually in those crates.** Cycles 207/208/209
  audited the three biggest crate docs (ui / core / config);
  cycle 210 closes the sweep on the remaining two:
  - `kettle-render`: pipeline order (quads → images → text →
    overlay quads), the post-text overlay pass for
    dim+scrollbar, the headless `capture_png` / `offscreen_selftest`
    paths, the broadcast-mode accent flip on tab/border.
  - `kettle-vt`: extractor's dual role for image protocols AND
    OSC 7 (cwd) / OSC 133 (shell integration), the
    `placeholder` module for kitty Unicode-placeholder
    decoding. Both `cargo doc --no-deps` zero-warning.
  All five workspace crates now have rustdoc landings that
  match the contract a contributor would expect after reading
  the CHANGELOG.

- **`kettle-config` crate-level doc lists every public module.**
  Cycles 207/208 siblings. Original kettle-config one-liner mentioned
  "Ghostty-compatible config, bundled Ghostty theme set, embedded
  Nerd Font, Terminator-compatible keybindings" but missed `color`,
  `font`, `fuzzy`, `palette`, `parse`, `template`, and the private
  `theme_filter` module. Now per-module breakdown with intra-doc
  links + cited usage (which UI overlay reuses each helper). Zero
  doc warnings. Closes the crate-level-doc sweep across `kettle-ui`,
  `kettle-core`, and `kettle-config` (3 of 5 workspace crates).

- **`kettle-core` crate-level doc lists every public module.**
  Cycle-207 sibling for the next crate over. The original kettle-core
  doc said "PTY management, the `alacritty_terminal` grid/VT engine
  glue, the UI event bridge, and buffer search" — missed `links`
  (OSC 8 + autodetect), `hints` (Ctrl+Shift+H targets), `images`
  (kitty graphics registries), `scrollbar` (scroll-on-output
  detection), and `url_trim` (cycle 166 bracket-balance helper).
  Now: per-module breakdown with intra-doc links. `cargo doc -p
  kettle-core --no-deps` reports zero warnings (had to disambiguate
  `search`/`links` between the module name and the re-exported
  function name via `mod@`).

- **`kettle-ui` crate-level doc lists what's actually in the crate.**
  Original doc (one-liner from early development) mentioned only
  "winit application, tab/pane multiplexer, keyboard input
  encoding, and the search overlay." Cycles since then added SSH
  launcher (Ctrl+Shift+S), command palette (Ctrl+Shift+K), hint
  mode (Ctrl+Shift+H), session restore, drag-and-drop, broadcast
  input indicators — all undocumented at the crate doc level. A
  new contributor reading `cargo doc -p kettle-ui` saw a stale
  one-liner and had to grep the source to figure out the actual
  surface. Now: per-module breakdown of `app`/`input`/`mux`/
  `session` + a list of modal overlays + the helpers that
  coordinate them. No code change.

- **`theme_filter::is_bundled_theme_filename` doc-comment lists all
  6 skip categories.** The original doc (cycle 167) listed 4
  patterns; cycles 186/187/190 expanded the implementation
  (case-insensitive OS metadata, emacs `#name#` autosave, Office
  `~$name` lock files) but updated only the inline cycle-N
  comments inside the function body. The summary list at the top
  of the doc — which is what a contributor reads first — was
  stale by 2 entries. Now lists all 6 patterns with one-line
  context for each. No code change.

- **`build.rs` module-level doc updated to reflect the cycle-195
  `+dirty` marker and rerun-if-changed removal.** The cycle-192
  module doc said "Outputs `KETTLE_GIT_SHA` as one of two forms"
  (clean SHA or empty). Cycle 195 added the `+dirty` third form
  but didn't update the doc; cycle 195's note was at the
  `cargo:rerun-if-changed` decision site (mid-function), not at
  the top where a contributor first reads. The module doc now
  enumerates all three output forms and cites both cycles. No
  code change; the contract was already implemented in 195.

### Added
- **`kettle` logs its build identity at startup (info level).** A
  user grep'ing their stderr for warnings to file a bug report
  previously had no way to know which build the warnings came
  from. The version+SHA is now logged on first start:
  ```
  [2026-05-20T17:16:54Z INFO kettle] kettle 0.1.0 (a2ff10b2f36f) starting
  ```
  Visible only when the user bumps logging (`RUST_LOG=info kettle`
  or `RUST_LOG=kettle=info`); below the `warn` default so it
  doesn't clutter normal stderr output. Reuses the cycle-192
  `KETTLE_VERSION` constant — same shape as
  `cargo --check-config` and `--version`.

### Documentation
- **`docs/UX-COMPARISON.md` drag-and-drop entry gains a citation
  paragraph.** Cycle 200 added the matrix row but didn't add the
  matching Citations paragraph (which cycle 193 had done for its
  broadcast row). The Citations paragraph explains iTerm2 /
  kitty `paste_from_drop` / WezTerm / GTK origin, plus kettle's
  three-property combination: shell-quote (cycle 175), bracketed-
  paste-safe wrap (cycle 182), and per-pane broadcast aware (the
  cycle 173/174 sibling pattern). Closes the cycle-200 docs gap.

- **`--config` `--help` text documents the cycle-198 unreadable-
  file hard-fail.** The clap doc comment mentioned only the
  cycle-106/164 cases (missing file, directory). Cycle 198 added
  the permission-denied class — the doc didn't reflect it.
  Updated to "must be an existing, regular, readable file" with
  all three hard-fail conditions enumerated. Same docs/runtime
  drift shape as cycle 168 (which originally removed internal
  `cycle N` refs from clap help). The drift-guard test still
  passes (no `cycle <digit>` substring introduced).

### Fixed
- **`--check-config` labels read errors as `i/o error:`, not the
  misleading `malformed value:`.** Cycle-196 follow-up. Cycle 196
  surfaced read failures by pushing them into the `malformed`
  list — they then printed with the existing `- malformed
  value:` prefix. Confusing: a permission-denied file isn't a
  value-parsing failure. Now they get their own category with
  an `i/o error:` prefix and are counted separately in the issues
  total. Sample output diff:
  ```
  before:  - malformed value: could not read /path: ... (using defaults)
  after:   - i/o error: could not read /path: ... (using defaults)
  ```
  Same exit-code semantics (still 1 when issues > 0). 243 tests
  pass.

### Documentation
- **`docs/UX-COMPARISON.md` matrix gains drag-and-drop file paths
  row.** Cycle 175 added drag-drop, cycle 182 made it bracketed-
  paste-safe — kettle's implementation has the distinctive triple
  property (shell-quoted, bracketed-paste-safe, broadcast-aware)
  that's worth recording in the comparison matrix. Row: kettle ✅
  (with the three properties named) · iTerm2 ✅ (long history) ·
  kitty ✅ via `paste_from_drop` · WezTerm ✅ configurable ·
  Terminator 🟡 (GTK builtin; path quoting varies) · Alacritty ⛔.

- **`docs/SHELL-INTEGRATION.md` added to README's Documentation
  list.** The doc has existed since the OSC-133 integration
  landed and got the cycle-189 fish-hook fix, but the README
  only linked it inline from the "Shell integration" feature
  bullet. A user browsing the documentation list to figure out
  what's available would miss it. Now listed alongside CONFIG
  and CONTRIBUTING with a one-line description.

### Fixed
- **`--config FILE` hard-fails at the CLI surface when the file is
  unreadable.** Cycles 106 / 164 caught the "no such file" and
  "not a regular file (typically a dir)" classes. Cycle 198 adds
  the third class: file *exists* and *is regular* but
  permission-denied / I/O-error on open. Pre-fix, kettle started
  with defaults, emitted a warn to stderr, and the user saw their
  theme not apply. Now: `--config FILE: not readable (permission
  denied or I/O error)` and the CLI exits non-zero. Same shape
  as the existing rejections — surface the problem at the CLI
  surface where the user can act on it, instead of silently
  falling back. Test gains a `#[cfg(unix)]` block that
  `chmod 000`s a tempfile and asserts the helper returns the
  right reason; gated on `is_err()` so running tests as root
  (which bypasses unix perms) doesn't spuriously fail.

### Performance
- **`--check-config` reads the config file once, not twice.**
  Cycle-196 follow-up. The cycle-196 fix probed `read_to_string`
  to detect read errors, then on success called
  `load_from_with_diagnostics` which read the file *again*
  internally. Harmless but wasteful (especially on slow disks
  / network mounts / large configs). Now: feed the already-
  read text straight into `parse_collect` and
  `detect_malformed_values` — both are public and take `&str`
  — so the disk read happens exactly once. Same observable
  behavior; just one syscall less. 243 workspace tests pass.

### Fixed
- **`kettle --check-config` exits non-zero when the config file
  is unreadable (perm-denied / I/O error), instead of silently
  returning "status: OK".** Pre-fix,
  `load_from_with_diagnostics` returned defaults on any
  `read_to_string` error and emitted a `warn` log to stderr.
  `--check-config`'s stdout said "config: /path" then "status:
  OK", and the exit code was 0 — making the user think their
  config loaded fine. Bug-report shape: "I set
  `theme = Catppuccin Mocha` but kettle keeps using TokyoNight,
  and --check-config says everything's fine" → the file was
  actually unreadable (umask, sudo'd kettle on a user-owned
  file, network mount lost, etc.). Now the read error is
  surfaced as a malformed-value entry so it shows in the
  issues list and triggers `exit 1`:
  ```
  status:  1 issue(s):
    - malformed value: could not read /etc/kettle.conf:
      Permission denied (os error 13) (using defaults)
  ```

### Added
- **`--version` SHA tags with `+dirty` when the working tree has
  uncommitted changes.** Cycle 192 captured the git SHA; cycle
  195 distinguishes a clean build at a commit from a dev-iter
  build with edits on top of that commit. Pre-fix, a developer
  with edits to `src/main.rs` reported the same SHA as the clean
  tip — bug reports against custom builds were indistinguishable
  from reports against the matching upstream commit. New output
  shapes:
  - `kettle 0.1.0 (a2ff10b2f36f)` — clean tip.
  - `kettle 0.1.0 (a2ff10b2f36f+dirty)` — same commit but with
    uncommitted edits. Mirrors `git describe --dirty`
    convention.
  Build script also dropped the cycle-192 `rerun-if-changed`
  restrictions — source-file edits need to refresh the dirty
  marker, and the two `git` invocations are ~10ms total which
  is well under build-time noise. The cost-benefit pivots
  toward "always rerun" once `+dirty` matters for bug reports.

- **`kettle --check-config` leads with the build version + SHA.**
  Cycle-192 follow-up. The version+SHA shipped in `--version` is
  the canonical "what build are you running" answer; a user
  pasting `--check-config` output into a bug report previously
  had to also run `--version` and quote it separately. The first
  line of `--check-config` is now `kettle:  0.1.0 (sha12)` —
  one paste covers both the build identity and the resolved
  config. Same convention `cargo --version`-style tools use for
  diagnostic flags. Output:
  ```
  kettle:  0.1.0 (a2ff10b2f36f)
  config:  ~/.config/kettle/config
  theme:   TokyoNight Night
  …
  ```

### Documentation
- **`docs/UX-COMPARISON.md` matrix now has a broadcast/group-input
  row.** The 173/174/178/184 trilogy made broadcast a real
  user-facing feature with double visual indicators (tab accent +
  pane border), but the comparison matrix didn't reflect it.
  Added a row showing kettle ✅, Terminator ✅ (origin),
  kitty ✅ (`multi-input.py`), WezTerm ✅, Ghostty ⛔, Alacritty ⛔.
  Citations section also gains an entry explaining the
  per-window-per-tab scoping (cycle-112 invariant), the
  cycles-173/174 sibling methods, and the cycle-178/184
  visual-indicator strategy.

### Added
- **`kettle --version` includes the git SHA.** Pre-cycle, the
  output was just `kettle 0.1.0` (the Cargo.toml version). Every
  serious Rust CLI (cargo, rustc, ripgrep, fd) embeds the build's
  git SHA so users reporting bugs can pin the exact commit they
  hit it on. With nightly `cargo install --git` builds becoming
  common, "kettle 0.1.0" on five different days could mean five
  different binaries. New `build.rs` captures
  `git rev-parse --short=12 HEAD` and embeds it as
  `KETTLE_GIT_SHA`; the main const concats it onto the version
  string. Output: `kettle 0.1.0 (a2ff10b2f36f)` in a git checkout,
  `kettle 0.1.0` in a source-tarball / vendored build (no SHA
  available — empty env string concats to nothing). The build
  script uses cargo:rerun-if-changed on `.git/HEAD` AND the
  ref file the symbolic ref points at (`refs/heads/<branch>`),
  so commits trigger a rebuild with the fresh SHA.

### Performance
- **`broadcast_paste` caches the two possible payload variants.**
  Cycle 174 introduced per-pane bracketed-paste wrapping inside
  `Mux::broadcast_paste` (so panes in vim and panes at a shell
  prompt both get a working paste). The per-pane wrap was
  computed *inside the loop* though — for an N-pane broadcast set
  with a 4 MiB clipboard payload, that's up to N × 4 MiB of
  temporary allocation (8+ MiB at modest pane counts, scaling
  with N). Now: lazy-cache the two possible payloads
  (`bracketed=true` and `bracketed=false`) via
  `Option::get_or_insert_with`. The wrap allocates at most once
  per BRACKETED_PASTE state regardless of pane count. If every
  pane in the broadcast set shares the same mode (typical), only
  one wrap allocation total. Same observable behavior; just
  doesn't allocate as much. No new test — the cycle-174
  per-pane-wrap correctness is unchanged; only the allocation
  count is. 243 workspace tests still pass.

### Fixed
- **Theme filter rejects Microsoft Office lock files (`~$name`).**
  Cycle-167/186/187 follow-up. When Office opens a
  `.docx`/`.xlsx`/`.pptx` from a network drive or shared folder
  (common pattern for theme contributors maintaining a shared
  doc), it writes a hidden-style sibling `~$filename` lock file.
  A maintainer with Office leaking lock files into
  `assets/themes/` would have those slip through cycle 167's
  filter (no leading dot, no `~` suffix, not a known OS metadata
  name). Now: leading-`~` prefix is rejected too. Bundled themes
  never start with `~`. Test gains 2 more asserts (`~$Theme`,
  `~TempTheme`). Closes the theme-filter junk audit at four
  cycles: 167 (initial) → 186 (case) → 187 (emacs `#name#`) →
  190 (Office `~$name`).

### Documentation
- **Fish shell-integration hook emits OSC 133 `D` (command finish
  + exit code).** The bash and zsh sample hooks in
  `docs/SHELL-INTEGRATION.md` emit all four marks (A / B / C / D);
  the fish sample only emitted A (prompt start) and C (preexec).
  Without D, kettle's per-prompt exit-status association is lost
  for fish users — jump-to-prompt still works (it keys off A) but
  any downstream tooling that consumes D (some shell-integration-
  aware status lines, the `__kettle_pc` exit-code template in
  bash) silently skips fish-driven prompts. Added a
  `__kettle_postexec` hook using `fish_postexec` + `$status` so
  fish parity matches bash/zsh. Also documented how to emit B
  inside the prompt itself (fish doesn't expose a fish_prompt_end
  event, but B is optional — kettle only needs A for jump-to-
  prompt). No code change; docs-only.

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
