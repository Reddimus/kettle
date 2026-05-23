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
  with a hostile scheme.
- **Bracketed-paste injection** — a paste payload that escapes the
  `\e[200~ … \e[201~` wrapper and runs as input.
- **Resource exhaustion via a single PTY frame** — a parser path that
  panics, allocates unbounded memory, or hangs the renderer on a small
  attacker-controlled payload (e.g. a sixel/kitty/iTerm2 image, OSC 52
  payload, scrollback line). The cycle-47 / cycle-49 / cycle-118 caps
  already gate the obvious cases; the cycle-576..582 kitty resource-cap
  chain bounds the kitty graphics protocol (PNG/JPEG/GIF decompression-
  bomb cap at 8192² / 256 MiB, `ImageData::new` `checked_mul` overflow
  guard, 384 MiB per-chunk-stream cap, 32-slot in-flight cap, 256
  frames-per-image cap, 64-slot caps on `store` / `anim` /
  `virtual_placements` / `rel` / `frames`); the cycle-10 64 MiB cap in
  `extract.rs` bounds any single APC/OSC payload. New bypasses are in
  scope.
- **Session/config tampering** — a config file or `session.json` that
  causes RCE, file-write outside the documented config/session paths,
  or persistent privilege escalation across launches. Note: the
  cycle-584..587 read-size sweep adds defense-in-depth size caps on
  every user-file read (1 MiB config, 16 MiB session.json, 4 MiB
  init.lua, plus the bg-image 8192² / 256 MiB cap from cycle-584) so
  a swap-attack with filesystem access can't OOM kettle on launch via
  these paths — but tampering that bypasses the cap (config that
  parses cleanly but escalates) remains in scope.
- **Lua plugin sandbox escape** — `lua-sandbox = safe` (default since
  cycle-376) nils `os.execute`, `os.exit`, `io.open`, `io.popen`,
  `package.loadlib`, `loadfile`, `dofile`, etc. A bypass that lets a
  user-supplied `init.lua` (or a `kettle.add_url_handler` /
  `kettle.add_menu_item` callback) reach an external process, the
  filesystem, or a native library despite the sandbox flag is in
  scope. `lua-sandbox = trusted` is opt-in and explicitly carries
  the same surface as native Lua — out of scope.
- **Detachable-tabs handoff** — `--tab-handoff PATH` (cycle-403) and
  `--tab-handoff-fd FD` (cycle-408) restore a JSON payload from
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
- Crashes from **valid** terminal programs (htop, vim, tmux, nvim) that
  reproduce on Alacritty / kitty / WezTerm too — those are normal bugs;
  open a public issue.
- Cosmetic / theme / font-rendering issues.

## Disclosure timeline

We default to **coordinated disclosure**: we'll work with the reporter on
a public-disclosure date once a fix is ready (typically the next release).
If a fix takes longer than 90 days from confirmed report we'll publish a
mitigations advisory even without a full fix.

## Hall of fame

Credit goes in the release notes for the version that ships the fix
(opt-in — reporters can stay anonymous). There's no monetary bounty.
