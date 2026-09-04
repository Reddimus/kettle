# Full-repo audit — 2026-08-07

Five independent adversarial audits at maximum reasoning effort covered every
Rust crate in the workspace, grouped so each auditor held one coherent subsystem:

| audit | crates | lines |
|---|---|---|
| vt-core | `kettle-vt`, `kettle-core` | ~27k |
| render | `kettle-render` | ~20k |
| ui | `kettle-ui` | ~54k |
| ipc | `kettle-ctl`, `kettle-state`, `kettle-remote` | ~17k |
| cli | `kettle`, `kettle-config`, `kettle-update` | ~38k |

Together they returned **29 findings: 5 CRITICAL, 15 HIGH, 9 MEDIUM.** Every one
was required to carry a `path:line` anchor, an exact reproducing trigger, the
concrete wrong outcome, and a record of what the auditor read while trying to
*disprove* its own finding. Findings that could not survive that self-refutation
were dropped before reaching this list.

**All 29 are fixed.** Each fix carries a regression test that fails without it.

This pass deliberately hunted one pattern above all others, because
`AUDIT-BACKLOG-2026-08.md` records it as this repository's recurring failure
mode: *"a fix that closes the case you tested and leaves the neighbouring one
open."* That prediction held. Nine of the 29 findings are the untouched sibling
of a defect an earlier pass already fixed — a second parser state, a second
platform branch, a second paste ingress, a second placement path.

## The pattern, stated plainly

Three separate streaming ANSI parsers exist in this codebase: the `kettle exec`
stripper, the session-log scrubber, and the VT extractor. They have now drifted
from one another **three separate times**, and this audit found holes in all
three simultaneously — each in a *different* state, each previously fixed in one
parser and not the others.

The correct long-term fix is one shared streaming transition kernel. It was
deliberately **not** attempted in this pass, because two of the parsers were
being repaired concurrently and a unifying refactor would have collided with
that work. The required scope is recorded in `AUDIT-DEFERRED.md` so it can be
done as its own change with its own measurement.

---

## CRITICAL

| Area | Defect |
|---|---|
| `kettle-update/install.rs` | A `SIGKILL`ed Linux update became **unreachable to its own recovery code**. `detect_managed_install` verified content provenance *before* any path could recover a transaction, so a crash between publishing a file and writing the provenance record made every later startup and `kettle update` report the installation unmanaged. The journal and last-known-good backups survived intact, but no production path could consume them — the install was permanently stranded until manual repair. Structural discovery is now split from provenance verification; startup, the update CLI, and direct library installation take the update lock and recover first. |
| `kettle-update/install.rs` | Backups became visible **before** their journal entries. Recovery found an unjournaled backup file, correctly classified it as foreign evidence, and refused to roll back — so every retry hit the same wall and a partially applied install could not self-recover. A durable `BackingUp` intent now records destination, transaction, and prior hash/size/mode before the backup name is exposed. |
| `kettle/main.rs` | `--check-config` **bypassed the hardened config reader** entirely, using unbounded blocking `read_to_string`. A FIFO at the resolved default path blocked forever; a large regular file ignored the documented 1 MiB cap; and UTF-16 configs the runtime accepts were reported unreadable. Startup, reload, and `--check-config` now share one hardened read/decode/parse API. |
| `kettle/exec.rs` | The exec ANSI stripper **ignored CAN/SUB in every state but one**. `printf '\033[31\030hello'` printed `ello`, and a CAN inside a control string swallowed the rest of the stream. MCP `kettle_run` strips by default, so this was the output agents read — CLI and MCP captures disagreed with what the terminal displayed. |
| `kettle-ui/mux.rs` | Priority replies could **splice control sequences into an in-flight paste**. User messages write in 8 KiB chunks and the worker loaded a queued reply at every chunk boundary, so an SGR mouse report could land between the bracketed-paste start and end markers and be interpreted as pasted content. Sustained replies could also starve a pending paste indefinitely. PTY writes are now message-atomic. |

## HIGH

**Renderer / compositing** — six findings, several macOS-specific:

- Metal advertises `Opaque` and `PostMultiplied`; Kettle selected `PostMultiplied`
  while every shader emitted **premultiplied** color and every blend treated the
  destination as premultiplied. The attachment was neither representation, and
  the compositor multiplied it again — darkening or contaminating every
  translucent edge. Scene rendering stays premultiplied; a final fullscreen pass
  unpremultiplies into the surface.
- Alpha-mode selection read `background-opacity` alone while the clear used
  `opacity * darkness`, so a transparent config at opacity 1.0 chose `Opaque` and
  discarded the alpha. The mode was never recomputed, so lowering opacity in
  Settings did nothing until restart.
- An opaque wallpaper drew *after* the darkened clear, and default cells emit no
  quad — so `background-darkness` had no visible effect in the focused pane at
  either endpoint, contradicting its documented guarantee.
- Per-pane OSC 11 backdrops blended over a surface already cleared to the
  focused pane's color, compounding alpha to 0.75 and leaking color between
  panes. Merely changing focus changed a secondary pane's opacity and tint.
- `minimum-contrast` measured against the *original* cell background, then
  painted an opaque selection over it — white-on-white satisfied a 4.5
  requirement at an actual 1:1. Active-search foreground bypassed the guarantee
  entirely.
- Atlas-full misses were cached as ordinary whitespace, and LRU eviction removed
  map entries without reclaiming atlas pixels. Once the atlas filled, affected
  glyphs were **permanently blank** until a font-setting change or restart.

**Multiplexer / broadcast** — four findings:

- Broadcast fanned out bytes *after* encoding them once for the focused pane's
  keyboard mode, so a legacy pane sharing a group with a Kitty pane received
  `ESC [ 97 u` instead of `a`. Key *releases* never crossed windows at all,
  leaving remote TUIs holding a logically stuck key.
- Group autoclean asked a single window whether a group was empty and disabled
  broadcast while another window still held members; it never ran on ungroup.
- Dropped files reached only the source window, while clipboard paste under the
  identical group scope reached everyone.
- File paths were formatted once for the *source* pane's shell, so a WSL pane
  received an unusable Windows path (and vice versa).

**VT parsing / logging** — four findings:

- Bounded recovery could split a UTF-8 scalar: when the last quarantined byte was
  a lead such as `E2`, recovery forwarded the orphaned `82 AC` alone — Kettle
  turning valid child output into invalid UTF-8.
- A non-ST `ESC` did not abort an intercepted DCS/APC string, so
  `ESC P q ESC c VISIBLE` withheld everything and the pane appeared frozen until
  a terminator arrived or ~16 MiB of budget burned.
- The session-log scrubber diverged from the terminal in three sibling states.
  One consequence has a privacy edge: with no ESC-from-CSI transition, an OSC
  payload — potentially a title containing sensitive text — was written into the
  supposedly plain-text log.
- ANSI-strip state leaked *across* logging sessions: stopping and restarting
  logging retained the old OSC state, silently dropping the new log's opening
  output.

**Remote** — one finding:

- Kettle **reversed OpenSSH's option precedence**. Real `ssh` keeps the first
  obtained value, so `ssh -l bob alice@h` connects as `bob` — but Kettle's
  displayed context and Reconnect action used `alice`, authenticating as a
  different user. Verified against `ssh -G` on OpenSSH 10.3; the pre-existing
  tests asserted the wrong behavior.

## MEDIUM

- `paste-images = of` (a typo) silently enabled disk materialization, because
  unknown tokens map to `On` with no diagnostic — misleading for a setting
  documented as a privacy control.
- Four keys documented as permanent no-ops were absent from inert-key reporting.
- Cross-window echo bypass stamped only the source window, so grouped panes
  echoed visibly out of phase.
- Chrome text damage omitted colors glyphon bakes at `prepare` time, so a hovered
  close glyph or a refocused pane title kept its old color.
- Tile wallpaper mode rebuilt ~4,050 quads *per frame* on a 4K surface —
  ~243,000 tile instances per second for a static image — and compressed its
  final edge tile. Now one fullscreen repeat-sampled instance.
- CPU background blur averaged straight sRGB and alpha independently, producing
  dark halos around transparent edges. Its test compared against a brute-force
  oracle that made the *same* mistake, so it pinned the defect instead of
  catching it.
- Standalone Kitty virtual placements for unknown image ids consumed the shared
  placement budget; 256 bogus ids starved a legitimate placement.
- The Linux process scanner published argv-truncated snapshots as complete,
  dropping the remote context of a live SSH session.
- Activation's long-path fallback was never length-checked, so a long `TMPDIR`
  silently opened a second process instead of reusing the primary.

---

## Final pre-release review — two more, both blocking

A last read of the whole integrated diff turned up two defects that every
earlier pass had missed. Both were reproduced before they earned a fix.

**`shell-integration/kettle.ps1` reported success for failed commands.** The
wrapper invoked the user's prompt first — correct, and deliberate, because `$?`
must reach starship/oh-my-posh unperturbed — and only then read
`$LASTEXITCODE`. But those prompts shell out while rendering, and every native
call overwrites it. A command failing with 37 followed by a prompt that ran
anything successfully emitted `D;0`, so command notifications, `command_finished`
events, and ctl/MCP `run_command` all reported success for a failed command.

`$?` and `$LASTEXITCODE` pull in opposite directions here: `$?` reflects only the
immediately preceding statement and is read-only, so it cannot be saved and
restored by assignment. Both are now captured in a single array literal, which
evaluates `$?` before the assignment resets it, and the failure indicator is
re-armed — by deliberately failing a statement — as the last thing before the
user's prompt runs. `$LASTEXITCODE` is restored afterwards so the prompt's own
native calls do not leak into the next command's view of it.

The fixture asserted `$?` and marker ordering but **never read the `D` payload**,
which is exactly why this shipped: a wrapper hardcoded to `D;0` passed every
check it had. The payload case is now covered, and was confirmed to fail against
the pre-fix snippet before being accepted.

**88 source-level drift guards could not fail.** A guard of the shape

    let src = include_str!("lib.rs");
    assert!(src.contains("self.evict_lru("), "...");

searches its own test module, so the needle written inside the assertion is
always present. The assertion is unconditionally true. This was proved rather
than argued: both defects named by the `glyphpipe.rs` guards were reintroduced
in production code — capacity failure cached as a permanent blank slot, and
refusal instead of eviction at the cap — and both tests still passed.

A scan found 88 such assertions across four files (80 in `kettle-render/src/lib.rs`
alone). The repair reuses the `production_source()` helper this repository had
already written for `mux.rs` and `app.rs`, which slices the test module off and
asserts that the slice worked. `lib.rs` needed a line-scanning variant because it
carries 28 interleaved test modules rather than one trailing block.

Slicing exposed **seven stale expectations in five guards**, every one of which
had been masking real drift invisibly:

| guard | what the needle still claimed | what production had migrated to |
|---|---|---|
| `idle_repaint_skips_glyphon_prepare…` | `let need_prepare = any_pane_text_changed` | `self.text_prepare_dirty` (retry latch); gate intact |
| `tab_text_uses_full_title_lane_budget` | `s.rect` | `s.title_rect` — the title lane, excluding close-button chrome |
| `image_placement_draw_keeps_len_fastpath…` | `pv.images.len() > 1` | `quota > 1` — fair per-class placement quotas |
| `cell_lock_emit_is_a_single_shared_fn` | def + 3 calls = 4 | 3 — the fourth call site was a test fixture |
| `no_call_site_uses_basic_shaping` | exactly 2 `Shaping::Basic` | 0 — which is the state the guard actually wants |

None required a production change; all five were stale text, and each was checked
against current source rather than relaxed until it matched. The first guard's
name was corrected too — it asserted the title lane while calling itself
`…full_segment_rect_budget`, a quieter instance of the same disease.

This is now the **fifth and sixth** time this repository has been caught with a
test that names a contract it does not enforce. The first three were found
earlier in this same audit. The class is worth stating as a standing rule: a
source-text guard must be proved to fail before it is trusted, and
`include_str!` on one's own file is a defect until the test module is sliced off.

### Three more in the fixes themselves, and a seventh guard

A review of the two fixes above found three defects **in them**, which is the
whole argument for not stopping the loop at the first clean-looking pass:

- The exit code was still wrong for cmdlets. `$LASTEXITCODE` is written only by
  *native* commands, so reporting it verbatim mislabelled both directions: a
  successful cmdlet after an earlier `exit 37` reported failure, and a failed
  cmdlet reported success. `$?` now decides, and the numeric code is consulted
  only once `$?` says the command failed.
- `$code` was a plainly named local hoisted above the prompt invocation.
  PowerShell resolves variables dynamically through the call stack, so it
  shadowed any `$code` the user's own prompt reads.
- The `$?` re-arm used `-ErrorAction SilentlyContinue` and removed the resulting
  `$Error` record afterwards. "Afterwards" is too late: while the prompt renders
  — exactly when posh-git inspects `$Error[0]` — kettle's synthetic error was
  the visible one. `-ErrorAction Ignore` sets `$?` and records nothing, so there
  is no window and no cleanup.

The same review found a **seventh** unfailable guard, in
`main.rs`'s `windows_gui_subsystem_with_conditional_attach_survives`. The first
scan had missed it because it only recognised `src.contains("literal")`; that
guard writes its needles into an array and passes them as a variable. The six
remaining files holding a raw own-file `include_str!` were then converted
wholesale — 15 reads — rather than patching the one instance.

`exec.rs` deserved the most care: its guard is *negative*
(`!scan.contains("kettle_ui::")`), and a negative guard fails **open** if the
slice loses production code. So the stripper was measured rather than trusted:
all 38 column-0 production functions survive, and a probe inserted at line 400
and again at line 2000 — real production sandwiched between the test modules at
672 and 2498 — trips the guard, while one at line 3000 inside a test module
correctly does not.

**Cross-platform verification.** The PowerShell snippet claims 5.1+ support that
had never once been executed. All four payload cases now run green on Windows
PowerShell **5.1.26100.8875 (Desktop)** on the Surface Book 3 as well as pwsh
7.6.4 on macOS, under `$ErrorActionPreference = 'Stop'`. The fixture
discriminates all three generations of the snippet: it passes on the shipped
one, fails the original on `D;37`, and fails the intermediate one on the
synthetic `$Error` record.

---

## Verification

Beyond each finding's regression test and a green `just gauntlet` on the
integrated result, the externally observable fixes were confirmed
**end-to-end against a built binary** — reproducing the defect on the pre-fix
build, then confirming correct behavior on the post-fix build, with adjacent
behavior checked for regression:

| case | before | after |
|---|---|---|
| CAN in CSI | `ello` | `hello` |
| CAN in OSC | *(swallowed)* | `VISIBLE` |
| ESC in DCS | *(swallowed)* | `VISIBLE` |
| SUB in CSI *(sibling)* | — | `world` |
| normal SGR *(regression check)* | — | `red plain` |
| UTF-8 *(regression check)* | — | `café ▐ €` |
| FIFO at default config path | blocked forever | returns promptly |
| 1.8 MB config | unbounded read | `is 1800000 bytes (cap 1048576)`, exit 1 |
| UTF-16LE config | reported unreadable | decoded, `font-size = 14` applied |
| `paste-images = of` | silently accepted | `malformed value` |

OpenSSH precedence was independently confirmed against `ssh -G` before accepting
the fix's direction.

**Not run on this host:** native Linux and Windows legs, live-GPU compositor
inspection, and the Linux-native procfs scanner test. Those remain for CI and the
cross-machine validation legs, and are recorded here rather than implied.
