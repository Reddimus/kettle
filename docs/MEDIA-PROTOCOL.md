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
same check availability runs, and refuses without starting anything once two
workers have been stuck. Nothing in the GUI calls it yet.

The platform starts the worker with `client::worker_command`: no arguments, an
empty environment, `/` as the working directory, stdin and stdout piped,
stderr discarded, and on Unix a process group of its own. Two threads move the
bytes, so no blocked read or write can hold a deadline: one writes Hello, then
the job once Ready has matched, then closes stdin; the other reads the first
frame under Ready's own cap (`wire::read_frame_within`, 149 bytes, checked
from the header before any payload is allocated), then the reply, then end of
file. The caller's thread watches the clock:

- **Ready within 5 s of the start, the start itself included.** The worker
  is started on a helper thread, so a start that blocks (an executable on a
  stalled network filesystem) cannot hold the caller; a worker that starts
  too late is killed by the helper. A worker that never answers is killed
  and, once reaped, retried once; total startup is at most 10 s. A Ready from
  another build is `RestartRequired`, never retried. Before a job, only a
  handshake refusal (`RestartRequired`, `UnknownMethod`) counts, and only with
  end of file after it and an exit rather than a crash.
- **The reply within the job's deadline**, 2 s for a raster and 3 s for other
  kinds, counted from before the job is written. A missed deadline kills the
  worker: `RenderTimeout`. Nothing is retried after Ready.
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
group and gives it 250 ms more. The platform kills the group before it reaps
the worker, even one that exited by itself (it checks with `waitid(WNOWAIT)`
first), so nothing the worker started outlives it and a process group id that
could already be someone else's is never signalled. A worker that something
else reaped (an inherited ignored `SIGCHLD` does that) reads as lost and is
never signalled again. A worker that will not exit after the kill, as one
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

Not yet: footprint polling and the 768 MiB aggregate limit, the GUI's
preview account and admission, and cancellation follow in later slices.

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

This build answers every job `WorkerUnavailable`: no renderer is linked in.
Exit codes 4, 8 and 9 mean what they mean for the video-preview worker. On
other platforms the binary exits 8 at once, and nothing starts it there. The
worker is built with the workspace but not packaged or started yet; the
release profile pins it at `opt-level = 3`. The feature `test-faults` lets a
test job make it panic, for the panic test; no shipped build enables it.

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
