# Scrollback-search audit - 2026-07-22

This report records the source screencast, independent baseline reproduction,
Terminator comparison, implementation boundaries, and release evidence for the
`Ctrl+Shift+F` search regression. It uses two tracks: **A** reviews every
search-owning code and documentation boundary; **B** reviews every supplied
video frame and every relevant UI/compatibility state. A completed baseline
reproduction is not counted as post-fix validation.

## Scope and machine

The supplied report was investigated on Ubuntu 24.04.4, GNOME 46, Wayland,
kernel 7.0.0-28, Intel Core i7-1165G7, 31 GiB RAM, and Intel Iris Xe graphics.
The installed baseline binary reported Kettle 2.37.0. The live installed Kettle
process was initially left running during source work so its panes and recording
session were not destroyed. A later read-only process check found no live Kettle
instance. Launching or restarting the installed app still requires the user's
approval; absence of a process is not authorization to start one.

Track A covers the strict matching engine and bounded terminal-grid adapter, UI
search state/editor/scheduler, renderer geometry/highlights, config/settings,
control protocol/MCP surface, tests, and user documentation. It does not turn
this search-focused change into an unbounded rewrite of unrelated terminal
features.
Track B covers the user recording, a real X11 baseline, a Terminator comparator,
and the native-platform/live-TUI checks required before making compatibility
claims.

## Source screencast provenance

- Path: `/home/kevim/Videos/Screencasts/Screencast from 2026-07-22 11-16-53.webm`
- SHA-256: `430e8f9ccd112b7cb416456d2118387afbf7718ab34be82542978a3d648b2c93`
- Size: 2,654,151 bytes
- Video: VP8, 1920 x 1080, variable frame rate
- Reported duration: 18.769102 seconds
- Decoded frames: exactly 88; every frame was inspected

The container timestamp starts at 0.126 seconds. The final decoded PTS is
18.895 seconds, so the first-to-last PTS span is the reported 18.769 seconds.
The full frame manifest is retained here to make the "every frame" claim
auditable:

```text
 1  0.126    2  0.238    3  0.272    4  0.305    5  0.339
 6  0.372    7  0.406    8  0.446    9  0.481   10  0.514
11  0.548   12  0.582   13  0.615   14  0.649   15  0.684
16  0.717   17  0.751   18  0.784   19  0.818   20  0.919
21  0.954   22  0.987   23  2.141   24  2.176   25  2.210
26  3.186   27  3.220   28  3.257   29  3.293   30  3.327
31  3.360   32  3.395   33  3.430   34  3.464   35  3.497
36  3.531   37  4.514   38  4.549   39  4.583   40  4.616
41  4.651   42  4.685   43  5.633   44  6.095   45  6.129
46  6.162   47  6.196   48  6.229   49  7.150   50  7.185
51  7.218   52  8.079   53  8.113   54  8.147   55  8.582
56  8.621   57  8.713   58  8.749   59  8.784   60 10.750
61 10.784   62 10.818   63 10.851   64 10.884   65 10.919
66 11.657   67 11.690   68 11.723   69 11.757   70 11.790
71 11.827   72 11.862   73 12.362   74 12.396   75 12.429
76 12.463   77 13.889   78 13.923   79 13.957   80 13.999
81 14.117   82 14.168   83 15.116   84 15.149   85 15.183
86 15.227   87 15.261   88 18.895
```

Search opens on frame 34 at PTS 3.464 seconds. The old bar reports 66 matches,
but matches in historical rows are not painted while navigating scrollback.
The final current-screen match is painted, showing that regex matching itself
can succeed while history-to-viewport projection fails. No inspected frame
shows the missing history highlights appear.

## Independent reproduction and comparator

The issue was reproduced before implementation with the installed baseline
under a real Xvfb X11 server and XTest input. The sequence generated repeated
history matches, opened Search with the real `Ctrl+Shift+F` chord, typed a
query, stepped through matches, scrolled, and stepped backward. Kettle reported
180 matches but did not paint the historical results. PNG evidence is stored in
the ignored directory:

`target/diagnostics/search-x11-baseline-20260722/`

It contains `00-before.png` through `05-prev.png` for before, open, query,
next, scrollback, and previous states. Because `target/` is ignored, these
machine-local captures are evidence artifacts rather than release payloads.

The same Xvfb/XTest method drove Terminator. Its compact search bar selected an
active match near the viewport while stepping and exposed case/backward
controls without obscuring pane rows. Thirteen comparator frames are under:

`target/diagnostics/terminator-search-baseline-20260722/`

The comparator establishes the interaction target; it does not require copying
Terminator's implementation or displaying an expensive global count.

## Root causes

The failure was a set of agreeing assumptions rather than one bad color:

1. UI projection rejected every match with a negative grid line, even though
   retained scrollback deliberately uses negative line coordinates.
2. Highlight projection did not apply the terminal's current display offset,
   so a valid history match did not map to its visible screen row.
3. The legacy helper matched materialized physical rows independently and could
   not represent a regex crossing a soft-wrap boundary.
4. It built and retained a complete match vector just to provide an index/count,
   making work and memory proportional to all retained history before useful UI
   feedback.
5. Result order began at the oldest global match instead of a viewport-relative
   anchor, while invalid regex syntax silently changed meaning through literal
   fallback.
6. Search state lived with the mux instead of the OS window, and cache freshness
   relied on timing state that did not express query, layout, and pane-output
   revisions together.

These explain the recording and the independent baseline: current-screen rows
can work while negative historical rows are filtered or projected incorrectly.

## Track A - implementation inventory and contracts

| Boundary | Owning paths | Contract under review |
|---|---|---|
| Regex/grid matcher | `crates/kettle-core/src/search.rs`, `src/lib.rs` | `regex-automata` meta engine, strict compile result, 4096-byte query and engine-size caps, implicit whole-match capture, signed points/spans, bounded row/cell/byte materialization, exact continuations, accuracy barriers, directions/wrap outcomes |
| Window UI state | `crates/kettle-ui/src/search_input.rs`, `window_state.rs`, `mux.rs` | per-window ownership, per-pane remembered query, grapheme editor, scan-token invalidation |
| Scheduling/input | `crates/kettle-ui/src/app.rs` | immediate/chunk scans, match anchoring, shortcuts/mouse/IME, no PTY forwarding, geometry diagnostics |
| Renderer | `crates/kettle-render/src/lib.rs` | responsive reserved lane, all hit targets, active/inactive signed multi-row highlights, status-only output |
| Settings/config | `crates/kettle-ui/src/settings.rs`, `crates/kettle-config` | persistent Wrap, Smart/Match/Ignore, Invert; Terminator aliases remain compatible |
| Control plane | `crates/kettle-ctl/src/protocol.rs`, `crates/kettle/src/{ctl_cli,mcp_tools,main}.rs` | additive `dispatch_ui_key`, authorization, token/batch bounds, MCP schema; query-free `ui_geometry` |
| Regression boundary | crate tests, MCP stdio test, live interaction harness | portable engine/editor/geometry tests plus native UI/PTY sentinel evidence |
| Documentation | README, config/example, Settings, Agent, Architecture, Testing, man page, comparison/audit/changelog | one consistent user contract and honest platform status |

### Public behavior

- `Ctrl+Shift+F` opens one responsive bottom lane with Editor, Previous, Next,
  Wrap, Case, Invert, and Close. It uses one row on a wide surface and adds as
  many rows as necessary on a narrow surface; controls are never omitted.
- Case labels are **Smart**, **Match**, and **Ignore**. Config values remain
  `smart`, `always`, and `never`; Terminator's boolean spelling remains an
  accepted compatibility alias.
- Patterns are strict Rust regexes and at most 4096 UTF-8 bytes. Invalid, Pattern
  too complex, and Query too long are distinct visible states, not alternate
  matching semantics. Engine ceilings are 512 KiB NFA, 256 KiB one-pass,
  256 KiB hybrid cache, and 40 KiB DFA. `WhichCaptures::Implicit` builds only
  the implicit whole-match capture because search does not consume subgroup
  values.
- Search suppresses zero-width results in one leftmost-first engine pass.
  Consequently, Rust's alternative priority remains visible: a nullable
  alternative that wins with an empty match can shadow a later consuming
  alternative at the same position.
- Enter follows the default direction; Shift+Enter uses its opposite.
  F3/Shift+F3 and explicit Next/Previous retain literal directions. Escape
  closes while keeping the selected result at its screen anchor.
- Editor selection, movement, deletion, word selection, and horizontal scroll
  follow Unicode grapheme boundaries. Inserted control characters are dropped;
  pasted tabs/newlines normalize to spaces.
- Search belongs to one OS window and targets the pane on which it opened. The
  last query is remembered per pane in that window only; it is not persisted to
  disk or exposed through diagnostics.

### Complexity and cancellation

| Work | Bound / complexity |
|---|---|
| Compile | query <=4096 UTF-8 bytes; <=512 KiB NFA, <=256 KiB one-pass state, <=256 KiB hybrid cache, <=40 KiB DFA; compile before taking the terminal lock |
| Typing feedback | target range <=1000 physical lines from a viewport-relative anchor; exact core yield may pause earlier |
| Idle retry | starts after 500 ms; nominal <=1000-line range, one bounded core work slice per event-loop turn |
| One engine invocation | <=64 KiB UTF-8; an invocation is not interrupted mid-call |
| One bounded core call | <=64 KiB aggregate UTF-8, <=262,144 inspected cells, <=256 complete logical-line haystacks |
| Exact work yield | continuation is the first unscanned hard logical line; never splits a complete line and is resumed without Results limited |
| One logical haystack | <=256 physical rows, <=64 KiB UTF-8, <=262,144 inspected cells including spacer/context inspection |
| Capacity barrier | stop at the first uninspected part of that logical line; no continuation beyond it; UI reports **Results limited** |
| Continuous output | preserve chunk cursor for progress; only non-navigation work gets fresh-anchor verification after 500 ms quiet |
| Explicit step | starts resumable traversal immediately in the requested direction, honoring wrap |
| Output-interrupted explicit step | **Results limited** until the user retries; an automatic default-direction retry would verify a different operation |
| Visible highlights | viewport +/-100 physical lines, with bounded soft-wrap context |
| Retained spans | <=65,536 per projection; projection `O(visible cells + visible spans)` |
| Definitive result | unavailable across an uncertain materialization/projection boundary; UI reports **Results limited** |
| Stale work | query/reflow restarts by scan token; output-only drift advances the token under the policies above |

The bar deliberately has no global match count. Computing an exact count would
force complete-history work on every edit or require a complex eventually
consistent count whose number could disagree with new output. Status and the
active result provide actionable feedback without that cost.

### Local engine-budget performance probe

This is measured, non-gating local evidence, not a CI benchmark or a native-UI
claim. The probe ran on the audit machine's Intel Core i7-1165G7 with Rust
1.96.0, optimized `rustc -O`, `regex-automata` 0.4.14, and Linux `taskset`
pinning the process to CPU 3. Its source and binary are intentionally ignored
diagnostics at `target/diagnostics/regex_limits.rs` and
`target/diagnostics/regex_limits`.

The exact build/run recipe used the checkout's already-built target rlib:

```sh
rustc -O --edition=2024 target/diagnostics/regex_limits.rs \
  -L dependency=target/debug/deps \
  --extern regex_automata=target/debug/deps/libregex_automata-c458cab110e7d576.rlib \
  -o target/diagnostics/regex_limits
taskset -c 3 target/diagnostics/regex_limits extra
taskset -c 3 target/diagnostics/regex_limits engines
taskset -c 3 target/diagnostics/regex_limits cachefamilies
```

For the worst accepted adversarial family below the 512 KiB NFA ceiling,
`(?:\w?){8}\P{Letter}\b`, three-sample median no-match time scaled as follows:

| UTF-8 haystack | Median |
|---:|---:|
| 64 KiB | 17.8 ms |
| 128 KiB | 35.8 ms |
| 256 KiB | 71.3 ms |
| 512 KiB | 143.6 ms |
| 1 MiB | 288.5 ms |

Production Kettle invokes the engine with at most 64 KiB, so only the first row
is an in-product single-call ceiling measurement; the larger rows are
diagnostic scaling evidence. `(?:\w?){10}\P{Letter}\b` needs 543,244 NFA bytes
and is rejected as Pattern too complex. The intentionally extreme N=200 family
needs 10,050,980 NFA bytes; under the prior/default unbounded configuration its
256 KiB no-match probe took 1.56 s and the compiled regex retained about
11.3 MiB of static memory. That contrast is why engine construction and each
synchronous haystack are bounded independently.

### Security and privacy boundaries

Search input is application chrome. Real keyboard, mouse, IME, and
`dispatch_ui_key` events terminate in the editor/navigation state and never use
terminal key encoding or PTY writes. `dispatch_ui_key` requires full agent mode,
accepts 1-64 tokens of 1-64 bytes, and validates the entire batch before the
first state change. A closed/unsupported modal is an error.

`ui_geometry.search` reports target pane, rectangles, reserved rows, focused
control, status, match/truncation booleans, and Wrap/Case/Invert values. That
object does not report the search query or matched terminal text. Query memory
remains in-process and per window.

## Track B - UI, accessibility, and compatibility states

The integrated release check must exercise at least:

- Type to search, Searching, Match, Wrapped, Start reached, End reached, No
  match, Invalid pattern, Pattern too complex, Query too long, and Results
  limited states;
- positive active-screen rows, negative history rows, soft wraps, one-cell and
  multi-row regexes, wide characters, combining marks, emoji ZWJ sequences,
  zero-width suppression, nullable-alternative priority, exact operation-budget
  continuations, and pathological soft-wrapped lines at every materialization
  bound;
- forward/reverse, wrap on/off, invert, Enter/Shift+Enter, F3/Shift+F3,
  keyboard control cycling, mouse hit targets, selection/copy/cut/paste,
  double/triple click, IME position, narrow/tall windows, top/bottom status bar,
  update banner, tabs/splits, resize, and close anchoring;
- screen-reader names/focus/actions for Editor, Previous, Next, Wrap, Case,
  Invert, Close, and status, with no inaccessible color-only state;
- a PTY sentinel while tmux, clean Neovim, configured AstroNvim, Codex CLI, and
  Claude Code CLI own the pane, proving modal keys do not leak to the program.

### Evidence status at implementation-documentation time

| Check | Status |
|---|---|
| All 88 supplied frames decoded and inspected | complete |
| Installed-baseline Xvfb/XTest reproduction | complete; failure reproduced |
| Terminator Xvfb/XTest comparator | complete |
| Core search verification | complete locally: `cargo test -p kettle-core search::tests` (27/27), `cargo test -p kettle-core --lib` (179/179), and warnings-denied all-target clippy |
| UI verification | complete locally: `cargo test -p kettle-ui --lib` (320/320) and warnings-denied all-target clippy |
| Renderer status verification | complete locally: bounded-status vocabulary test (1/1) and warnings-denied all-target clippy |
| Format/worktree whitespace | complete locally: `cargo fmt --all --check` and `git diff --check` |
| Engine-budget performance probe | complete locally; non-gating i7-1165G7/CPU-3 evidence and ignored recipe above, not CI |
| Settled post-fix Xvfb history E2E | complete after engine/work-yield hardening: `target/diagnostics/search-history-e2e-settled/search-history-20260722-164503/`; statuses Wrapped/Match/Match/Match, forward offsets 1710/735/135, reverse 735 |
| Cross-platform live-UI helper self-test | complete locally: `just live-ui-helper-selftest` |
| Windows compile cross-check from Ubuntu | complete locally: `cargo check -p kettle-ui --target x86_64-pc-windows-gnu`; this is not a native Win11/ConPTY or MSVC UI check |
| Ubuntu Wayland live renderer, input, IME, accessibility, Super-key installed launch | pending |
| Full workspace format/lint/build/test/docs gates | complete locally: `just gauntlet` through the successful `just gauntlet-strict` run |
| Strict dependency/supply-chain gate | complete locally: `just gauntlet-strict`; advisories/bans/licenses/sources passed, `cargo machete` found no unused dependency, tracked audit reported 805/805 files with 0 errors and 0 warnings |
| GitHub Linux/macOS/Windows native CI | pending |
| Native Windows 11/ConPTY live UI | pending; must not be inferred from Linux |
| Windows 11 WSL + tmux/AstroNvim/Codex/Claude flow | pending |
| macOS native keyboard/IME/accessibility/Metal UI | pending |
| Signed release, updater install, and user-approved installed-app launch/restart | pending; no live Kettle was present at the latest read-only check |

Focused commands recorded as passed at this checkpoint:

```sh
cargo test -p kettle-core search::tests                 # 27/27
cargo test -p kettle-core --lib                         # 179/179
cargo clippy -p kettle-core --all-targets -- -D warnings
cargo test -p kettle-ui --lib                           # 320/320
cargo clippy -p kettle-ui --all-targets -- -D warnings
cargo test -p kettle-render status_vocabulary_is_bounded_and_semantic --lib  # 1/1
cargo clippy -p kettle-render --all-targets -- -D warnings
cargo test -p kettle-vt --lib                          # 92/92
cargo check -p kettle-ui --target x86_64-pc-windows-gnu
cargo fmt --all --check
git diff --check
just gauntlet-strict
```

Update this table only with the exact command/environment and artifact path for
checks actually run. Missing external CLIs or credentials are skips, not
passes. The broader structural and platform follow-ups remain in
[AUDIT-DEFERRED.md](AUDIT-DEFERRED.md).

## Release evidence required

Before closing the regression, record:

1. focused `kettle-core`, `kettle-render`, `kettle-ui`, `kettle-ctl`, and
   `kettle` tests plus warnings-denied clippy;
2. `cargo fmt --all --check`, the repository gauntlet, and the strict release
   gauntlet;
3. a post-fix live search artifact set driven through `perform_action`,
   `dispatch_ui_key`, `ui_geometry`, `read_cells`, and full-window screenshots;
4. native CI results without converting unrun live GPU/UI tests into CI claims;
5. signed release/update verification, installed binary/config/desktop entry
   checks, and user approval before launching or restarting the installed app.

This division keeps the result reviewable: source contracts and bounded
algorithms can be proven portably, while compositor, GPU, IME, accessibility,
desktop launcher, and real TUI behavior remain native evidence obligations.
