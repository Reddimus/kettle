# Changelog

All notable changes to kettle. Format roughly follows
[Keep a Changelog](https://keepachangelog.com/); the project moves in small,
durable, fully-tested cycles (lint · build · test · docs · commit · CI).

## [Unreleased]

### Fixed

- Keep paths containing wide characters whole during double-click selection.
- Avoid signaling an already reaped Unix PTY child during pane teardown.
- Keep Command shortcuts from selecting hints, changing vi selection or
  activating menu mnemonics; prevent control-plane typing into a field
  covered by a menu or vi mode.
- Use the actual UTC calendar day for solar theme scheduling, including leap years.
- Recognize working-directory, prompt and completion OSCs after repeated
  escapes or intervening controls; preserve ordinary recorded output and
  strip those escape prefixes correctly in session logs and `exec --strip-ansi`.
- Coordinate Linux installation and removal with the updater, and remove its
  idle lock during uninstall without allowing stale lock holders to proceed.
- Make complexity, deadline and private video-output fixtures independent of
  runner scheduling and umask while retaining the production bounds.
- Accept absolute cache directories in the macOS release update smoke.
- Create missing private data directories with owner-only modes, so agent
  plugins can install under a group-writable default umask.
- Stop video-session test heartbeats after the final frame, avoiding a write
  racing the worker's normal exit.

## [5.0.0] — 2026-10-11

### Added

- Inline media cards for Claude Code: `kettle mcp --display`, launched by
  Kettle's Claude Code plugin, asks for a card under the `kettle_show` call,
  and Kettle registers one only for Claude Code under Anthropic's signature
  running in the pane the item lands in. Kettle builds the card's cells and
  caption; the plugin's hook collects them once and prints them under the
  call, where Kettle paints the image. The model is told only that the media
  shows below the call.
- `kettle agent-setup --print` prints a `codex` shell function to review and
  add to your shell's startup file. In a Kettle pane, with Codex CLI 0.159
  or a later 0.x, an interactive Codex session started through it (fresh,
  `codex resume` or `codex fork`) gets this Kettle's display server for that
  launch only, so `kettle_show` sends media to the pane's shelf; such a
  session runs without Codex's shared background server, so it does not
  appear in `codex agents`. Every other Codex command, and any other version,
  runs unchanged, and Kettle edits neither the shell's startup files nor
  Codex's configuration.
  `--status` reports whether the shell reaches its Kettle with agent previews
  on, what a launch would get, and Claude Code's plugin and policy state for
  the pane, and `--uninstall` prints how
  to remove the function. It is for bash, zsh and fish, on macOS and Linux.
- The Codex launch function approves `kettle_show` by its own name for the
  launch, never the whole server, so media shows under any approval policy:
  with `approval_policy = "never"`, Codex refused every call that would ask.
- Codex shows a card under the call for media it sends with `kettle_show`,
  like Claude Code. On macOS with Codex CLI 0.162, the launch function adds a
  `PostToolUse` hook that collects the card Kettle registers for the call, and
  Codex prints it under the call, where Kettle paints the image. Codex asks
  you once to trust the hook in its own hook review; until then, and with
  other Codex versions, media goes to the pane's shelf. A card's provenance
  tab names the harness that printed it.
- "Copy agent setup commands" in the palette (`copy_agent_setup`) puts the
  `codex` function for your shell on the clipboard, ready to paste into its
  startup file. In English and Spanish.
- A new pane on macOS or Linux whose `PATH` has no `kettle` gets the running
  Kettle's folder appended, after your own commands, so agents and setup
  commands there find it (`add-kettle-to-path`, Settings → Agents; `off`
  leaves the `PATH` as configured).
- Codex previews (`agent-display-codex`, Settings → Agents → Codex previews;
  off by default, needs agent previews): the zsh or fish a new pane starts
  defines `codex` itself, as the function `kettle agent-setup --print`
  prints, so there is nothing to paste. zsh reads a Kettle-owned, read-only
  `.zshenv` through a borrowed `ZDOTDIR` it puts back exactly; fish runs
  Kettle's code after your configuration (`fish -C`). Your startup files run
  as before, and a `codex` you define or can autoload still wins. `kettle
  agent-setup --status` says whether your saved settings turn it on.
- Claude Code previews (`agent-display-claude-code`, Settings → Agents →
  Claude Code previews; off by default, needs agent previews): Claude Code
  started in a new pane gets Kettle's own plugin, the display server and the
  hook that prints inline cards, through `CLAUDE_CODE_PLUGIN_DIRS`. Kettle
  writes the plugin read-only into its own data directory and checks it
  before every new pane. It changes none of Claude Code's settings, and new
  panes get nothing while Claude Code's managed policy forbids plugins from
  the environment, nor a plugin an outer Kettle passed down. Settings says
  why when a pane gets no plugin.
- `kettle ctl get_state` reports `caller`: whether the connecting process runs
  in one of this Kettle's panes, from the process tree rather than anything
  the caller says. A client's first request claims its own pid and start time;
  Kettle believes it only when the kernel agrees, then walks at most 64
  parents looking for a pane's own child process. Every pane now starts with
  `KETTLE_PANE_ID` and `KETTLE_PID`, set after your `env` entries; they are
  reported as hints and never count as proof.
- `kettle ctl` and `kettle mcp` now pick the Kettle they run inside, matched
  by process ancestry, before a newer one; then the one `KETTLE_PID` names;
  then the newest, as before. A Kettle chosen by `--pid` or ancestry is the
  only one tried. Before sending anything, a client checks that the server it
  reached is the process its registry entry names. A server also leaves a
  pointer to its entry where the OS keeps the registry for your account, so a
  client started with a stripped environment still finds it.

- Agent previews (`agent-display`, Settings → Agents → Agent previews, or
  `--agent-display on|off`) let agents show media in Kettle without reading
  the screen or typing. On its own it starts the control server for display
  requests only, which refuses every read with `display_only` and every
  mutation with `read_only`. Turning it on applies at once, also for agents
  already connected; turning it off applies when Kettle restarts.
  `--agent-server off` alone also turns previews off. `kettle ctl get_state`
  reports the policy in force as `policy: {server, display}`.

- Inline media-card rendering foundation: bounded marker capture and registered
  card recognition, owned fallback glyphs, clipped posters, caller labels and
  selection tint in Grid and Legacy text modes. Preview accounts are
  separate from terminal images, and unavailable preview capacity leaves text
  windows usable.

- Kitty graphics capability queries decode the supplied image and echo its
  image id in an immediate success or error reply. They preserve stored images
  and placements, support chunked direct transfers and quiet replies, and
  answer during synchronized output before a following device-attributes reply.

- Video paste receipts identify container content before asking the native
  poster provider. Text or still-image containers named as videos no longer
  show a video card; movies renamed to another supported video suffix still
  work. Header inspection reads at most 64 KiB on the background worker.
- `kettle show PATH` (or stdin, with no path or `-`; `--mermaid` renders
  either as a Mermaid diagram) and the `show` control method send an image,
  SVG or Mermaid diagram to the media shelf of the pane they run in, in the
  Kettle they run inside. They need only Agent previews, never full control.
  Media too large for one request is refused, never cut short, with the
  advice to pass a file path. Media lands
  in the caller's own pane by its process ancestry; a sender Kettle cannot
  place is labeled unverified with the program the system names for it (for
  `kettle show` and `kettle mcp`, the program that ran them) and, on macOS,
  who signed that program's code once macOS validates it.
  Each pane keeps up to eight items: a full shelf lets go of the one viewed
  least recently, never the one its lane shows; `list_panes` reports them.
  Showing the same file again replaces its item in place, and `--key` (an
  agent's `key`) names the item to replace. A push never opens anything on
  screen. Media previews run on macOS and Linux; elsewhere the worker says
  `unsupported_platform`.
- `kettle_show` takes inline Mermaid source (`mermaid`) as well as a file
  `path`, and the full MCP server (`kettle mcp`) offers it too. Its
  instructions follow where the server runs: inside a Kettle pane they say
  when to show media and that showing is not seeing, in tmux what happens to
  the pane, and elsewhere not to call it.
- `kettle mcp --display` offers agents one tool, `kettle_show`, which sends
  an image, SVG or Mermaid diagram file to the media shelf of the pane the agent runs in. It
  needs only Agent previews and cannot read the screen, type or run
  anything.
- Inline cards for the keyboard and screen readers: quick select labels each
  card in the focused pane and opens it when picked, and a screen reader
  hears each card as a button named for its item's title, kind, size and
  sender, which opens it in the pane's preview lane. In English and Spanish.
- Right-click an inline card for its menu: Open, and Open in Preview on
  macOS or Open in Image Viewer on Linux with Eye of GNOME. The viewer gets
  a PNG copy of exactly what Kettle shows, never the bytes a program sent,
  and never through the default app for the file type: Kettle checks it is
  Apple's own Preview, or a root-owned `/usr/bin/eog`, first. Copies are
  private to you, marked as downloaded on macOS, kept to the newest 32 and
  128 MiB (each for at least a minute, so its viewer can read it), and
  deleted when Kettle quits. The preview lane offers the same from a `↗`
  button in its header.
- The first inline card you see with its image shown says "Click to open"
  along its foot for ten seconds, or until you open a card, and then never
  again: Kettle records that it showed in `ui-tips.json` beside the config
  file, so of two Kettles running at once only one shows it. It waits until
  nothing covers the card.
- The pointer turns into a hand over an inline card a click would open, and
  the card gets an accent outline; one that just appeared gets them once it
  has stayed put half a second, even if the pointer does not move.
- Clicking an inline card opens its item in its pane's preview lane. The card takes
  both the press and its release, so the program behind it sees neither. A
  Shift-click selects text as it does anywhere else, and the wheel, like a
  press on a card that came on screen under the pointer less than half a
  second ago, goes where it would without the card.
- Preview lanes: `open_media_shelf` (palette: "Open media shelf";
  `Ctrl+Shift+Cmd+I` on macOS) opens a lane along the bottom of the focused
  pane, about 40% of it, on the item published last, with its title, kind,
  size and sender, and closes it again. The terminal shrinks to make room,
  so nothing is covered, and its program sees the new size; keys still go
  to it. The lane's header browses the shelf (`‹`/`›`), opens the item
  outside (`↗`), collapses the lane to a one-row strip (`▾`) or closes it
  (`×`); a lane with no room for its rows shows as that strip, along the
  pane's bottom whichever side it opened on, and a header too narrow for
  every control keeps close and collapse first. The terminal always keeps
  at least 20 columns and 5 rows; opening a lane in a pane too small even
  for the strip says so instead. Each pane has its own lane; lanes are
  never saved, and leave with their pane or a torn-off tab.
  `preview-lane-side = right` opens lanes beside the terminal instead. A
  screen reader hears each lane, its item's place on the shelf and each of
  its buttons. A pane titlebar marks unopened items as `▣N`, and the window
  title counts them.
- A preview lane can show the source an SVG or Mermaid diagram was rendered
  from (`≡`), scrolled with the wheel; put the picture on another
  background (`◐`), drawing a diagram again for it; and copy the picture, or
  the source exactly as it was read (`⧉`); and read an item's file again
  (`↻`), replacing it in place. Also as `preview_source`, `preview_canvas`,
  `preview_copy` and `preview_reload`. In English and Spanish.
- Zoom and pan a preview: `−` and `+` in the lane's header, a pinch, or
  Cmd+wheel (Ctrl+wheel off macOS) at the pointer zoom the picture, the
  wheel or a drag pans it, and `⤢` fits it again. Also as
  `preview_zoom_in`, `preview_zoom_out` and `preview_fit`.
- An agent's Mermaid diagram card shows the new picture when its preview
  lane draws it again on another background.
- Preview a diagram an agent printed: `/copy` it in Claude Code or Codex and
  use `render_clipboard_as_diagram`, or select the reply and use
  `render_selection_as_diagram` (also on right-click), and the lane renders
  it, with the agent's labels, notices and wrapping taken back off.
- Markdown diagram galleries: `kettle show notes.md`, an agent's
  `kettle_show`, a link or quick select opens a Markdown file's Mermaid
  diagrams (up to 32, each up to 64 KiB, in a file up to 1 MiB) as one
  gallery, as do several diagrams copied or selected together. The lane's
  detail line pages it with `‹` and `›`, as do Page Up and Page Down in a
  focused lane and `preview_previous_diagram` and `preview_next_diagram`;
  every page comes from the file as it was read, and `↻` reads it again at
  the diagram shown. The `show` control method's `markdown_index` opens a
  file's gallery at a page. In English and Spanish.
- `focus_preview` ("Focus preview") gives a preview lane the keyboard until
  Esc: arrows, `+`, `-`, `0` and `c` move, zoom, fit and copy, and nothing
  else typed reaches the program in the pane.
- Drag a preview lane's edge to resize it; the terminal keeps at least 20
  columns and 5 rows, and its program sees the new size as you drag.
- A zoomed preview sharpens a moment after it stops moving: the part in view
  is drawn again at the size shown, from its file only when you moved it
  yourself and not once the file changed.
- Preview a file without an agent: `preview_link` (palette: "Preview a file
  in Kettle") labels the image, SVG and Mermaid files in the focused pane,
  and typing a label renders that file in its pane's lane; right-clicking a
  link to one offers "Preview in Kettle". A link from another machine's pane is
  refused and one from behind tmux, screen or zellij is asked about first,
  as opening it is. Your preview goes ahead of anything agents are
  showing, and a notification says why when one cannot open. In English
  and Spanish.
- Preview a copied file: `preview_clipboard_path` (palette: "Preview the
  copied file in Kettle") opens the image, SVG or Mermaid file you copied
  in a file manager, or whose path or `file://` link you copied, in the
  focused pane's lane. Shift+right-click offers it as "Preview Copied File".
  `preview_next`, `preview_previous` and `close_preview` browse and close
  the focused pane's lane from the keyboard or the palette.
- Shift+right-click on selected text opens the menu instead of extending
  the selection; off the selection it still extends it. With nothing
  selected it opens the menu too, mouse reporting or not: a plain click
  used to leave an empty selection that Shift+right-click silently
  extended.
- The media worker can classify a file by its content, raster or SVG,
  whatever it is called, and says what it rendered. Kettle and its worker now
  speak media protocol 7; a worker from an older build is refused until
  Kettle restarts, as before.
- `kettle-media`, a new crate, defines the bounded media protocol for the
  agent visuals: jobs and results, caps, source authorization, the
  build handshake and the binary frames between Kettle and a media worker.
- `kettle ctl get_state` reports `media`: whether media previews are
  available, and a fixed reason when not. Kettle looks for its media worker
  only beside its own executable, never in `PATH` or the working directory,
  and checks the file and, on macOS, its code signature before trusting it.
  Missing workers read `worker_missing`; installed workers that pass their
  checks read `available`. Kettle and its workers
  now share one build identity, a hash of the source they were built from.
- `kettle-media-worker`, the media worker executable, is built with the
  workspace. Before reading anything it closes inherited descriptors, turns
  off core dumps and lowers its resource limits, and a watchdog ends it if
  its parent stalls. Unix packages now ship it beside the terminal. The media
  client can run a job in a fresh worker under startup and job deadlines and
  a 768 MiB limit on the memory the worker and everything it started hold,
  killing the worker's whole process group before reaping it.
- The media worker renders raster images through `kettle-media-render`, a new
  crate in safe code that writes nothing. It reads a file once through one
  open descriptor, refusing anything but a regular file and a file that
  changes while it is read; it takes the format from the content, not the
  name, and decodes PNG, JPEG, WebP, BMP and a GIF's first frame only after
  checking the image's size against the decoded caps. The image is scaled
  down to fit the requested box, never enlarged past its own size, keeping its
  aspect ratio, without transparent pixels bleeding color.
- The media worker renders Mermaid diagrams, every family merman 0.8.0
  pins, from flowcharts and sequences to gantt charts, mind maps and
  timelines: `kettle show diagram.mmd`, a small one on stdin, or an agent's
  `kettle_show` puts the rendered diagram on the shelf, in the pane's colors.
  Labels are measured and drawn with the same fonts, bundled Fira Sans for
  text, and the diagram's SVG goes through the same checks as any other SVG.
- The media worker renders SVG with resvg. The document is parsed with no
  DTD, scripts and foreign content are dropped, its style sheets (simple
  selectors, only from retained subtrees) are applied and written as
  attributes, and every reference
  outside the document (files, network, data URLs) is removed before resvg
  sees it;
  resvg's own resolvers load nothing either. A document whose references
  would expand past a million units, nest deeper than 256, or make resvg
  allocate more than four million pixels of layers, filter results, masks,
  clips or pattern tiles is refused before any of it is allocated. Filter
  input-name copies count toward expanded work, and every merge input and
  both inputs of blend, composite and displacement filters count toward the
  layer budget. SVG
  results are at most 1024 pixels a side and a million pixels, and text is
  drawn with the bundled JetBrains Mono face by default. A job can explicitly
  supply up to eight regular outline font files, 32 MiB each and 128 MiB total,
  preserving a selected TTC face index. Each job has its own font database;
  no host fonts are discovered, and embedded SVG, color and bitmap glyph
  formats are refused. Results report actual font use and missing scripts
  through `FontFallback`, `MissingGlyphs` and `uncovered_scripts`.
- Unix packaging builds `kettle-media-worker` separately with the unwinding
  `media-worker` profile. Linux installers and package-manager templates install
  both executables; updates journal the pair. The first restart after a Linux
  4.9-to-5.0 update installs the worker from packaged compatibility data through
  the same recovery journal. macOS updates exchange the whole paired bundle.
- `kettle ctl dispatch_ui_key` drives quick-select hint mode too, in the order
  the keyboard reaches it: right after a confirmation.
- In quick-select hint mode (`Ctrl+Shift+H`), holding Shift while typing a
  label opens a path in the default app and copies a URL. A plain label still
  opens a URL and copies everything else. A path resolves from that pane's
  directory; it is copied instead when it names another machine's file (an
  ssh, mosh or container session), when the label covers only part of a name,
  or when it climbs with `..`, and behind tmux, screen or zellij Kettle asks
  first. On Windows path hints are copied.
- `language` (`auto`, `en`, `es`; Settings → Behavior → Language) chooses the
  language of Kettle's own text: Settings, menus, the macOS menu bar's
  application menu and the Dock menu, prompts, notifications and
  screen-reader names. The Linux desktop entry has Spanish too. The CLI's
  output stays English, byte for byte, whatever the language. `auto`, the default, follows the
  operating system's locale. A change applies when Kettle restarts. The
  Spanish is neutral Latin American, reviewed in context on the rendered
  Spanish build.
- A theme picker: type to filter the 500+ bundled themes by name, or move
  through them with the arrows, and the window previews the selected theme.
  Enter keeps it and Esc puts back the theme you started with. With nothing
  typed it lists the current theme's appearance first, each theme followed by
  the most similar one, and tags each row dark or light. Open it from the
  Settings Theme row (Enter or a click), right-click → Theme…, the command
  palette's "Choose theme…", or the bindable `open_theme_picker` action.
- `kettle video-frames clip.webm -o sheet.jpg` and the full MCP server's
  `kettle_video_frames` return a contact sheet of a video's frames for a
  model to read, each labeled with its time, with an index of the times.
  Kettle reads GIF, APNG and animated WebP itself, and on macOS MP4 and
  QuickTime through AVFoundation; any other video needs your own ffmpeg and
  ffprobe, which Kettle finds only in fixed places, trusts only when no one
  else could have changed them, and runs contained, with a deadline. Kettle
  never installs them. The display server never offers the tool. `--start`
  and `--end` narrow the window, `--at` takes one frame, `--count` takes 1
  to 16 (9 by default), and `--max-edge` sets the sheet's longer edge, up to
  2560 pixels (1568 by default).
- Agents and `kettle show` can show a video: it lands as its poster, the
  middle frame, with a play glyph and its length on its card, and the lane,
  `kettle show` and `kettle_show`'s result say its size, codec, length and
  sound. It plays in its lane when you play it, and the model has still not
  seen it. Quick select and the menus offer common video files for
  a preview.
  Without a decoder for it, the notice says to install ffmpeg.
- The media worker confines itself before it reads a job: to the job's own
  files, a trusted video decoder's own programs, and nothing to write or
  connect to (Landlock and seccomp on Linux, a Seatbelt profile on macOS).
  Kettle decodes video only inside it; on a Linux kernel without Landlock,
  videos are not shown or read while images and diagrams still are.
- A video card's menu, and the lane's open button, open the video in
  QuickTime Player on macOS or mpv on Linux (`/usr/bin/mpv` only), never your
  default application, and only in a format that player plays. The player
  gets a private copy of the file Kettle showed, checked to still be that
  file: a clone where the disk allows, otherwise a copy that leaves 2 GiB
  free, owner-only, quarantined on macOS, up to 4 GiB, and deleted when
  Kettle exits. It opens paused.
- A video plays in its lane when you press its `▶` (Space while the lane has
  the keyboard, or the `video_play` action): every frame at its time, up to
  1080p and 30 frames a second, decoded in a sandboxed worker of its own.
  On macOS, AVFoundation plays MP4 and QuickTime with their sound, from that
  worker's own output, each frame kept to the sound as you hear it (the
  output device's delay, Bluetooth's included, allowed for); `♪` mutes it
  (M while the lane has the keyboard, or `video_mute`), across seeks, until
  you unmute it. Otherwise your ffmpeg plays the picture only. The footer
  says when a video plays muted, or why it plays without sound. `‖` pauses it; the bar under the picture shows where it is and
  moves it there when you press or drag along it; Left and Right step it
  five seconds while the lane has the keyboard; screen readers get the bar
  as a slider. It pauses when its lane is hidden, until you play it again,
  holds its last frame at the end, and at most two lanes play at once (the
    one played least recently waits on its frame). It never starts on its
  own, for a control client or a Lua script, or for a file that changed; a
  control client or a Lua script may pause or mute it, never play, move or
  unmute it.
- The `preview_play` action plays a video's silent preview instead: eight
  frames from across the video, shown in turn, each for an eighth of its
  length, at most twelve a second and at least two, until `■` stops it. The sandboxed worker makes them within two seconds, or the
  poster stays and the lane says why. With `video-preview-hover = on`, the
  pointer resting on the picture loops a preview you played for up to four
  seconds, unless `reduce-motion` says otherwise: `auto`, the default,
  follows macOS's Reduce Motion and holds the hover preview back on Linux,
  where the system does not say; `off` allows it there. It never starts on its own, never for a control client or a Lua
  script, and never makes a sound.
- `kettle ctl get_state` says which video decoders a worker uses, under
  `media.video`: AVFoundation then your ffmpeg on macOS, your ffmpeg on
  Linux, and that no GStreamer is used.

### Changed

- A pasted or dropped video's poster is made by the sandboxed media worker on
  macOS and Linux, the same way a shown video's is, from the exact file the
  receipt checked; Kettle itself decodes nothing. macOS no longer asks Quick
  Look, so WebM and other videos AVFoundation cannot read get a poster when
  ffmpeg is installed, and a generic receipt otherwise. On Linux the poster
  now comes from your ffmpeg when it is installed; without one, or for a
  video it cannot read, the thumbnail your file manager cached is still used
  when it matches the file, read by the worker as any image is (so also
  without the sandbox) rather than by Kettle.

- JSON objects in Kettle's control, MCP and other JSON replies keep their
  keys in the order Kettle writes them, in every build, rather than
  alphabetically. A JSON reader sees no difference; a script that compared
  reply text byte for byte may. `kettle exec --json` events keep their bytes.
- The control server checks each request's permission on its connection
  thread before anything else, so a refused request does no work and never
  reaches the window. A refusal now comes before parameter errors, a
  `subscribe` that fails no longer switches the connection to the event
  stream, and a request a `read-only` server refuses reads "This connection
  cannot perform control mutations." instead of naming a config value.

- Each media render attempt encodes its job without first copying the
  source, which can be a 32 MiB image, so a render no longer holds two copies
  of it while starting the worker.

- Building Kettle from source now needs Rust 1.95 or newer (was 1.89). The
  `msrv` CI job, the Nix toolchain and the build docs follow the
  `rust-version` in `Cargo.toml`. `sysinfo` moves to 0.39, whose releases
  need 1.95; the process walk behind tab titles and remote detection asks it
  only for parents, command lines and working directories, never thread
  lists.
- Image rendering reuses retired textures for same-size replacements without
  reserving a second texture, preserves every image needed later in the frame,
  and releases decoded CPU pixels independently of the GPU cache.
- A clicked file link or path in a pane connected to another machine no
  longer opens the local file of the same name: Kettle says it names a file
  there instead. That covers a detected ssh, mosh or container session and a
  remote client in the pane, also when run through `sh -c`, `env` or `sudo`.
  Behind tmux, screen or zellij, which may or may not be on this computer,
  Kettle asks before opening it. Web and mail links open as before.
- Kettle no longer opens programs or shortcuts from terminal links: a
  clicked `file://` link, OSC 8 link or path to an `.exe`, `.app`, `.lnk`,
  `.desktop`, `.rdp`, script, executable file without a document extension
  (`a.out`), macOS alias or Linux launcher is refused with a notification
  instead of being run. A symlink is judged by what it points at, a name is
  read as Windows reads it (`payload.exe.`), and a configured custom URL
  handler gets a file link only after the check. Documents and folders open
  as before, though a file link's `?query` or `#fragment` is no longer passed
  on, and one with an encoded `/` or `\` (`%2F`, `%5C`) is refused.
- `TRANSLATING.md` explains how Kettle's text is translated and reviewed, and
  each Spanish message's review status is recorded by key. Every Spanish
  message has been reviewed in context on the rendered Spanish build and
  accepted.
- The tab bar's numbered fallback label (`tab 2`) and an untitled pane's
  screen-reader name follow the UI language. `kettle ctl` tab titles keep the
  English `tab 2`.
- The right-click menu's Theme submenu, a list of every bundled theme, is now
  a Theme… row that opens the theme picker.
- Screen readers read a picker row's hint after its name, such as a
  command's shortcut or a theme's appearance.
- Stepping through themes (`next_theme`, `prev_theme`, and ←/→ on the Settings
  Theme row) stays with the current theme's appearance and follows an order
  of likeness, judged by background, foreground and accent; the Settings row
  steps through the popular themes in that order. It used to follow the
  themes' names or the popular list's order, flipping between light and dark
  palettes. `toggle_light_dark` still switches appearance.
- Close confirmations name the tab or window and count in words that match
  the number: "Close tab with 1 pane?", "Close window with 3 panes?" rather
  than "Close 3 panes?". The paste prompt says "1 line" or "2 lines". They
  used to say "pane(s)" and "lines" whatever the count.

### Fixed

- `login-shell = true` with no `command` no longer stops Kettle at launch on
  macOS and Linux. It added `-l` to your default shell, which takes no
  arguments there, and the pane's spawn panicked. Your default shell already
  starts as a login shell; `login-shell` now adds `-l` only to a shell a
  `command` names.
- `kettle mcp` lists its tools to clients on MCP 2026-07-28, which include
  current Claude Code. Its tool list left out the `ttlMs` and `cacheScope`
  fields that revision requires on every list result, so those clients
  rejected the list and showed Kettle's server with no tools.
- `kettle ctl send_mouse` with `button` `back` or `forward` reaches a
  mouse-tracking program as the physical Back and Forward buttons do. It used
  to send xterm's button numbers 8 and 9 as report codes, which read as a
  left or middle press with Meta held.

- A mouse Back or Forward release reaches a mouse-tracking program only after
  a press it received. A press that dismissed a context menu or that a dialog
  took used to leave the program a release with no press, and one it received
  lost its release when a dialog opened while the button was held.

- Every mouse press a program receives gets exactly one release, in its own
  pane. A button held while focus moves to another pane or tab keeps its drag
  and release with the program that got the press; the release used to go to
  the newly focused pane, leaving the first program a press with no release
  and handing the other a release with no press. Two buttons held at once
  each keep their own, and Back and Forward follow the same rule. When Kettle
  ends a drag itself (the window loses focus, search or a menu opens, the
  pane turns read-only or is reset, or the tab moves to another window), the
  program gets its release at the last cell it saw instead of waiting
  forever. Pressing Shift partway through no longer drops the release, a
  press the program never got (its input was full) leaves no release behind,
  and a release always gets through: each pane's input keeps a little room
  that only mouse releases may use. The newest press owns the drag, so a
  Shift-drag selection started while a program holds Back still selects.

- `kettle ctl`, `kettle mcp` and other control clients keep finding running
  servers after many Kettle sessions. Each exit used to leave its control
  socket behind; once more than about a thousand piled up, discovery stopped
  before reaching live servers' entries and reported that no server was
  running. Servers now unlink their socket on exit, pruning a dead server
  removes its socket, and a starting server clears leftovers in the registry
  directory, including those from earlier releases. Leftover sockets no
  longer count toward the 1,024 entries a discovery reads, within a walk of at
  most 8,192 directory entries.

- A paste receipt and a pending video preview move with their tab when it is
  torn off, moved to a new window or docked into another window, keeping
  their pixels and remaining time. They used to stay in the old window, and a
  video poster that finished after the move was dropped. A failed move puts
  them back. Docking replaces only the part that arrived, so a receipt or a
  pending preview that belongs to another tab in the target window stays,
  and a preview that fails no longer removes another tab's receipt.

- Dragging a tab onto another window docks where that window's tab bar is
  painted. A bottom bar's docking band ignored the search bar and a bottom
  status bar, and a side strip's band ran under the search bar. The split
  preview while dragging a pane now matches the pane the split creates,
  rounded the same way.

- In Spanish, the pasted-path receipt titles said "pasted image" and "pasted
  video" where the card shows a pasted path; they now say "Ruta de imagen"
  and "Ruta de video". "Ocultar el puntero al escribir" replaces "ratón",
  since the setting also hides the trackpad pointer.

- Automatic window accents follow a palette change that keeps the theme's
  name, such as a config reload that edits one palette color. Accents were
  re-resolved only when the theme name changed, so windows kept their old
  colors and a tab torn off afterwards could take its source window's color.
  A window whose accent changed while its renderer was being rebuilt after a
  GPU failure also keeps the new accent once the rebuild finishes, instead of
  going back to the old one.

- An opaque context menu, Settings panel or paste receipt over the cursor
  cell now hides the whole cursor. The inverted glyph of a focused block
  cursor was drawn after every overlay and showed through them.

- On macOS, a window move reported while Kettle is handling a key or other
  non-mouse event no longer aborts it. The caption-drag check read mouse-only
  event fields, which AppKit refuses with an exception for those events.

- Kitty transmission and placement commands return completion replies after
  actual image storage or placement admission, including synchronized output.
  Chunked replies retain the original ids and quiet settings. Missing images,
  missing parent placements, invalid data, and refused storage return errors.
  Anonymous images remain silent. Refused relative replacements keep the previous
  placement, accepted relative definitions survive reflow during admission, and
  combined uploads preserve their relative destination. Zero identifiers are
  treated as absent, unrecognized actions cannot become uploads, and a delete
  refused for conflicting identifiers leaves partial uploads intact.

- Kitty animation frame uploads and composition return completion replies after
  refreshing animation state. Uploads report the actual one-based frame number.
  Missing frames and invalid composition rectangles leave existing pixels intact;
  zero composition dimensions use the full source canvas. Partial appended frames
  use a transparent canvas unless a background color or existing frame is selected.
  Animation controls require an existing root and preserve the current frame
  when given an invalid selector.

- Torn-off windows reserve automatic accent colors in process even when the
  optional presence registry is unavailable. Closing a window frees its color
  for reuse. When the palette is full, a torn window avoids the original
  window's color if another hue exists; reuse favors the least-used hue.
  Pinned colors retain their configured behavior.
- On macOS, dragging a released single-tab window by its tab uses manual
  follow and keeps the original press point under the pointer. A fast drag
  processes its first movement immediately, allowing that same gesture to
  latch a sibling window's insertion target before release.
- Native macOS caption drags of a single-tab window use the same rejoin path
  when its automatic tab bar is hidden, and holding Escape before release
  cancels the rejoin. Desktop-to-client docking coordinates account for each
  target window's display scale.

- New Kitty image ids remain addressable at image-count and retained-byte limits
  by reclaiming eligible old images, with unplaced images first. Pixel snapshots
  keep their charge, and active count pressure preserves the other screen's roots.

- Kitty relative placements select the exact parent named by `Q=`, including
  chained and virtual placements. Rendering and spatial deletion use the same
  origins; a missing explicit parent cannot fall back to another placement.

- Kitty image-id retransmission retires old placements and animation data with
  the first accepted chunk, releasing their pixel leases before replacement
  decoding. Unrelated uploads and matching ids on the other screen survive.
  Self-composition releases its temporary source handle before editing pixels.

- Kitty frame composition uses the correct source and destination offsets.
  Newly appended animation frames default to 40 ms; edits with omitted or zero
  delay preserve the existing timing.

- Kitty root-frame edits refresh existing physical, virtual, and relative
  placements, including synchronized output, while preserving placement
  geometry, animation timing, and independent primary/alternate screen images.

- Kitty animation edits work when the old GPU cache still has a weak reference
  to the pixels. They transfer the buffer without copying it; retained pixel
  snapshots keep their memory charge until their last handle is released.

- Partial Kitty animation frames keep the full image canvas size. If the canvas
  cannot fit in the current memory quota, the frame is refused without changing
  the image; retrying after memory is released works normally.

- Small encoded terminal images can load when the remaining image quota holds
  their decoded pixels. They no longer need room for the maximum image size.
  An already-RGBA8 decode also avoids a redundant copy of its pixels.

- macOS release packaging works with the system Bash when assembling command
  arguments for ordinary binaries and the media worker. It signs nested binaries
  before the app bundle so the new helper does not block packaging.

- On macOS, a video preview helper that exits before reading its request can
  no longer close Kettle. A write to a pipe nobody reads raises SIGPIPE on the
  whole process there, so blocking it on the writing thread did not help; the
  pipe is now marked to fail the write quietly instead.
- On Linux, a video poster that tumbler (Xfce's thumbnailer) cached is used:
  it records the file's modification time with a fraction of a second, which
  Kettle read as stale. A different time or file still does not match.
- A video paste after an update replaced Kettle no longer asks the new build's
  preview helper to read the old build's request. On Linux the helper is the
  running Kettle itself; elsewhere a helper from another build says so, and
  Kettle shows no video card and logs once that previews wait for a restart.
- Opening the current directory in the file manager works for a folder whose
  name holds a space or `#`. The link was refused, or named another folder.
- A `file://` link or a reported working directory (OSC 7) is checked after
  its escapes are decoded: `.%2e`, `%2F%2Fhost`, `%00` and other spellings of
  a climb, a network share or a control character are refused, as are a bad
  escape in a link and an encoded `/` (or `\` on Windows). They could slip
  past checks that only looked at the encoded text. A local WSL share is
  still accepted as a working directory.
- A path in quotes or backticks is one link and one quick-select target,
  spaces included: `"docs/annual report.pdf"`, Python's `File "/my app/x.py"`
  and `` `out/report #1.pdf` `` open the whole file. They used to stop at the
  first space or `#`. Quoted prose such as `"see src/main.rs"` still links
  only the path inside it.
- A URL in Markdown code or emphasis (`` `https://…` ``, `**https://…**`) is
  linked and quick-selected without the trailing backtick or asterisks.
- A URL or path the terminal wrapped onto the next row is one link and one
  quick-select target: clicking either row opens the whole URL, and hovering
  lights both. It used to be two broken links. A program's own line breaks
  are never joined, and a URL running off the visible rows is not linked until
  all of it is in view.
- A path that begins inside a longer token is no longer a link or a
  quick-select target: `foo(1)/bar.png` held a clickable `/bar.png`, which
  opened an unrelated file. Paths after a space, a bracket, `=`, or a list,
  chain or redirect separator (`PATH=/a:/b`, `&&/usr/bin/x`, `>/tmp/out`) are
  unchanged.
- Quick-select hints and double-click selection take a relative path whole:
  `out/diagram.png`, not `/diagram.png`, which named a different file. A
  drive-letter path (`C:/out/diagram.png`) is taken whole too, and an IPv4
  network such as `10.0.0.1/24` stays an address rather than a path.
- A compact paste receipt in a remote pane shows its whole warning, "Remote ·
  local path". It was cut to "Remote · local path o…".
- A key that closes or replaces a modal (the command palette, a picker, a
  title editor, the Settings path prompt) ends an input-method composition in
  progress there, so a late commit can no longer type into the terminal or the
  next modal.
- Dragging a pane onto another shows where it will land with the search bar
  closed. The drop preview appeared only while search was open.
- In the window, tab, pane and group title editors, the input method's
  candidate window opens at the caret. It opened two or three columns to the
  caret's left.
- A Settings keybind row for an action with more than one shortcut shows the
  same shortcut on every launch, the one the command palette and menus show.
  It used to show any of them, and could change between launches.

## Older releases

- [4.x](docs/changelog/CHANGELOG-4.x.md)
- [3.x](docs/changelog/CHANGELOG-3.x.md)
- [2.x](docs/changelog/CHANGELOG-2.x.md)
- [1.x](docs/changelog/CHANGELOG-1.x.md)
- [0.x](docs/changelog/CHANGELOG-0.x.md)
