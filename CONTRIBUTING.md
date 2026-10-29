# Contributing to kettle

Keep each change focused and test the behavior it changes. For build tools and
platform dependencies, see [Installation](docs/INSTALL.md#from-source). For test
commands, native checks and coverage limits, see [Testing](docs/TESTING.md).

Participation follows the [Code of Conduct](CODE_OF_CONDUCT.md). Report security
issues privately as described in [SECURITY.md](SECURITY.md).

## Workflow

1. Read the owning crate, nearby tests and applicable `AGENTS.md` instructions.
   Pick a bounded bug or feature. [ROADMAP.md](docs/ROADMAP.md) lists priorities;
   [AUDIT-DEFERRED.md](docs/AUDIT-DEFERRED.md) records unresolved audit work.
2. Keep logic in the crate that owns it. Reuse existing helpers. Extract pure
   decisions when that makes behavior easier to test, and keep I/O at the edges.
   Preserve input caps, permissions and explicit failure behavior at terminal,
   IPC, persistence, update and installer boundaries.
3. Add a regression that fails without the fix. Unit tests usually live beside
   their code. Put black-box, subprocess or cross-crate tests in the owning
   crate's `tests/` directory. Use behavioral tests when possible; use source
   guards only when they protect a contract behavior tests cannot express.
4. Update user-facing docs and add a short `CHANGELOG.md` entry under
   `Unreleased` when behavior changes. State the user's symptom and resulting
   behavior. Keep unfinished work in the roadmap or deferred-audit list.
5. Format only the changed crates while iterating, then run the gate:

   ```sh
   cargo fmt -p <crate>
   cargo fmt --all --check
   just gauntlet
   ```

   Release and supply-chain changes also need `just gauntlet-strict`. See
   [Testing](docs/TESTING.md#what-strict-and-full-mean) for required tools and full
   native gates. Record skipped checks and their reasons. CI includes an
   MSRV (Rust 1.95) check; a portable or offscreen pass does not establish native
   platform or live-UI behavior.
6. Commit the focused change, open a PR and wait for required CI and review.
   Follow the [review policy](.github/review-policy.md). Keep the tree green
   before each commit; preserve unrelated work in a dirty checkout.

Use ASCII unless the file already needs Unicode. Keep comments about non-obvious
invariants or platform behavior. Cite the specification or source when behavior
depends on another implementation. New UI text belongs in `kettle-i18n`, in
English and Spanish together; see [Translating](TRANSLATING.md).

## Commits and pull requests

Greptile reviews PRs and updates using [the repository policy](.github/review-policy.md)
and [repository settings](.greptile/config.json). Reviews are advisory; required
build and MSRV checks still gate merges. The optional hosted Claude review needs
the repository variable `CLAUDE_REVIEW_ENABLED=true` and available service quota.

Use `<crate>: <imperative summary>` subjects, for example:

```text
kettle-ui: Keep pinned previews open while searching
```

For changes that need explanation, describe the bug, fix and test rationale in
the commit body. Keep PRs focused enough to review as one change. Explain the
trigger, resulting behavior and validation; name any remaining limitation.

Before requesting review, check that:

- the change stays within its intended scope and reuses the owning crate's APIs;
- regression coverage fails against the old behavior;
- help, README, docs, installer and desktop/man-page assets agree where affected;
- user-visible changes have an `Unreleased` changelog entry;
- required local checks pass, and native checks or skips are recorded;
- the PR explains the user impact and any security or compatibility tradeoff.

## Project layout

| Location | Responsibility |
|---|---|
| `crates/kettle` | CLI, exec, ctl and MCP entry points |
| `crates/kettle-ui` | Window lifecycle, input, tabs/splits, settings and media presentation |
| `crates/kettle-core` | PTY lifecycle, terminal grid, search, hints and links |
| `crates/kettle-vt` | Image protocols and control-string extraction |
| `crates/kettle-render` | GPU pipelines, text, chrome geometry and screenshots |
| `crates/kettle-config` | Config parsing, themes, keybinds, SSH hosts and fuzzy matching |
| `crates/kettle-i18n` | Typed UI text catalogue |
| `crates/kettle-state` | Atomic persistence, trusted paths and file locks |
| `crates/kettle-update` | Signed feeds, bounded archives and update transactions |
| `crates/kettle-remote` | Process inspection and SSH/container detection |
| `crates/kettle-ctl` | Local IPC, control protocol and discovery |
| `crates/kettle-media` | Media jobs, source authorization, caps and worker protocol |
| `crates/kettle-media-worker` | Worker executable, containment, playback and limits |
| `crates/kettle-media-render` | Raster/SVG/diagram decoding in the worker |
| `crates/kettle-media-native` | Native video and trusted FFmpeg/ffprobe runs |
| `scripts/`, `packaging/`, `.github/` | Local tools, distribution assets and CI |

[Architecture](docs/ARCHITECTURE.md) describes crate boundaries and invariants.
Record architecture changes there and verification changes in
[Testing](docs/TESTING.md).

## Local Git helpers

Enable the optional pre-commit gate with:

```sh
git config core.hooksPath .githooks
```

The hook documents which paths trigger the gauntlet; it skips documentation-only
commits. Record bulk changes with no semantic effect in `.git-blame-ignore-revs`.
To use that list locally:

```sh
git config blame.ignoreRevsFile .git-blame-ignore-revs
```

## App icons

`scripts/gen-icons.py` owns icon geometry. It needs Pillow and regenerates SVGs,
Linux PNGs, the macOS Icon Composer source and compatibility iconset, and the
retained Windows icon:

```sh
python3 scripts/gen-icons.py
file packaging/linux/kettle-*.png
```

Commit source and generated assets together. PNGs must be 8-bit/color RGBA;
GNOME Shell rejects 16-bit icons. See [macOS packaging](packaging/macos/README.md)
for the Xcode asset pipeline and [Testing](docs/TESTING.md#supply-chain-fixture-gates)
for icon validation.

## Releasing

Follow [RELEASING.md](docs/RELEASING.md). It owns the two-PR procedure, strict
validation, signed release commit/tag, packaging and native release gates.
Use `scripts/release.sh` and `scripts/tag-release.sh`; do not replace the
procedure with manual version edits or tagging.
