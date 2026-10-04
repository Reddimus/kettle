# Bounded media protocol

## Ownership

`kettle-media` is a leaf crate whose only dependency is `sha2`. It opens no files and starts no process
itself: its client reaches the filesystem and the worker only through a platform its caller supplies.
The public model uses six job kinds. Input limits apply when validating or encoding a job
and when decoding its frame. For paths, the worker later checks the opened object's size and
permissions using the job kind's input cap. External attestations are declarations from the
caller, not proof established by this crate.

`ExternalRequest` and the private-channel `Job` use different source types and frame kinds.
External paths carry only dev/ino attestations. GUI user pulls require an explicit action
witness. Decoding a private-channel UserPull never creates a GUI witness, and changing its
frame kind to an external request makes decoding fail.

The worker answers Ready only after setup. `check_ready` compares version, source hash and
protocol version, requiring the local protocol too. It returns RestartRequired on mismatch.
`check_peer_failure(UnknownMethod)` returns ReverseSkew with the older-GUI wording. Neither
helper retries. State sequencing and verification of which spawned worker supplied a reply
belong to P2.

## Wire version 1

The header is 11 bytes: `KMED`, u16 LE protocol version 1, a frame kind, then u32 LE payload
length. No padding or compression is permitted. Binary data and UTF-8 strings have u32 LE
byte lengths. Lists have u32 LE counts. Integers and f64 bit patterns use LE. Boolean and
optional-value discriminants are exactly 0 or 1. Floats must satisfy the field validators.
Native paths carry tag 0 for Unix bytes or tag 1 for Windows UTF-16LE, and the receiving
platform refuses the other encoding.

| Kind | Direction | Payload order |
| --- | --- | --- |
| 1 Hello | Parent to worker | Version text, source hash hex text, u16 build protocol |
| 2 Ready | Worker to parent | Same BuildId representation |
| 3 ExternalRequest | External to parent | Kind, restricted source, theme, canvas, target, fonts |
| 4 Job | Parent to worker | Kind, worker source, theme, canvas, target, fonts |
| 5 Rendered | Worker to parent | Width, height, RGBA blob, 32 digest bytes, optional path identity, display lines, fence count, optional index, fence sources, script names, warning codes |
| 6 Failure | Worker to parent | One fixed error-code byte |

Job-kind tags 0 through 5 are Mermaid, Svg, Raster, MarkdownDiagrams, VideoProbe and
VideoStills. Markdown adds one index byte. Stills adds count u8, edge u32, start f64, optional
end f64 and optional single-frame time f64. Single-frame time requires count 1, start 0 and
no end time. Interval times must be finite, nonnegative and ordered. The still-count cap is
16 and the requested edge cap is 2560, as in the video-frame contract.

Source tag 0 carries a bounded byte blob. Mermaid, SVG and Markdown bytes require UTF-8.
Tag 1 carries a native path followed by authorization tag 0 and dev/ino u64 values, or
private-channel authorization tag 1 with no payload. ExternalRequest refuses tag 1.
Theme stores background, foreground, all 16 palette colors and accent as four-byte RGBA
colors, then its dark flag. Canvas tags 0/1/2 mean theme/white/checker. Target stores width
u32, height u32, scale f64, optional crop of x/y/width/height u32. Crop lies within the target
box. Each font stores a native path and u32 face index.

Path identity stores dev, ino and size u64, signed seconds i64, then nanoseconds u32 below
one billion. Rendered rows use straight RGBA, without premultiplication. There is no stride
field. The byte count must equal checked width * height * 4. Digests refer to source content,
so the codec cannot verify them from returned pixels alone.

Encoding validates first, measures with checked lengths, then requests one exact frame
capacity. Decoding validates the header and the entire borrowed payload before requesting
owned storage. That pass checks trailing bytes too. The second pass measures each requested
byte copy and list capacity. The per-direction budgets include list element storage, rather
than treating serialized length as the only allocation bound. Allocation statistics do not
measure allocator rounding, caller-owned input or simultaneous stream buffering. Stream
readers buffer at most one capped frame and then invoke the same decoder. P2 must include
both that buffer and the owned result in footprint accounting.

## Digest

SHA-256 comes from the `sha2` crate the workspace already uses; a value computed
independently pins the framing. The input is exactly:

```text
"kettle-media-content-v1\0"
u64 LE content length
content bytes
u8 identity-present
if present: dev, ino, size, mtime seconds, mtime nanoseconds
```

The four first identity fields are eight bytes each, nanoseconds four bytes. This prevents
identical bytes read from different file versions from sharing a path cache key. The caller
supplies metadata from the held source descriptor. P2 must prove unchanged identity and
content while reading. SHA-256 here does not establish file authorization or worker identity.

## Fixed failures and unspecified bounds

Failure bytes 0 through 26 are pinned in `FailureCode` and round-trip tests. `code`, `reason`
and `model_message` map applicable cases to the model-facing error contract. Failures carry
no source, path, parser diagnostic or other input. WorkerUnavailable is the added fixed
reason for a missing or unverifiable worker. Codec/backend identities and platform package
hints are future bounded provider data, not free-form strings accepted in P1 failures.

The plan leaves some small metadata bounds unspecified. P1 chooses 64 bytes each for version
and source hash hex, 32 uncovered script names of 64 UTF-8 bytes, and at most 16 warnings from four
fixed variants. Native path length is a byte limit on both supported encodings. Empty build
names and hashes are refused; hex letter case is preserved, and exact build equality is
intentional. Changing tags, layouts or acceptance rules requires a protocol version decision
and updated golden fixtures.

## Build identity and worker availability

Every Kettle binary takes its identity from one build helper,
`crates/kettle/build_support/source_id.rs`: a hash of every file under `crates/`
(through links) and the workspace `Cargo.toml` and `Cargo.lock`, by relative
path and contents, read without git. A checkout and an exported source tree of
the same files agree; any source edit changes it; a rebuild, or the same source
built elsewhere, keeps it. The helper is under `crates/`, so it hashes itself.
`kettle`'s build script emits it as `KETTLE_SOURCE_HASH` (16 hex digits) and,
unchanged for the video-preview worker, as `KETTLE_SOURCE_ID`
(`<version> (<hash>)`). `BuildId::from_embedded(version, source_hash)` builds
the handshake identity from `CARGO_PKG_VERSION` and `KETTLE_SOURCE_HASH`, never
from the git commit. It is a compatibility token, not an attestation: a
worker's authenticity comes from the checks below and the signed release.

`client::WorkerClient` answers whether media previews are available.
`availability()` returns the last answer at once (`Checking` before the first
check finishes) and, when no check is running, starts one in the background, so
a replaced or removed worker shows on the next ask. A check inspects the worker
file, verifies its signature unless this exact file already passed, and
inspects it again, refusing a file that changed meanwhile. The file identity is
device, inode, size, modification time and status time, so an in-place rewrite
that keeps the modification time is still noticed. Failures are not cached.

The platform comes from the caller through `client::WorkerPlatform`. The
`kettle` binary's `media_platform` records the worker's path at startup:
`kettle-media-worker` beside the running executable, with links in the
executable's path resolved, so a renamed or deleted executable does not move it
and a Homebrew link finds its install. On macOS that is
`kettle.app/Contents/MacOS/kettle-media-worker`. `PATH`, the working directory,
`argv[0]`, the environment and the configuration are never consulted, and no
other directory is searched. Windows and other platforms report
`unsupported_platform`.

The worker must be a regular executable file, not a link, without set-id bits,
owned by the user or root, and neither it nor its directory may be writable by
group or others. On macOS neither may carry an ACL entry that allows anyone
more than reading (deny entries are fine); on Linux an entry that grants write
shows in the group mode bits, which are already refused. On macOS
`/usr/bin/codesign --verify --strict` must pass every architecture against this
requirement, and every architecture's signature must carry the hardened
runtime flag (`codesign --display` is asked about each one the universal
header lists):

```
anchor apple generic and identifier "org.kettle.terminal.media-worker"
and certificate 1[field.1.2.840.113635.100.6.2.6] exists
and certificate leaf[field.1.2.840.113635.100.6.1.13] exists
and certificate leaf[subject.OU] = "<kettle_update::APPLE_TEAM_IDENTIFIER>"
```

That is Apple's chain to a Developer ID Application certificate issued to
Kettle's team, under the worker's own identifier; the app's identifier is
`org.kettle.terminal`, so the app's requirement is not reused. Each `codesign`
run has a 30 second deadline; one that outlasts it is killed and reported as
`check_failed`, so a stuck tool cannot hold the answer at `checking`. A killed
run that has not exited a second later, as uninterruptible I/O can cause, is
left to a reaper, and no `codesign` run starts until it exits, so at most one
is ever left behind. Unsigned,
ad-hoc and other teams' workers fail it; there is no flag, variable or
fallback that accepts them. A local or Nix build on macOS therefore reports
`unverified_worker` until a development signing policy is chosen. On Linux the
package install checked the worker's bytes against the signed release; there
is no signature to check at run time.

These checks keep a stray, half-installed or foreign file from running as the
worker. A program running as the same user can rewrite a user-owned install,
including Kettle itself, so it is outside what they can stop.

`kettle ctl get_state` reports the answer as `media`:
`{"availability": "checking"}` or `{"availability": "unavailable", "reason":
<code>}`. Codes are fixed, never a path or tool output:

| Code | Meaning |
| --- | --- |
| `unsupported_platform` | This platform has no media worker |
| `no_install_location` | The running executable's directory could not be established |
| `worker_missing` | No worker beside the executable |
| `unsafe_worker_file` | The worker or its directory failed the file checks, or could not be read |
| `unverified_worker` | The signature check failed, or the file changed while it ran |
| `check_failed` | The check could not run |
| `incomplete` | The worker passed every check, but this build cannot render yet |
| `stuck_workers` | Two killed workers would not exit, so media is off until Kettle restarts |
| `not_configured` | The GUI was started without a media client |

No build renders yet, so `incomplete` is the best answer there is. Nothing
here spawns a worker.

## Running a job

`WorkerClient::render(job)` runs one job in a fresh worker, one job at a time,
on the caller's thread (never the UI's). It checks the worker again first, the
same check availability runs, inside the startup deadline, and refuses without
starting anything once two workers have been stuck. Nothing in the GUI calls it yet.

The platform starts the worker with `client::worker_command`: no arguments, an
empty environment, `/` as the working directory, stdin and stdout piped,
stderr discarded, and on Unix a process group of its own. Two threads move the
bytes, so no blocked read or write can hold a deadline: one writes Hello, then
the job once Ready has matched, then closes stdin; the other reads the first
frame under Ready's own cap (`wire::read_frame_within`, 149 bytes, checked
from the header before any payload is allocated), then the reply, then end of
file. The caller's thread watches the clock:

- **Ready within 5 s of the start, the check and the start included.** The
  worker is checked and started on a helper thread, so a check or a start that
  blocks (an executable on a stalled network filesystem) cannot hold the
  caller; a worker that starts
  too late is killed by the helper, and while such a start is still running
  no other begins. A worker that never answers is killed
  and, once reaped, retried once; total startup is at most 10 s. A Ready from
  another build is `RestartRequired`, never retried. Before a job, only a
  handshake refusal (`RestartRequired`, `UnknownMethod`) counts, and only with
  end of file after it and an exit rather than a crash.
- **The reply within the job's deadline**, 2 s for a raster and 3 s for other
  kinds, counted from before the job is written. A missed deadline kills the
  worker: `RenderTimeout`, unless it ended by itself as the deadline passed,
  when its exit says why. Nothing is retried after Ready.
- **A reply counts only** when end of file follows it with nothing between,
  and the worker then exits 0 by itself. A reply followed by more bytes, a
  second frame, a crash or a non-zero exit is discarded.
- **A refusal** keeps the worker's code only when a worker can mean it (input,
  file, render, skew and unavailability codes); a code naming GUI or agent
  state becomes `WorkerUnavailable`.
- **An exit without a usable reply** says why: 4 (the worker's watchdog) is
  `RenderTimeout`, 9 is `RestartRequired`, a signal (a CPU, file-size or memory
  limit, or a crash) is `RenderResource`, and anything else, including a
  protocol violation, is `WorkerUnavailable`. While it waits, the caller's
  thread also looks at the worker every 25 ms, so an exit is seen even when
  something the worker started still holds its stdout and no frame comes.

Stopping a worker gives it 250 ms to exit by itself, then kills its process
group and gives it 250 ms more. A worker reaped after the kill with any status
but `SIGKILL` ended by itself first, and that exit is its answer, so one that
exits 9 just as the startup deadline passes is `RestartRequired`, not a cold
start to retry. The platform kills the group before it reaps
the worker, even one that exited by itself (it checks with `waitid(WNOWAIT)`
first), so nothing the worker started outlives it and a process group id that
could already be someone else's is never signalled. No worker starts while
exited children reap themselves (an inherited ignored `SIGCHLD`, or
`SA_NOCLDWAIT`), since one could vanish and its group id be reused before
Kettle sees it exit; a worker something else reaped anyway reads as lost and
is never signalled again, by a kill or otherwise. A worker that will not exit after the kill, as one
stuck in uninterruptible I/O on a network filesystem can, is kept and
counted, and reaped at a later check once it does exit; after two, media is
off for the life of the process (`stuck_workers`). The pipe threads end when
the worker's pipes close; nothing joins them, so none can hang the caller.

`kettle` restores SIGPIPE's default action for its command line, so a write
to a worker that has died must not raise it. The writer thread first asks the
platform to guard its writes: on Linux, where the signal goes to the writing
thread, it is blocked there; on macOS, where it goes to the whole process,
the platform marks the worker's stdin pipe `F_SETNOSIGPIPE` when it starts
the worker. Either way the write fails with `EPIPE` instead.

**Memory.** While it waits, the caller's thread also measures the worker and
everything in its process group on a fixed 25 ms schedule (a slow probe skips
ticks rather than shifting them): that is everything a group kill reaches,
including a grandchild whose parent has exited. On macOS the group comes from
`proc_listpgrppids` and each member's physical footprint from
`proc_pid_rusage`, as Activity Monitor reports it; on Linux from each thread's
`children` list plus a scan of `/proc` for the group at most once a second
(every 100 ms on a kernel without those lists, where the scan is the only way
to find a new child), and each member's resident pages from `statm`, read as
bytes since a command name need not be UTF-8. A member counts, toward the sum
and toward the 64-process cap, only if it is still in the group once
measured, so a pid reused in between is not counted and a remembered one that
has exited is forgotten.
The sum may count shared pages twice, which errs toward stopping the job. Above
768 MiB the worker's group is killed: `RenderResource`, never retried, before
Ready, after it, or while a worker that has replied is exiting (a reply does
not excuse memory held on the way out). A live worker that cannot be measured, or a tree of more than
64 processes, fails the job closed (`WorkerUnavailable`) rather than count as
nothing; one that exited as it was measured does not. Sampling is protection,
not proof: an allocation can cross the limit briefly between samples.

Not yet: the GUI's preview account and admission, and cancellation follow in
later slices.

## The worker executable

`crates/kettle-media-worker` builds `kettle-media-worker`, a separate
executable with no command-line interface, logging or UI. Its build script
embeds the same `KETTLE_SOURCE_HASH` as `kettle`'s, from the same helper, so
both binaries of one source answer with one `BuildId`. On Linux and macOS:

1. **Early setup, the first statement of `main`.** Before any argument,
   environment variable or input is read, and before any thread, hook or
   library starts, it closes every descriptor above stderr and sets the core
   size limit to 0. Linux uses `close_range(3, ~0, 0)`; where the kernel or a
   seccomp policy refuses that, it closes each descriptor `/proc/self/fd`
   lists (not each number below the descriptor limit, which a parent can
   lower below a descriptor it already holds). macOS has no `close_range`; it
   closes each descriptor the kernel lists for the process
   (`proc_pidinfo(PROC_PIDLISTFDS)`), refusing a list that fills its buffer. Linux also clears the dumpable flag
   (`PR_SET_DUMPABLE`). Only `EBADF` is tolerated; any other failure exits 8.
2. **A panic hook** that writes `media worker panic` to stderr and nothing
   else: no message, payload, location, backtrace or crash file. The GUI will
   discard the worker's stderr anyway.
3. **Resource limits**, each lowered to its value or to an inherited soft or
   hard limit that is already lower, never raised: CPU 5 s, regular-file size 0,
   descriptors 32, and on Linux address space 1 GiB (macOS does not enforce
   one). Failure exits 8.
4. **A watchdog thread** that exits 4 when the current phase's deadline
   passes, whatever the main thread is blocked on: 5 s from start until Ready
   is written, 5 s from Ready until the job arrives, and 3 s from the job until
   its reply is written. A parent that stalls or dies cannot keep the worker
   alive. A watchdog that cannot start exits 8.
5. **One job.** Hello must carry this build's identity. A different build, or
   a frame header of another protocol version, is answered
   `RestartRequired` and exits 9. Ready follows, then one Job and one reply,
   then exit 0. A frame out of order, cut short or that does not decode is
   answered `BadParams` (or `TooLarge`) and exits 2; a parent that closes stdin
   between frames ends the worker quietly with 0. stdout carries frames only, through one writer.

The reply is the job rendered by `kettle-media-render` (below), or its fixed
failure. Exit codes 4, 8 and 9 mean what they mean for the video-preview worker. On
other platforms the binary exits 8 at once, and nothing starts it there. The
worker is built with the workspace but not packaged or started yet; the
release profile pins it at `opt-level = 3`. The feature `test-faults` lets a
test job make it panic, for the panic test; no shipped build enables it.

## Rendering a job

`crates/kettle-media-render` is what the worker renders with, in safe code
(`forbid(unsafe_code)`). It reads its source and writes nothing: no file is
created, written, renamed or removed, and it starts no process and opens no
socket; a source guard test holds it to that, and the worker's file-size limit
of 0 backs it up. Every size is checked before the work it would cost, so the
worker's limits and the client's deadlines and memory limit are a second
bound, not the only one. An empty target box, one over 4096 pixels on an
edge, a scale that is not a positive finite number, or a crop that is empty or
leaves the box, is `BadParams`. Raster and SVG jobs are rendered; every other
kind is `UnsupportedMedia` until its renderer lands. On Windows, where no
worker runs, every job is `UnsupportedPlatform`.

**The source.** Inline bytes over the job kind's input cap (32 MiB for a
raster, 2 MiB for an SVG) are `TooLarge`. A path is opened once, read-only, non-blocking and
without becoming a controlling terminal, and everything after that is decided
from the open descriptor, never the path:

| The file at the path | Answer |
|---|---|
| missing, or any open failure but permission | `FileNotFound` |
| not readable | `FilePermission` |
| a directory, FIFO, socket or device (a FIFO with no writer does not block) | `FileNotRegular` |
| an external request's attested device and inode differ from the open file | `Changed` |
| larger than the cap when opened, or more than the cap read | `FileTooLarge` |
| a different identity or size after the read than when opened | `Changed` |
| a symbolic link at the leaf, or group-writable | followed, accepted |

The descriptor reads at most the cap plus one byte, so an oversized file is
never read whole. Renaming another file over the path after the open changes
nothing: the open file is read. The source is not a snapshot: a same-size
rewrite that restores its modification time between the two checks is not
seen. The digest is taken over the bytes read and that file's identity
(device, inode, size and modification time), as [Digest](#digest) frames it.

**Raster decoding.** The format comes from the content's signature, never the
name or extension. PNG, JPEG, WebP, BMP and GIF are decoded; any other format,
TIFF included, is `UnsupportedMedia`. Two containers are read before their
decoder sees them. A BMP header, or a WebP extended header's canvas, that
declares an image past the caps below is `RenderResource` (their decoders
would refuse some of these with a parse error instead). In a WebP, every lossy
bitstream must declare the size it fills: a still image's must match the
canvas, and an animation frame's must match that frame, with the chunk after
a frame's alpha plane being that bitstream. A mismatch is `RenderParse`,
before the decoder can allocate for the larger size or write past a smaller
buffer. Then, before a pixel is decoded, the decoder reports the image's
dimensions and its native decoded size, and both are checked: an edge over
8192 pixels, RGBA over 64 MiB, or a native size over 64 MiB (a 16-bit image
needs more) is `RenderResource`. The codec's own limits are set to the same
values, but they are best effort. A GIF or an animated WebP yields its first
frame only. Content that does not decode is `RenderParse`.

**Fitting.** The image is scaled to fit inside the target box, keeping its
aspect ratio, each edge rounded and kept between 1 pixel and the box. It is
resampled (triangle filter) with premultiplied alpha, so a transparent pixel's
hidden color does not bleed into its neighbours, and returned as straight
RGBA in which every fully transparent pixel is all zero. Resampling filters
each input row once and keeps it only while an output row needs it, so beside
the decoded image and the result it holds a few rows, never a full-size
working copy. A crop is in target
box coordinates, with the fitted image centered in the box: it returns just
that region, transparent wherever the image does not reach. The canvas color
is the GUI's to draw behind the result, and the scale is for vector content: a
raster target is already in device pixels.

**SVG: sanitizing.** The source must be UTF-8 (`RenderParse` otherwise). It
is parsed once with at most 500,000 nodes (`RenderResource` past that) and no
DTD: a document type declaration, and with it any entity, is `RenderParse`.
The root must be an SVG-namespace `svg` element. The document is then written
back, so usvg only ever sees the rewritten text:

| In the source | Written back |
|---|---|
| elements outside the SVG namespace (editor metadata) | dropped, with their content |
| `script`, `foreignObject`, `tref` | dropped, with their content (a `switch` falls back to its next child) |
| `<style>` elements and `style` attributes | resolved here and written as presentation attributes (below); none of the document's CSS reaches usvg |
| `href` or `xlink:href` naming a local fragment (`#id`) | kept: the plain one when both do, as usvg prefers it |
| any other `href` (`file:`, `http:`, `data:`, a path) | removed |
| other namespaced attributes (`xlink:title`, `xml:base`, ...) | dropped (`xml:space` kept): usvg reads attributes by local name |
| a property `url(#id)` | kept |
| a property naming anything else | the attribute, or the declaration, removed |
| a backslash, or an unclosed `url(`, in a property | `RenderParse` |
| a selector other than a type or `*` with `.class` and `#id` parts (a combinator, pseudo-class or attribute selector), an at-rule, any `!` in a declaration, or a property name other than lower-case letters and hyphens | `RenderParse` |
| style rules plus the declarations they apply, times elements, over 10,000,000 | `RenderResource` |
| a presentation property value over 1,024 bytes | `RenderParse`: usvg parses an inherited value again for every element it reaches |
| a font size other than a number with an absolute unit (`em`, `ex`, `%`, or a keyword such as `larger` or `xx-large`) | `RenderParse`: usvg scales each by the parent's size, and a chain multiplies past any bound |
| a percentage past 1000% | `RenderParse`: viewports nest, each scaling the next |
| a dash list of more than 64 entries | `RenderParse`: usvg keeps a copy for every element it applies to |
| a `font-family`, `font-variation-settings` or `font-feature-settings` over 256 bytes | `RenderParse`: usvg copies them into every positioned piece of text |
| `inherit` for a clip, mask, filter or marker | `RenderParse`: it takes a reference from the parent |
| a filter other than `none` or one `url(#id)` (a list, or a function such as `blur()`) | `RenderParse`: resvg runs a list on one layer with results of different sizes |
| an `feConvolveMatrix` order over 64 | `RenderParse` (usvg multiplies the two in 32 bits) |
| a duplicate `id` | `RenderParse`: usvg resolves a duplicate by its first or last element depending on the reference |
| a number in a property or declaration past 10,000,000 | `RenderParse` |
| a non-zero number below 1e-6 | written as `0` |
| event attributes, comments, processing instructions | dropped |

CSS is applied here, the way usvg applies it: an element's presentation
attributes, then the matching rules in ascending specificity (a later rule
winning a tie), then its `style` attribute, each declaration replacing the
value before it, and only presentation properties applying; the `marker`
shorthand sets all three marker properties and the `font` shorthand its parts
(one usvg could not read is left out, as usvg leaves it). `mix-blend-mode`,
`isolation` and `font-kerning`, which usvg reads from CSS but ignores as
attributes, are written in a `style` attribute composed here from the
keywords usvg reads, and any other value is left out. Comments are removed
first. The winners are written as attributes, so usvg's CSS engine never
sees the document's CSS (only the composed keyword `style` above): which elements a rule reaches, how often it matches again in `use`
copies, and how it splits a property name are no longer questions.

The number bounds keep the products usvg forms directly and then unwraps (a
marker's size times a stroke width, a radius times a scale) finite and
non-zero; the rounding noise editors write (about 6e-17) becomes the zero it
stands for. They cannot rule out every derived value rounding away (a tiny
width added to a far coordinate); the guard below answers those. A
unit that begins with `e` (`2em`) is read as a unit, not an exponent. Hex colors,
fragment names and identifiers (`id`, `class`, filter result names) are not
read as numbers.

The writer escapes `&`, `<`, `>` and quotes itself, and tabs and line breaks
in attributes, so values reach usvg exactly as they were parsed. A rewritten
document over 8 MiB is `RenderResource`. usvg then parses it with no
resources directory and with image resolvers that return nothing for data
and string references alike, so even a reference the rewrite missed reads no
file and no network.

**SVG: structure.** Before usvg, the document's cost once its references are
expanded is counted: one unit per element, plus every number its attributes
hold (path data, point lists and filter tables alike; identifiers such as
`id` and `class` aside), a unit per 64 bytes of attribute text, and the
characters of its text times the text elements it sits in (usvg copies what
each of them sets into every positioned piece), plus, for a text element, its
spans times its characters (usvg shapes the whole chunk once per span), and
for a text path, its characters times its path's numbers (each character is
laid against every segment), plus a target's whole cost each
time it is referenced (`use`, paint servers, clips, masks, filters, `feImage`,
text paths and linked templates), plus, on a shape that can carry markers,
its vertices times the most expensive marker (a path's vertices are its
numbers: an `H` or `V` takes one). This runs on the rewritten text, where
CSS is already attributes. Paint is counted without following which
declaration wins: the paint an element is drawn with is one declared on it
or an ancestor.
Every element's paint context reaches all of those (its own references, then
its parent's context, and every paint server the document uses where it
declares `context-fill` or `context-stroke`); a shape is charged for its
context once, text (a link inside text included) twelve times per character
(usvg copies a piece's paint for the span, its laid-out chunk and its
flattened outline, each with up to three decorations), and a `use` once per
piece in its copy. A `use` copy inherits marker properties from the `use`; other
targets inherit from where they are defined. Each of these is
`RenderResource`:

- more than 125,000 elements;
- an expanded cost over 1,000,000 units;
- an expanded nesting deeper than 256 (usvg and resvg recurse that deep);
- more than 8 viewports (`svg`, `symbol`) nested, references followed: a
  percentage takes its size from the viewport around it;
- a filter of more than 64 primitives: usvg looks up each primitive's input
  among those before it;
- a marker that could hold markers: one whose content, or what it references,
  sets a marker property, or one that inherits one. usvg allows such nesting,
  and it multiplies per vertex at every level.

A reference cycle is `RenderParse`, through paint contexts as well: usvg
recurses through a pattern whose content may draw with that pattern without
end, so a pattern defined under an element whose paint names it is refused
whatever its own content declares.

**SVG: layers.** The image is fitted inside the target box keeping its aspect
ratio, then, without a crop, within 1024 pixels a side and 1,048,576 pixels
(`MAX_SVG_RENDERED_EDGE`, `MAX_SVG_RENDERED_PIXELS`); the result may be
smaller than the box. A crop selects target-box pixels around the centered
image at the full fitted size, and must itself fit those limits, or it is
`BadParams`. Before the canvas is allocated, every allocation resvg 0.48.1
would make is counted, each time it would make it, against 4,194,304 pixels
(`MAX_SVG_LAYER_PIXELS`):

- an isolated group's layer, at its transformed bounds widened by 2 pixels a
  side, clamped to resvg's own limit (from -2 to +3 canvas widths and
  heights: up to 25 times the canvas area);
- each filter primitive's result at the layer's size (an input that is the
  source graphic copies the whole layer, and every result lives until the
  filter ends), and what an `feImage` renders;
- a clip's canvas and mask at the layer's size, for every clip in a chain
  and every clipped group inside one;
- a mask's canvas and masks at the layer's size, and its content, for every
  mask in a chain;
- a pattern tile at its own transformed size, for every fill and stroke that
  uses it, and its content.

Past that is `RenderResource`, and so is drawing more than 1,000,000 nodes,
which bounds the drawing whatever the structural count allowed. A transform
that is not finite is `RenderParse`, and so are a filter or primitive
rectangle reaching past 16,777,216 pixels on its layer (tiny-skia's integer
conversion of it unwraps), a list of filters on one element, and an image
node, which nothing may load. Pixels are not time, and filter scratch
buffers and path tessellation are not counted: the worker's deadline and the
client's memory limit still stand behind this.

The admission above is built from usvg's and resvg's source as pinned, and
it refuses what it cannot follow rather than guess. It is not a proof that
neither library can panic on some input it allows, so usvg's parsing,
resvg's rendering and the raster decoders run inside a guard that answers a
panic as `RenderParse`. That takes unwinding: the worker is built with the
`media-worker` profile (release, unwinding) for that reason. A build that
aborts on panic instead leaves the worker as the crash boundary: the panic
ends only the worker, the job fails, and Kettle is untouched. The fuzzing
that follows this work aims at what remains.

**SVG: result.** resvg's premultiplied pixels come back as straight RGBA,
rounded, with every fully transparent pixel all zero. Text is drawn with the
JetBrains Mono face bundled in the worker and nothing else: no host font is
discovered or loaded, and any `font-family` resolves to that face. The
result carries the source as display lines: at most 2,000 lines of at most
4,096 bytes, cut at a character boundary, with `SourceDisplayClipped` when
anything was left out. As for a raster, the canvas is the GUI's to draw
behind the result, and the target's scale is unused (the box is already in
device pixels). The worker builds the font database before it reports
Ready, outside the job's deadline.

## P2 boundary and separate worker decision

Production uses a separate O3 `kettle-media-worker` executable. The parent resolves it only
from its installation and verifies it as above, and validates build equality after startup.
No search through PATH or cwd, and no re-exec of the terminal for the new worker.

P2 adds the spawn/deadline/footprint/reap client, the heavy safe renderer and the actual worker
binary. It owns setup sequencing, one startup-only retry, no retry after Ready, fd sweep and
non-dumpable setup, minimal environment, neutral cwd, null stderr, process groups, rlimits,
self-sandbox, bounded pipe progress, aggregate child/decoder footprint monitoring and bounded
reaping/abandonment. It must enforce decoded-raster caps before codec allocation, device and
preview-account caps before upload, SVG amplification caps, source identity and no-write
policy. It also measures the separate worker's size, spawn-ready time and hostile footprint,
and verifies atomic two-binary packaging/update behavior. Renderer/platform acceptance,
installation identity and those resource budgets are not proved by P1's fixture.
