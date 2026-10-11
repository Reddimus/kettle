# Security policy

Thanks for helping keep kettle and its users safe.

## Reporting a vulnerability

Please **do not** open a public GitHub issue for security reports.

Use GitHub's private vulnerability reporting — open
<https://github.com/Reddimus/kettle/security/advisories/new> and submit a
draft advisory. That channel is encrypted to the maintainers only and lets
us coordinate a fix + release before the issue is public.

If you cannot use the web form, email the maintainers at the address
listed on the GitHub profile of any active committer with the subject
prefix `[kettle-security]`.

We aim to acknowledge a report within **3 business days** and ship a fix
within **30 days** of confirmation for issues we agree are exploitable.
Bigger or upstream-dependent issues take longer; we'll keep you in the loop.

## What's in scope

kettle is a terminal emulator, so its attack surface is roughly *"a
malicious program is running inside a tab and is trying to break out."*
Reports that fit any of these are welcome:

- **PTY-to-host escape** — an escape sequence, OSC payload, image protocol
  frame, or terminal extension that causes the kettle process to
  read/write outside the terminal sandbox (filesystem, network, OS
  clipboard), execute attacker-controlled code, or corrupt memory.
- **Clipboard exfiltration** — OSC 52 *read* paths that leak the host
  clipboard despite the `osc52` config gate (default `copy`).
- **Hyperlink / URI abuse** — an `OSC 8` or auto-detected URL that
  bypasses `kettle_core::links::is_safe_url` and reaches the OS opener
  with a hostile scheme, or a `file://` link that makes the OS opener run a
  program or follow a shortcut. Kettle refuses local links to programs and
  shortcuts (see `names_program_or_shortcut` and `check_file_link`): by
  extension, read as Windows reads a name; a macOS bundle folder; a macOS
  alias by its Finder flag or bookmark data; a Linux launcher by its first
  group; or an executable file without a document extension. An encoded separator in a file link is refused before
  anything looks at the path, so it cannot spell a network share. It
  resolves symlinks first and opens the resolved path, never the link's text,
  through the custom URL handler or the system opener, off the UI thread. A
  local process that rewrites the file between the check and the open is not
  stopped; it can already run code itself. A file link printed in a pane
  connected to another machine names a file there, so Kettle refuses to open
  the local file of that name, and behind a terminal multiplexer it asks
  first (`link_gate`, `pane_path_origin`). What runs in the pane when the
  link is opened decides, so output printed while a session was remote can
  still open locally after the pane returns to a local shell.
  Every `file://` link and OSC 7 working directory is checked after
  decoding (`decoded_file_url_path`, `plain_cwd`): no `..` segment, network
  path, control character or encoded separator, however it is spelled. Two
  exceptions are deliberate for OSC 7: a local WSL share
  (`//wsl.localhost/Ubuntu/…`) stays a working directory, and on POSIX a
  backslash (`%5C`) is part of a name, not a separator.
- **Control-caller verification** — a process that `kettle ctl get_state`
  reports as `verified` in a pane although that pane's own child process is
  not one of its ancestors, for example through a reused pid, a claim that
  does not match the kernel's view of the connection, or a parent link read
  while it changed. The connecting process's claim, the accept-time kernel
  identity and the ancestry walk live in `kettle-ctl`'s `identity` and
  `process` modules; pane child identities are read at spawn
  (`Pane::caller_root`). The control server itself is a same-user surface:
  a process running as you can already drive it when it is enabled.
- **Media routing** — a `show` push landing on a pane other than its
  sender's without being labeled unverified, reaching a Kettle the sender
  does not run in, naming a sender by anything but the kernel's view of the
  connection, or opening anything on screen. Routing lives in `kettle-ui`'s
  `media::route`; display discovery is `kettle-ctl`'s
  `Client::discover_display`.
- **Opening media outside Kettle** — a card menu's Open in Preview
  (macOS) or Open in Image Viewer (Linux) handing anything but a fresh PNG
  of the pixels Kettle shows to anything but the one permitted viewer:
  Preview, checked against Apple's signature requirement for
  `com.apple.Preview`, or `/usr/bin/eog` owned by root and writable only by
  root, run with fixed arguments, no shell and no search path, never the
  default association. The copy lives in a private per-process store
  (owner-only directory and files, 32 copies and 128 MiB counting any it
  could not delete, oldest dropped first once a minute old, closed and
  deleted on exit and by the crash sweep), is marked downloaded on
  macOS through Kettle's own handle, and is checked to still be Kettle's
  file before the viewer starts. A copy that reaches another user, outlives
  those bounds or is replaced before launch, or an open that no press on a
  card asked for, is in scope. The store is `kettle-ui`'s
  `paste_image::OPENED`; the hand-off is `media::external`.
- **Opening a video outside Kettle** — a video's Open in QuickTime Player
  (macOS) or Open in mpv (Linux) handing anything but a checked private
  copy of the file shown to anything but the one permitted player:
  QuickTime Player, checked against Apple's signature requirement for
  `com.apple.QuickTimePlayerX`, or `/usr/bin/mpv` owned by root and
  writable only by root, run with fixed arguments, no shell and no search
  path, never the default association, and only for a container and codec
  that player reads; never the video's poster, and never the file a program
  named. Kettle opens that file itself, read-only and without blocking, and
  copies it only while it is still the file shown: the same device, inode,
  size and modification time, and the same first 64 KiB, making the same
  container. The copy is a clone where the file system makes one and
  otherwise a chunked byte copy that, before every chunk, still fits with
  2 GiB of the volume left free; either kind is given up at exit or after
  two minutes, checked before and after a clone and between chunks. Copies
  made at once count each other's unwritten bytes against the reserve. A
  copy is made and checked without holding the store, so neither exit nor
  another open waits on the source's file system (the store's own
  operations are on Kettle's temporary directory); a read the file system
  never answers is not interrupted, holds one of the two open slots, and
  leaves its partial copy to the crash sweep.
  The copy is made owner-only before it is opened, named by the container
  its bytes are, checked again (its length, its first bytes, and the source
  unchanged while it was copied), marked downloaded on macOS, and checked
  to still be Kettle's just before the player starts. mpv then gets the
  copy's own file, read-only, as descriptor 3 (`fd://3`, after `--`), so it
  plays exactly what was checked. QuickTime Player takes a path, so a
  process running as the user could swap the copy in its owner-only
  directory between that last check and QuickTime Player opening it, as it
  could Preview's PNG; such a process could also start either app itself.
  A copy that does not reach the player is deleted at once (a kept copy
  that cannot be deleted stays counted until cleanup); a launch asks
  whether exit has begun just before it starts, though one already past
  that check can still race exit, whose cleanup then takes its copy; and a
  launch that fails is reported: on macOS when `open` cannot hand QuickTime
  Player the file, on Linux when mpv exits with an error. Videos past 4 GiB are not copied.
  The store keeps 8 copies and 8 GiB, each counted at its full length,
  copies being made included, drops its oldest once five minutes old (a
  player that has its copy open keeps reading it), and is closed, deleted
  and swept as the image copies are. Nothing in the GUI decodes the video.
  A copy that reaches another user, a player started on a file Kettle did
  not check, any player but the permitted one, or a copy past those
  bounds, is in scope. The store is `paste_image::VIDEOS`; the hand-off is
  `media::external::video`.
- **Codex's startup** — with `agent-display-codex` on, the zsh a new pane
  starts reads a Kettle-owned, read-only `.zshenv` that Kettle checks before
  each pane and points zsh at by borrowing `ZDOTDIR`, and the fish one runs
  Kettle's code from its command line (`-C`) after the user's configuration.
  Startup that runs anything but the `codex` function and the user's own
  startup files, leaves `ZDOTDIR` or Kettle's own variables changed, reaches
  a shell other than the one the pane starts, or writes any user file is in
  scope.
- **Bracketed-paste injection** — a paste payload that escapes the
  `\e[200~ … \e[201~` wrapper and runs as input.
- **Resource exhaustion via a single PTY frame** — a parser path that
  panics, allocates unbounded memory, or hangs the renderer on a small
  attacker-controlled payload (e.g. a sixel/kitty/iTerm2 image, OSC 52
  payload, scrollback line). Existing size caps already gate the
  obvious cases; a resource-cap chain bounds the kitty graphics
  protocol (PNG/JPEG/GIF decompression-bomb cap at 8192² / 64 MiB,
  `ImageData::new` `checked_mul` overflow guard, 96 MiB
  per-chunk-stream cap, 8-slot / 128 MiB in-flight cap, 128-frame
  total animation cap, 256-slot caps on `store` / `anim` / `frames`
  and on `virtual_placements` + `rel` combined); the 16 MiB sequence
  cap in `extract.rs` bounds any single APC/OSC payload. New bypasses
  are in scope.
- **Session/config tampering** — a config file or `session.json` that
  causes RCE, file-write outside the documented config/session paths,
  or persistent privilege escalation across launches. Note: every
  user-file read has a defense-in-depth size cap (1 MiB config, 16 MiB
  session.json, 4 MiB init.lua, plus the bg-image 8192² / 64 MiB cap)
  so a swap-attack with filesystem access can't OOM kettle on launch
  via these paths. Tampering that bypasses the cap (config that parses
  cleanly but escalates) remains in scope.
- **Media protocol** — `kettle-media` defines the frames a future media
  worker and the GUI exchange (no worker ships yet). Its decoder checks a
  whole frame, every length, count, enum and trailing byte, before it
  allocates, caps each direction, keeps external requests from expressing a
  GUI user pull, and never echoes input in a failure. A frame that gets past
  those checks with oversized or hostile content is in scope.
- **Previews the user asks for** — Kettle reads a file a pane names into a
  preview lane only on the user's own gesture, a `preview_link` label or
  the right-click menu's "Preview in Kettle" row, and only past the gate
  that opening the link passes: refused for a pane on another machine or
  one that has gone, asked about behind tmux, screen or zellij and checked
  again on confirm. A copied path or `file://` link
  (`preview_clipboard_path`, or Shift+right-click's "Preview Copied File")
  passes the focused pane's gate the same way, since it may have been
  copied from that pane; a file a file manager copied is this computer's,
  as no pane's output can put one on the clipboard, and must still be an
  absolute path that a file URL carries unchanged, never a `\\host\share`
  path (a share mounted as a folder reads like any other folder). Input a
  control client sends (`perform_action`, `dispatch_ui_key`, `send_mouse`)
  never counts as that gesture: while Kettle handles a control request it
  starts no such read. Terminal output, a control client or a remote host
  that starts such a read without that gesture, or past that gate, is in
  scope.
- **Copied and selected diagrams** — `render_clipboard_as_diagram` and
  `render_selection_as_diagram` read the clipboard or a pane's selection only
  on the user's own key press, palette pick or menu click, never for a control
  client, and refuse more than 1 MiB of text before taking a diagram from it
  (a selection is measured cell by cell before its text is built). What they
  take is rendered as the user's own bytes, never a path, so nothing on disk is
  read. Getting either to read for a control client, or to render something
  other than the text's own diagram, is in scope.
- **What a preview keeps** — a shelf item keeps the bytes a request carried,
  and an SVG's or a diagram's text as the worker read it from a file, both
  charged to the bounded preview account and released with the item's
  pixels; a file is kept only as its path and the authorization it was read
  under. Rendering an item again for another background re-reads a file only
  under that authorization, never while a control request is handled, and
  keeps the result only if it has the digest the item was rendered from.
  Reloading reads the item's own file again, as the user's pull, only on the
  user's gesture. A zoomed lane's sharper pixels re-read a file the same
  way: under the item's authorization, only for a view the user changed,
  never one a control client changed last nor for a change a control
  client made to the lane, and kept only with the item's digest; once a
  file changed, nothing more is read for that item, whether its lane closes
  or not, until a reload. Those pixels are charged to the same account, never evict an
  item's, and go with their item, lane or pane. A video's silent preview
  re-reads the file the same way, in the sandboxed worker: one stills job
  under the item's authorization, only on the user's own gesture (a press
  or Space in a focused lane), never while a control request or a Lua
  script's action is handled and never for a file found changed (a hover
  loop, `video-preview-hover`, only plays frames such a preview left, and
  reads nothing); its reply is refused from its header past
  the size of the eight-frame sheet it asked for, checked to be that sheet,
  kept only with the item's digest, charged to the same account, and held
  by the lane, never the item, until the lane closes, shows another item or
  the item changes. It plays no sound.
  While the user has given a lane the keyboard (`focus_preview`), no key
  pressed, chord, input-method text or control client's
  `send_keys`/`send_text` reaches a pane in that window; getting one
  through is in scope. The release of a key the terminal already had when
  the lane took the keyboard still goes to the terminal, to the pane
  focused when it comes, as every release does.
  The lane's source view shows control, format and bidirectional characters
  as U+FFFD. Getting a preview to keep source or pixels past that account,
  to read a file for a control client, or to show a changed file as the
  item it was, is in scope.
- **Media worker selection** — Kettle looks for its media worker only beside
  its own executable, at the path recorded at startup, never in `PATH`, the
  working directory or the environment. The worker must be a regular
  executable file, not a link, without set-id bits, owned by the user or
  root, with neither it nor its directory writable by anyone else, by mode
  or by ACL; on macOS its signature must also pass a strict check against
  Kettle's own requirement (Apple's chain to a Developer ID Application
  certificate of Kettle's team, under the worker's own identifier, with the
  hardened runtime on every architecture). A file that changes during the
  check is refused. Getting Kettle to accept a worker that fails any of
  these is in scope. A program running as the same user that rewrites a
  user-owned install is not: it can replace Kettle itself.
- **Media worker setup** — `kettle-media-worker` (not shipped yet) closes
  every inherited descriptor above stderr, turns off core dumps and, on
  Linux, becomes non-dumpable before it reads anything, then lowers its CPU,
  file-size, descriptor and (Linux) address-space limits and starts a
  watchdog. A descriptor that survives into the worker, a payload that
  reaches its stderr, or a worker that outlives its watchdog is in scope.
  The client starts it with no arguments, an empty environment but for
  `KETTLE_MEDIA_DECODER` (the trusted ffmpeg Kettle found, if any), `/` as its
  working directory and stderr discarded, in a process group of its own that
  is killed before the worker is reaped, and accepts a reply only after the
  worker exits 0 with nothing after the reply. It measures the worker and
  everything it started every 25 ms and kills them above 768 MiB together;
  a tree that cannot be measured fails the job. A worker process or a child
  of one that outlives its job, or a reply accepted from a worker that then
  crashed, is in scope.
- **Media worker sandbox** — before it reads a byte of a job's media or
  fonts, the worker opens the files the job names (its source, checked as
  rendering checks it, and its fallback fonts) and confines itself, and
  everything it starts, to them: no other file read, no write but
  `/dev/null`, no network, no program but a trusted video decoder's own, no
  leaving its process group, and no change to any file's mode, owner,
  attributes or times. Rendering reads only the files the worker admitted
  before confining itself: through their own handles, a file that failed
  to open keeps that failure, and a path the job never named is refused. On Linux this is Landlock,
  from ABI 1, handling every right the kernel knows, with held files
  granted by inode (a name moved onto another file grants nothing), the
  decoder's programs and their script and ELF interpreters executable, and
  the system library trees and the decoder's package prefix readable
  (Homebrew's, only when it is in a `Cellar` directly inside one of
  Homebrew's own prefixes; the Nix store; `/usr/local` or `/opt/local`;
  elsewhere only a real `lib` directory beside its `bin`, never a `lib`
  that is a link, and never the directory above, which could be a home). A seccomp filter on every thread refuses what Landlock does not
  cover: creating, binding or connecting any socket, and any socket pair
  but a connected Unix one (which the standard library's spawn uses and
  which reaches nothing else); System V IPC; truncation; metadata changes; `O_TRUNC` opens; namespaces and
  mounts; tracing; BPF; perf; io_uring; keys; `setsid` and `setpgid`; any
  ioctl but those asking about a descriptor or setting its own flags;
  any signal leaving the worker: from ABI 6 Landlock keeps signals inside
  the sandbox; on older kernels the filter allows only `kill` of the
  worker's own process group and thread- or queue-directed signals to the
  worker itself, so a decoder can signal only the whole group, never
  itself or one of its threads alone, and the worker does not kill a
  decoder past its deadline, which then goes with the worker's process
  group once the job has failed; and any process for a
  job that runs no decoder. The watchdog thread, started
  earlier, confines itself to nothing and reports whether it could; a job
  counts as confined only if it did, so a signal handler can never run on
  an unconfined thread. On macOS it is a deny-by-default Seatbelt profile
  (`sandbox_init_with_parameters`, every path a parameter) that allows,
  beyond the job's grants, only what AVFoundation was measured to need: the
  system's code and libraries under `/System/Library`, `/usr/lib` and
  `/Library/Apple` (never all of `/System`, whose `Volumes/Data` holds the
  users' homes), file metadata (names and sizes, never contents), sysctl
  reads, Apple's video decoder service and its IOSurface client, `/dev/fd`
  (descriptors already held), and what dyld needs to start a granted
  program; it refuses `setsid`, `setpgid` and System V IPC, and signals to
  anything outside its sandbox. A
  video is never decoded unconfined: where the sandbox cannot be applied (a
  Linux kernel without Landlock, or with it turned off; a macOS without the
  call) a video job fails with `backend_unavailable`, reason `sandbox`,
  while raster, SVG and Mermaid, which only Kettle's own code parses, still
  render in the bounded worker. A decoder that is a script launching other
  programs is not supported. A job reading or writing past its grants,
  reaching the network, running another program, leaving its group, or
  decoding a video unconfined, is in scope. The policy is `kettle-media-native`'s
  `sandbox`; the barrier is the worker's `answer`.
- **Media rendering** — the worker renders with `kettle-media-render`, safe
  code that writes nothing. A source path is opened once, read-only and
  non-blocking, and decided from that open file: it must be a regular file
  (a FIFO, device or directory is refused without blocking), an external
  request's attested device and inode must match it, at most its cap plus
  one byte is read, and a file that changes while it is read is refused. An
  image's format comes from its content; only PNG, JPEG, WebP, BMP and the
  first frame of a GIF are decoded, and the dimensions and decoded size the
  decoder reports are checked against 8192 pixels an edge and 64 MiB before
  any pixel is decoded. BMP and WebP headers are read first, and a WebP lossy
  frame must declare the size of the canvas or animation frame it fills.
  Reading a file other than the one opened or attested, a decode or resize
  that allocates past those caps, a decoder panic reached from hostile
  input, or rendered output that carries a transparent pixel's hidden color
  is in scope.
- **External video decoder** — a video's stills come from Apple's
  decoder on macOS or the user's own ffmpeg and ffprobe, never bundled.
  Kettle ships and loads no GStreamer, on any platform; `get_state` says so.
  Kettle's launcher looks for ffmpeg only in fixed places and the Nix
  profile under `HOME`, never `PATH`; the worker uses only the binary the
  parent names in its environment, and trusts it only when
  the file and every directory and link on the way to it belong to the user
  or root and no one else can write them (a sticky directory passes; on
  macOS the admin group counts as root, since its members are root through
  sudo, and Homebrew's directories are writable by it). It is checked again
  before each run. Each run is the worker's child in its process group,
  with an empty environment, a fixed argument list into which only numbers
  Kettle computed and the sniffed demuxer go, the `file` protocol alone, a
  fresh descriptor of the held file (checked to be that file) as its only
  input, stderr discarded, its output read to an exact size, and a
  deadline. On Linux a seccomp filter stops it from starting a process or
  leaving its process group or session; on macOS its process limit stops
  it from starting a process (a decoder is never run as root, whom the
  limit does not bind), and the worker's sandbox refuses it `setsid` and
  `setpgid`, so it cannot leave its group either. On macOS, MP4 and QuickTime
  are read first by AVFoundation inside the worker, opened by descriptor
  with references outside the file forbidden; decoding runs in Apple's
  decoder service, demuxing in the worker under its limits. Running a binary from
  anywhere else, an argument a request or the media chose, a decoder that
  starts a process, reads another file, or outlives its job, or a frame
  accepted at the wrong size, is in scope.
- **Video paste receipts** — a pasted or dropped video's receipt starts
  with a check, Kettle itself run as a short-lived helper with a two-second
  deadline: it opens the file only through a parent chain no other
  principal can change, refusing a link, a multiply linked file or one
  another principal can write, compares its identity and sampled contents
  when it opens it and before it answers, and reads at most 64 KiB to see
  that it starts like a video. On macOS and Linux it decodes nothing, and the
  poster is then the media worker's, sandboxed as above, from a job held to
  the device and inode the check opened, and kept only when the worker's
  file identity still matches; on Linux, when no decoder can make one, a
  cached freedesktop.org thumbnail the check opened the same way, which the
  worker renders only once its `Thumb::URI` and `Thumb::MTime` name the
  video as the check saw it. A cached thumbnail is a raster, so where the
  sandbox cannot be applied it renders in the bounded, unconfined worker as
  any raster does. Either poster is kept only when a second check, as
  strict as the first, finds the same file with the same sampled contents.
  No video or thumbnail byte is parsed in the GUI. On Windows, where no
  worker runs, the check asks the Shell's thumbnail provider, and identity
  and sampled contents are checked around that path-based call.
  A receipt for a file another principal could swap, a poster of a file
  other than the one checked, or any receipt byte parsed outside the worker
  on macOS or Linux, is in scope. The check is `video_preview`'s
  `run_worker`; the poster is `video_preview::worker_poster`.
- **Model frames** — `kettle_video_frames` (full MCP only, never the
  display server or Kettle's integrations, approved by nothing Kettle
  sets) and `kettle video-frames` return a video's frames to the caller: a
  separate, deliberate read, unlike showing. The file is opened by the
  caller's own process, inside its sandbox, and decoded in a worker that
  process starts. The image goes to the model, and the harness may keep it
  in its session files (Claude Code does); the command writes its JPEG
  privately (mode 0600), whole, and never over the video. Failures name no
  path and carry no media. A frames result reaching a caller that did not
  ask for it, the display server or an integration offering or approving
  it, a read outside the caller's own permissions, or the command writing
  anywhere but where it was told, is in scope.
- **Diagram rendering** — Mermaid source is parsed and laid out by merman
  0.8.0, pinned exactly, with its resource-constrained policy, a deadline,
  no network or file access, and Kettle's own text measurement from the job's
  fonts. Its SVG then passes the same sanitizer and admission as an outside
  SVG, in a generated mode that leaves out what it cannot check instead of
  passing it: an element whose attributes fail the checks, an id a later
  element repeats (the first element owns it, written or not), a style
  declaration it cannot read. Relative font sizes are resolved to absolute
  ones within the same number bound, or left out. A diagram that outlasts its
  deadline or resource policy, gets anything unchecked past the sanitizer,
  or makes the worker read or reach anything is in scope. A Markdown
  gallery's fences are found by pulldown-cmark 0.13.4 in the worker, within
  1 MiB, 32 pages and 64 KiB a page, under the same deadline; a document
  that gets past those caps, takes the parser past its deadline, or puts a
  page together from anything but one fence of the snapshot read is in scope.
- **SVG rendering** — an SVG is parsed with no DTD, and written back without
  scripts, foreign content, event attributes, namespaced attributes other
  than `xlink:href` and `xml:space`, or any `href` or `url()` that names
  something outside the document. Style sheets in those dropped subtrees
  have no effect. Its CSS (simple selectors only) is applied
  here and written as attributes, so usvg's own CSS engine never runs. A
  reference that cannot be read plainly, CSS this does not resolve, a font
  size other than an absolute number, `inherit` for a reference, a list of
  filters, a duplicate id, or a number large enough to overflow what usvg
  multiplies, refuses it (a tiny one is written as zero). resvg then parses it with image resolvers that load
  nothing and no resources directory, and draws text with the bundled face
  and explicit job fonts, with no host discovery. Up to eight held-read regular
  font files are allowed, 32 MiB each and 128 MiB total. Only the selected
  collection face is parsed, with bounded metadata and its index preserved;
  embedded SVG, color and bitmap glyph tables are refused before usvg can
  reach their separate parsers. Per-job databases cannot retain another job's
  supplied fonts. Its expanded size (references, `use` copies and
  per-vertex markers and filter input-name copies counted every time) and
  every layer, filter result, mask, clip and pattern tile resvg would allocate,
  plus each merge input's
  layer-sized copy, conversion and compositing charge, are admitted before any
  is built. Blend, composite and displacement also charge both input surfaces
  beside their output. A reference cycle, through inherited paint as well, is
  refused. An SVG that makes the worker read or fetch anything outside its
  explicit job inputs,
  load a font it was not given, expand or allocate past those limits before
  being refused, or crash the worker, is in scope. Beyond these bounds,
  where the worker's sandbox applies, it confines code running in it after
  a renderer exploit to the job's own files; where it cannot apply, raster,
  SVG and Mermaid still render under these bounds alone.
- **Lua plugin sandbox escape** — `lua-sandbox = safe` (the default)
  nils `os.execute`, `os.exit`, `io.open`, `io.popen`,
  `package.loadlib`, `loadfile`, `dofile`, etc. A bypass that lets a
  user-supplied `init.lua` (or a `kettle.add_url_handler` /
  `kettle.add_menu_item` callback) reach an external process, the
  filesystem, or a native library despite the sandbox flag is in
  scope. The `kettle.*` side-effect APIs are also capped against
  resource-exhaustion (1 MiB per `send_text`, 8 KiB per `notify`
  field, 1024-command queue length) — bypassing those caps to OOM
  kettle from a sandboxed script is in scope too. `lua-sandbox =
  trusted` is opt-in and restores the full Lua command and file APIs —
  out of scope. The `debug` library and native module loading remain
  unavailable because mlua's safe state removes them unconditionally.

  **What `safe` does not mean.** It nils the stdlib routes to a
  process, but `kettle.send_text` types into the focused shell and a
  newline in that text runs what it typed — that is the documented
  plugin API, and the shipped example uses it. Safe mode guards
  against a *careless* plugin touching the filesystem or spawning
  something behind your back; it does not contain a hostile one, so
  a report that a safe-mode plugin ran a command **by typing it** is
  working as designed rather than a sandbox escape. Run a plugin you
  have not read under `lua-sandbox = restricted`, where `send_text`
  and `exec_action` refuse; a bypass of *that* level is in scope.
- **Detachable-tabs handoff** — `--tab-handoff PATH` and
  `--tab-handoff-fd FD` restore a JSON payload from
  another kettle process. A handoff payload that bypasses path
  validation, escapes the JSON schema, or causes the receiving
  kettle to spawn a shell outside the documented argv / cwd
  bounds is in scope.
- **Build / supply-chain** — anything that lets a malicious dependency
  or build script reach the released binary.

## What's not in scope

- Issues that require **root or local code execution already**
  (e.g. "an attacker who can edit `~/.config/kettle/config` can change
  your theme") — that's the normal config surface.
- Bugs in upstream crates (`alacritty_terminal`, `vte`, `wgpu`,
  `cosmic-text`, etc.). Please file those upstream; if a kettle-side
  mitigation is also needed, mention it in your report and we'll
  coordinate.
- Crashes from valid terminal programs without a security impact. Open a public
  issue for those.
- Cosmetic / theme / font-rendering issues.

## Disclosure timeline

We default to **coordinated disclosure**: we'll work with the reporter on
a public-disclosure date once a fix is ready (typically the next release).
If a fix takes longer than 90 days from confirmed report we'll publish a
mitigations advisory even without a full fix.

## Hall of fame

Credit goes in the release notes for the version that ships the fix
(opt-in — reporters can stay anonymous). There's no monetary bounty.
