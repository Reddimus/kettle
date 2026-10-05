# Agent visuals design

This record describes the 5.0 agent-visuals architecture and the boundaries
that each implementation slice must preserve. It is a design record, not a
claim that all these features ship. The current implementation has bounded
media frames, worker discovery/build identity, a separate resource-limited
worker, raster/SVG rendering, paired packaging/update recovery, and internal
availability. Cards, preview lanes, Mermaid, and audio/video playback remain
later work.
The existing poster worker also uses the shared content-based video container
classifier; this does not add a decoder or playback.

## Ownership

| Component | Responsibility |
| --- | --- |
| `kettle-media` | Bounded jobs/results and wire frames; source authorization; build handshake; shared video container identification; injected platform client for worker availability, deadlines and footprint monitoring |
| `kettle-media-worker` | Separate O3 executable; early descriptor sweep and resource setup; one job/reply per process; watchdog and input-free failure reporting |
| `kettle-media-render` | Held-descriptor source/font reads; content-based raster selection; SVG sanitizing and admission; rendering into straight RGBA |
| `kettle` | Installed-worker resolution and native verification; CLI/MCP and launch integration as they land |
| `kettle-ui` | User actions, request admission, lifetime/cancellation, fallback-font resolution and bounded requeue, preview/card/shelf/viewer state as they land |
| `kettle-core`, `kettle-vt`, `kettle-render` | Terminal state, registered card markers, geometry and GPU presentation as they land |
| `kettle-update` | Verified packages, installation provenance, publication/recovery of the executable pair |

The heavy renderer dependency belongs only to the worker. Ordinary terminal
startup does not load worker fonts, start a worker, or create a footprint
monitor. Existing image protocols, paste receipts and the older poster worker
retain their own paths until an explicit migration is implemented and tested.

## Request and process boundary

The parent resolves the worker only from its installation, never from PATH,
cwd or an environment override. File and platform identity checks precede
trust. Hello/Ready then require matching version, source hash and protocol.
The source hash is build equality, not a package signature. Installation trust
and artifact verification remain separate requirements.

Each worker serves one job and exits. Startup has a 5 s deadline and at most
one startup-only retry. A ready worker gets no retry: raster work has 2 s;
other job kinds have 3 s. The parent measures the worker's process group and
fails work above 768 MiB, killing the recorded group before bounded reaping.
Successful pixels are accepted only with a valid reply and clean worker exit.
Unmeasurable or abandoned processes fail closed. Sampling can miss a short
peak; it is protection, not an exact heap accounting proof.

Setup closes inherited descriptors, disables core dumps, lowers limits and
starts a watchdog before Ready. This currently provides resource isolation,
not a complete operating-system sandbox. Decoder-helper confinement and native
sandbox acceptance are required before later video/playback features ship.
Windows media remains unsupported; the text terminal continues to work.

The packaged raster/SVG worker is available internally after its installation
checks pass. User-facing preview callers are introduced separately. A renderer
test does not prove native release-artifact installation or UI acceptance.

## Source, fonts and rendering

External source paths carry device/inode attestations. A GUI user pull needs
an explicit action witness. Inline sources and paths are bounded before work;
path reads use a single non-blocking, read-only regular-file descriptor and
compare identity before/after reading. A rename over the path cannot redirect
the held descriptor. This is not an immutable filesystem snapshot: a
same-size rewrite restoring modification time can evade those checks.

Raster format comes from content, not extension. Decode dimensions and bytes
are admitted before pixel decoding; scaling uses premultiplied alpha and
returns straight RGBA with transparent pixels cleared. SVG is rewritten with
no DTD, scripts, foreign content or external references. CSS is resolved into
checked attributes before usvg. Expanded structure and resvg surfaces are
admitted before rasterization. Image resolvers load nothing.

SVG text uses the bundled face plus at most eight explicit regular font files,
32 MiB each and 128 MiB total. Only a selected collection face is parsed and
inserted into an isolated per-job database; its TTC/OTC index is preserved.
Metadata is bounded and embedded SVG/color/bitmap glyph formats are refused
because their parsing paths would bypass document restrictions. No host font
discovery runs in the worker. Actual shaped glyphs report `FontFallback`,
`MissingGlyphs` and sorted `uncovered_scripts`; excessive coverage traversal
or report size is refused rather than silently truncated.

The planned GUI font path resolves required scripts through its existing font
system and requeues once with explicit path/index pairs. Authentic CJK,
Arabic and Hebrew typography needs licensed fixtures and native acceptance;
synthetic cmap tests prove selection/coverage only. Mermaid's proportional
font and text measurer will use this worker font stack, with measured metrics
and a documented license. Emoji remains deferred.

Layer pixels do not model all time or memory. Font shaping, glyph outlines,
path tessellation and filter scratch remain behind the process bounds. A
panic guard requires the worker's unwinding `media-worker` profile; an aborting
build has only the process boundary. The source digest identifies source
content/identity. Any future render cache must also key theme, target, renderer
version and the effective font inputs.

## Distribution and recovery decisions

Official macOS helpers use the release signing team and identifier
`org.kettle.terminal.media-worker`. Unsigned, ad-hoc and Darwin Nix builds
keep media unavailable while retaining text-terminal behavior. A verified
whole app-bundle transaction publishes the GUI and helper together.

Linux packaging must publish and recover a verified executable pair. The
4.9-to-5 bootstrap uses a verified worker capsule carried as shell-integration
data, followed by a journaled first-restart installation. That data directory
is never an executable lookup or execution location. Capsule contents,
manifest binding, interrupted publication and rollback use the existing
updater. Provenance includes the installed worker. Signed release-artifact
upgrade smokes remain final release acceptance, separate from hermetic tests.

Build the worker in its own Cargo invocation using `--profile media-worker`,
so clipboard TIFF features do not unify into its codec graph. Report worker
size separately. The GUI's existing size/startup budget remains in force.

## Planned presentation and authorization

Display-only policy and verified pane ancestry precede production display
pushes. A caller cannot infer permissions from an environment hint or the
newest pane. Read/control operations remain separate from display admission.
Integration must preserve existing user configuration and tool approvals.

Registered inline cards use bounded side data and recognized markers; unknown,
torn, overwritten or expired markers cannot draw content or impersonate the
terminal. All text extraction paths must scrub recognized marker payloads.
Raw PTY recording is a separate, documented privacy boundary. Widthless or
ambiguous wrapping falls back to clipboard/file delivery rather than inventing
an inline layout. Unsupported harness modes use the shelf/plain path until
their nonce-leak acceptance gate passes.

Card interactions, shelf eviction, viewer modalities and preview lanes need
native hit-testing, keyboard, scrolling, accessibility and Reduce Motion
checks. Media geometry must preserve the terminal grid, split/zoom behavior
and paint ordering. Raster success alone does not establish UI correctness.

## Acceptance and references

Each slice needs focused regressions, bounded hostile-input checks, native
platform CI, format/lint/build gates and an independent review. Performance
claims require quiet, recorded measurement windows; test durations on a busy
machine are correctness evidence only. Shipping requires exact signed
artifacts, native visual/audio acceptance and real installed 4.9 updaters
transitioning to those 5.0 artifacts on macOS and both Linux release
architectures. The Spanish catalogue requires owner acceptance before release.

Implementation details and fixed limits live in
[MEDIA-PROTOCOL.md](MEDIA-PROTOCOL.md), crate ownership in
[ARCHITECTURE.md](ARCHITECTURE.md), the threat boundary in
[../SECURITY.md](../SECURITY.md), verification in [TESTING.md](TESTING.md),
and qualified measurements in [PERFORMANCE.md](PERFORMANCE.md).
