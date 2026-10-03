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
  The client starts it with no arguments, an empty environment, `/` as its
  working directory and stderr discarded, in a process group of its own that
  is killed before the worker is reaped, and accepts a reply only after the
  worker exits 0 with nothing after the reply. It measures the worker and
  everything it started every 25 ms and kills them above 768 MiB together;
  a tree that cannot be measured fails the job. A worker process or a child
  of one that outlives its job, or a reply accepted from a worker that then
  crashed, is in scope.
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
