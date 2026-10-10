# Changelog

All notable changes to kettle. Format roughly follows
[Keep a Changelog](https://keepachangelog.com/); the project moves in small,
durable, fully-tested cycles (lint · build · test · docs · commit · CI).

## [Unreleased]

### Added

- Inline media cards for Claude Code: `kettle mcp --display`, launched by
  Kettle's Claude Code plugin, asks for a card under the `kettle_show` call,
  and Kettle registers one only for Claude Code under Anthropic's signature
  running in the pane the item lands in. Kettle builds the card's cells and
  caption; the plugin's hook collects them once and prints them under the
  call, where Kettle paints the image. The model is told only that the media
  shows below the call.
- `kettle agent-setup --print` prints a `codex` shell function to review and
  add to your shell's startup file. In a Kettle pane, an interactive Codex
  session started through it (fresh, `codex resume` or `codex fork`) gets
  this Kettle's display server for that launch only, so `kettle_show` sends
  media to the pane's shelf. Every other Codex command runs unchanged, and
  Kettle edits neither the shell's startup files nor Codex's configuration.
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
  selection tint in Grid and Legacy text modes. Registrations remain test-only;
  production display callers are introduced separately. Preview accounts are
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
  Each pane keeps its last eight items; `list_panes` reports them. A push
  never opens anything on screen.
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
- An agent's Mermaid diagram keeps its inline card's picture when its
  preview lane draws it on another background.
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
  speak media protocol 3; a worker from an older build is refused until
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
  aspect ratio, without transparent pixels bleeding color. Video rendering
  remains later work; GUI preview
  callers are introduced separately.
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
  Raster and SVG are internal services; preview UI and public display callers
  are introduced separately.
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
  language of Kettle's own text: Settings, menus, the Dock menu, prompts,
  notifications and screen-reader names. `auto`, the default, follows the
  operating system's locale. A change applies when Kettle restarts. The
  Spanish text is a first draft awaiting review.
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
  never installs them. The display server never offers the tool.
- Agents and `kettle show` can show a video: it lands as its poster, the
  middle frame, with a play glyph and its length on its card, and the lane,
  `kettle show` and `kettle_show`'s result say its size, codec, length and
  sound. Nothing plays inside Kettle yet, and the model has still not seen
  it. Quick select and the menus offer common video files for a preview.
  Without a decoder for it, the notice says to install ffmpeg.
- A video card's menu, and the lane's open button, open the video in
  QuickTime Player on macOS or mpv on Linux (`/usr/bin/mpv` only), never your
  default application, and only in a format that player plays. The player
  gets a private copy of the file Kettle showed, checked to still be that
  file: a clone where the disk allows, otherwise a copy that leaves 2 GiB
  free, owner-only, quarantined on macOS, up to 4 GiB, and deleted when
  Kettle exits. It opens paused.

### Changed

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
  each Spanish message's review status is recorded by key. The Spanish
  catalogue is a draft until a fluent reviewer accepts it.
- The tab bar's numbered fallback label (`tab 2`) and an untitled pane's
  screen-reader name follow the UI language. `kettle ctl` tab titles keep the
  English `tab 2`.
- The right-click menu's Theme submenu, a list of every bundled theme, is now
  a Theme… row that opens the theme picker.
- Screen readers read a picker row's hint after its name, such as a
  command's shortcut or a theme's appearance.
- Stepping through themes (`next_theme`, `prev_theme`, and ←/→ on the Settings
  Theme row) stays with the current theme's appearance and moves to the most
  similar theme each time, judged by background, foreground and accent. It used
  to follow the themes' names or the popular list's order, flipping between
  light and dark palettes. `toggle_light_dark` still switches appearance.
- Close confirmations count in words that match the number: "Close tab with 1
  pane?", "Close window with 3 panes?". The paste prompt says "1 line" or "2 lines". They
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

- The window close confirmation names the window: "Close window with 3
  panes?" rather than "Close 3 panes?". In Spanish, the pasted-path receipt
  titles said "pasted image" and "pasted video" where the card shows a pasted
  path; they now say "Ruta de imagen" and "Ruta de video". "Ocultar el
  puntero al escribir" replaces "ratón", since the setting also hides the
  trackpad pointer.

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

## [4.9.0] — 2026-10-02

### Added

- `RUST_LOG=warn,kettle::startup=info` prints how long each startup phase
  took, from `main` to the first frame.
- `keybind-yield` (`auto` by default) decides which default chords a program
  in the pane gets while it owns the keyboard; `off` keeps every default chord
  Kettle's, as in 4.8.
- Kettle answers XTVERSION (`kettle(<version>)`), DECXCPR, DECRQSS (SGR,
  scroll region, cursor shape) and XTGETTCAP (truecolor, styled and coloured
  underlines, cursor shapes, palette size), and DECRQM for modes 47 and 1047.
  Claude Code now draws with synchronized output in Kettle, and Neovim finds
  truecolor without `COLORTERM` (over ssh) and draws undercurls as curls.
- Programs can follow Kettle's light or dark theme: `CSI ? 996 n` answers
  the scheme, and DEC mode 2031 reports every change of the theme's colours,
  whether from `theme-mode = auto`, `toggle_light_dark` or a theme picked by
  hand. Claude Code's automatic theme switches with Kettle.
- `macos-cursor-blink-layer` (`true` by default, macOS only): once a window has
  drawn nothing but its cursor blink for half a blink period, the window server
  blinks the cursor and Kettle stops drawing until something changes. `false`
  keeps redrawing the window at every blink. `ui_geometry` reports who draws the
  blink under `cursor_blink`.

### Changed

- On macOS, an eligible first shell starts before AppKit finishes launching,
  sized from the measured font cell to the exact configured grid. Restored
  sessions and later windows keep their existing startup paths.
- Reading `ui_geometry` and closing a control connection that attached no pane
  no longer request a frame. Polling geometry no longer forces a repaint on any
  platform. Agents and resize probes must request an action when they need one.
- Settings use consistent display text and unit spacing, stable label columns,
  separate label/value ellipses in narrow panels, evenly spaced category names,
  and contextual dependency and timing notes.
- Recolouring trailing blank cells, such as a reverse-video block or a
  prompt's padding, no longer reshapes the row's text. Rows shape through
  their last non-blank cell, with face-matched padding to preserve baselines.
- A blinking cursor no longer re-uploads the window's shapes or re-prepares its
  text on each blink; the blink only changes what is drawn.
- A window whose content is not changing writes nothing to the GPU, a
  blinking cursor included. On macOS the driver then releases its blit pool:
  3 s after a burst of output Kettle's memory is 236 MiB instead of 365 MiB
  (10 paired rounds on an Apple M5 Max, B/A 0.647, 95% CI 0.645-0.653). While
  any frame draws, the render pool (about 168 MiB) stays.
- On macOS and Linux GPUs that share memory with the CPU (Apple silicon, and
  integrated or software Vulkan adapters), Kettle writes the quads and glyphs
  that change in a frame straight into memory the GPU reads, instead of
  copying them through a staging upload. Elsewhere uploads work as before.
  Output in the default grid text mode no longer re-prepares the window's
  unchanged chrome text.
- Kettle finds and loads its fonts on a thread while the event loop starts,
  instead of on the main thread before the first window and shell. The first
  window waits only for whatever font work is still unfinished when it opens.
  New windows (`Cmd+N`) load fonts as before.
- A new window builds each distinct quad and image GPU pipeline once, instead
  of once per layer, and on macOS no longer decodes the window icon, which
  macOS ignores. Both were startup work with no effect.
- `Shift+Arrow` reaches a program that owns the keyboard (the alternate
  screen or mouse reporting, or the kitty keyboard protocol or
  `modifyOtherKeys` away from the shell prompt) instead of resizing a split,
  so Codex's `Shift+Left` ("shift+← to answer" a queued question) and
  `Shift+Right`, and Neovim's `Shift+Arrow` motions, work. Jump to prompt,
  scrolling, `Shift+Home/End` and tab switching reach such a program when
  Kettle's action would do nothing and the view is not scrolled back. At a
  shell prompt, with broadcast input on, and for chords you bind yourself,
  nothing changes.
- `Ctrl+Shift+-` and `Ctrl+Shift+_` no longer shrink the font, so `Ctrl+_`
  reaches the program (undo in Claude Code, zsh, bash and emacs). `Ctrl+-` and
  `Cmd+-` still shrink it.
- When focus has nowhere to go (a zoomed split, which shows only its focused
  pane, a tab with one pane, or no pane on that side), the pane focus chords
  `Ctrl+Shift+N/P`, and `Cmd+Opt+Arrow` and `Ctrl+Cmd+Arrow` on macOS, reach a
  program that owns the keyboard, if its keyboard protocol sends the chord as
  itself (the kitty protocol, or `modifyOtherKeys` level 2 for
  `Ctrl+Shift+N/P`). `Alt+Arrow` on Linux and Windows already did.

### Fixed

- A scheduled light/dark theme (`theme-schedule`) now switches at its time even
  while the window draws nothing, such as an idle window, or one whose cursor
  the window server blinks. Before, it waited for the next keystroke, output
  or other repaint.
- A program that asked for focus reports (DEC mode 1004: tmux with
  `focus-events`, Neovim's `FocusGained`/`FocusLost`, Codex and Claude Code)
  hears focus leave its pane when focus moves to another pane or tab, and
  return when it comes back. Kettle used to report only the window gaining or
  losing focus, so a pane in a hidden tab still believed it had the keyboard.
- Underlines under detected paths and URLs stay on their text after a pane
  changes size without new output: zooming a pane, splitting next to it or
  resizing the window. They used to keep the old width's positions, so they
  ran across the wrong words until something was printed.
- A runtime diagnostic incident appears in `<cache>/kettle/diagnostics` only
  once it is complete; a reader could see one empty or half-written.
- A key that closes or replaces one of Kettle's own modals (Enter in the
  command palette, a dialog, the layout picker or the SSH launcher, Escape in
  vi mode or a Settings text field) no longer acts again when held: holding
  Enter to confirm used to send the program behind it an extra Enter per key
  repeat, or launch the first entry of the layout picker the palette had just
  opened, and holding Escape in a Settings text field or a keybind capture
  closed Settings too. The Escape that cancels a pane or tab drag is held
  back the same way. Inside a modal, keys that activate, toggle or pick act
  once per press, so holding Enter no longer runs a context submenu's first
  row, holding Space or Left/Right no longer flips a Settings toggle or cycles
  a choice back and forth (a number still steps), a held hint letter no longer
  completes a doubled label, and holding `v` in vi mode no longer flickers
  visual mode. Moving keys still repeat.
- A cursor position report (`CSI 6 n`) under origin mode counts the row from
  the top margin, as xterm does, so a program that saves the position and
  restores it with CUP lands where it was.
- `Option+Return` on macOS is `Alt+Return`, so it adds a newline in Codex,
  Claude Code and zsh's emacs keymap instead of submitting. Where no keyboard
  mode applies (no kitty protocol, no `modifyOtherKeys` level that encodes it,
  no modified-Enter fallback), `Alt+Return` sends `ESC CR`, as other terminals
  do, instead of a bare CR.
- `Ctrl+Shift+Space` toggles vi mode, in and out; the default never matched a
  real key press, neither did any `space` chord in a config, and vi mode
  took the chord before it could leave.
- Resizing a split while its tab is zoomed no longer moves the hidden panes,
  which then appeared moved on unzoom.
- `Ctrl+Shift+X` on a tab with one pane does nothing. It used to set a hidden
  zoom that the next split silently cleared. Scaled zoom still enlarges the
  font there.
- An `Alt+Arrow` chord you bind yourself stays Kettle's on Linux and Windows
  even with no pane in that direction, as every chord you bind does.
- Decreasing the font at size 5 no longer raises it to 6; the font now
  shrinks to 5, the smallest size the config accepts.
- Holding a toggle chord (vi mode, zoom, fullscreen, broadcast, read-only,
  and the other toggles) toggles once; key repeat used to flip the state back
  and forth for as long as the chord was held.
- A control-socket embedder that reconnects as soon as Kettle closes its
  connection (after the connection sat idle, say) is served. With all eight
  connection places in use, Kettle announced the close before it freed the
  place, and could refuse the reconnect.

## [4.8.0] — 2026-09-28

### Added

- `split_left` and `split_up` (Ghostty's `new_split:left` and
  `new_split:up`) put the new pane left of or above the focused one. They
  have no default keys.
- The right-click menu adds rows from Ghostty's menu: Split Left, Split Up,
  Close Tab, New Window, Close Window, Set Tab Title…, Set Pane Title…, and
  Reset Terminal.
- In the search bar, `Cmd+G` / `Shift+Cmd+G` (`Ctrl+G` / `Ctrl+Shift+G` on
  Linux) step to the next or previous match, and the find shortcut pressed
  again selects the query. Buttons show hover and pressed states.
- The search query has undo and redo (`Cmd+Z` / `Shift+Cmd+Z`, or `Ctrl+Z` /
  `Ctrl+Shift+Z` on Linux). On macOS, `Cmd+Backspace` / `Cmd+Delete` delete to
  the ends of the query, `Ctrl+A` / `E` / `B` / `F` / `D` / `H` work as in any
  macOS text field, and `Ctrl+K` deletes to the end when `vim-menu-nav` is off.

### Changed

- An idle window on macOS uses about 97 % less CPU and wakes about 30 %
  less often. Kettle turns off macOS window restoration ("Resume"), which
  kept saving the app's state while nothing happened. macOS never
  reopened Kettle's windows from that state, since they have no restoration
  class; `restore-session` is the setting that reopens them.

### Fixed

- Text shaping includes an unreleased upstream cosmic-text fix for lines that
  hold several bidi paragraphs, such as right-to-left text after a paragraph
  separator. Kettle carries the fix in a vendored copy until a release has it.
- Kettle's shortcuts work while the search bar is open. A new tab, font zoom,
  tab switching, split focus and the rest did nothing until the bar closed.
  On Linux, `Ctrl+Shift+A` and `Ctrl+Shift+X` split and zoom again instead of
  selecting or cutting the query, and `Alt+1`…`9` switch tabs instead of
  typing a digit.
- The search bar's text is centered in its buttons and editor. Labels sat
  5 px low and flush against the left edge of each button, and the query's
  characters jumped a column whenever the caret moved or focus changed.
- Clicking a search button no longer takes focus from the query, and a
  button now acts on release. Wrap and Invert keep the current match instead
  of jumping back to where search opened, and Case searches again from it.
- CJK and emoji in a search query no longer push the controls after the
  editor out of place, and the caret and selection now sit on the glyphs.
- Space on a focused search button presses it instead of typing a space, and
  a focused button's label is readable on the accent color. Holding Space or
  Enter on a toggle or Close acts once, and the held key no longer types into
  the terminal once the bar closes.
- The search status no longer flashes `Searching…` on every keystroke of a
  query that has no match.
- A search that restarted after output stopped could stay at `Searching…`
  with nothing running when no match was on screen. It now goes on to search
  the rest of the history.
- Reset, Clear Scrollback, and Reset and Clear now act on the terminal.
  They were typing `ESC c` or `CSI 3 J` into the running program, so in zsh
  or bash Reset capitalized a word instead of resetting. Like Terminator's,
  they also work on a read-only pane, since nothing reaches the program.

## [4.7.0] — 2026-09-28

### Changed

- Scrolling inside a scroll region, as `vim`, `less` and status-line TUIs
  do, is up to twice as fast. The region's rows move in one step instead
  of one row at a time.
- An idle window wakes about 0.3 times a second instead of 1.3. The
  event-loop watchdog now sleeps while the loop is idle instead of checking
  every second.
- A confirm bar draws its destructive button, such as Close or Delete, with a
  bold label, so it stands apart from Cancel.
- Output no longer restarts the cursor blink after `cursor-blink-timeout`.
  Only typing, pastes, focus, and settings changes do, as in kitty and
  Alacritty, so a window whose program keeps printing stops redrawing for
  the cursor.
- The first shell starts before the window and GPU are set up, about 50 ms
  sooner. The font is measured first, and the renderer reuses it instead of
  loading the system fonts a second time.
- `window-width` and `window-height` open a window at exactly that grid. A
  120x36 request used to open at 123x35. The default window keeps its size.

### Fixed

- `theme-schedule` clock times and the status-bar clock use local time. Both
  used to run on UTC, so `19:00 dark` switched at 12:00 in California.
- An explicit `font-family` or `font-size` wins over Terminator-style
  `font = Mono 10` wherever the lines sit, and the `font` size is clamped to
  5 to 72 points like `font-size`. `--check-config` now flags an out-of-range
  `font` size.
- A `trigger` pattern's `^` and `$` anchor to each row of output, so a
  pattern like `^Build failed: (.+)$` can match. They used to anchor only to
  the start and end of the whole visible screen.
- `background-animation = off` freezes the starfield. It used to jump forward
  on every repaint from typing or output.
- Quick-select hint labels use the new font after a font-family change. They
  could keep the old font until the labels themselves changed.
- Zooming all panes after a scaled zoom keeps the new size when you leave the
  scaled zoom. It used to snap back to the size from before.
- A mistyped `--profile` is reported with `--gpu-info` and `--config-path`
  even when `--check-update` is also given. The combination used to show the
  default config instead.
- Disabled menu items and shortcut hints are readable: they keep 45% of the
  text color instead of a sixth, about 3:1 contrast on TokyoNight instead of
  1.6:1.
- `scripts/gen-starfield.py` writes a 1280×720 loop that fits Kettle's 128 MiB
  animation cap, so all 32 frames play. At 1920×1080 only 16 loaded and the
  loop jumped halfway. The BACKGROUNDS example `ffmpeg` command fits the cap
  too.
- A Lua plugin refuses to load if Kettle cannot remove the unsafe standard
  functions from its sandbox, as documented. Failures used to be ignored.
- `just install`, `uninstall`, `install-local` and `install-recording` run only
  on Linux. On macOS they ran the Linux installer.

## [4.6.0] — 2026-09-27

### Added

- `cursor-blink-timeout` stops the cursor blink after 10 seconds without
  typing or output, leaving the cursor visible. Set it to `0` to keep the old
  behavior. An idle window then stops redrawing, which on macOS drops its
  memory from about 350 MiB to about 35 MiB and its CPU use to nearly zero.

### Changed

- `kettle exec --json` streams large output 3 to 5 times faster with about 70%
  less CPU, because each event is written to stdout in one call instead of
  about 30.
- On macOS, Kettle reads only each pane's own processes when it checks for
  SSH sessions and shell directories, instead of every process on the
  machine. An idle window with a blinking cursor uses about 80% less CPU.

### Fixed

- `kettle exec` without `--cwd`, and the MCP `kettle_run` tool without a
  `cwd`, now run the command in the current directory as documented. They
  used to run it in your home directory, as did `--cwd DIR` when DIR was
  deleted just as the command started.
- `kettle exec` accepts a working directory whose name is not UTF-8, which
  Linux allows, instead of refusing to start.
- `kettle exec`, and the MCP tools built on it, stream large command output
  about 100 times faster on macOS. 8 MiB used to take 23 seconds.
- `kettle exec --timeout` no longer warns that stdout was not fully delivered
  when every byte was delivered, so the warning now means output was dropped.
  A timed-out `--json` run now always ends with its exit event while stdout
  is being read.
- A command with an enormous argument list in one pane, such as
  `nvim $(git ls-files)`, no longer freezes SSH labels and shell directories
  in every other pane until it exits.
- On macOS, new tabs, splits, and windows no longer pause about 100 ms each
  when Kettle starts from a shell with a high open-file limit, as VS Code and
  other Node-based tools set.

## [4.5.2] — 2026-09-26

### Fixed

- New splits, tabs, and duplicates open where the shell is now, and labels keep
  up. A shell that never reports its directory is read from the OS, which can
  lag a `cd` by about 200 ms.
- Directory reports from local shells are no longer dropped after macOS renames
  the host on a network change.

## [4.5.1] — 2026-09-22

### Fixed

- Clipboard pastes now fall back correctly for empty or non-text Linux selections and preserve images and files on macOS.

## [4.5.0] — 2026-09-20

### Added

- Select, copy, follow links, scroll, and use terminal mouse reporting while
  search stays open. Clicking another split moves the search there. Copy uses
  the query selection when present, otherwise the grid selection.

### Changed

- The visual bell flashes only the ringing pane and fades over 300 ms.
  `bell-flash-intensity` now measures a CIE L\* lightness step instead of blend
  alpha. The default is `0.03`; custom values may need raising. Use
  `0.06`-`0.10` for a stronger flash, `0` to disable, or `1` for solid foreground.
- Fresh windows target a `160x45` cell baseline, fitted within 90% of monitor
  width and 85% of height. Explicit sizes and restored geometry take precedence.
  Set `window-width = 98` and `window-height = 35` for the previous size.

### Fixed

- On Linux, zoomed panes pass `Alt+Arrow` to the terminal. Unzooming restores
  directional focus between visible panes.
- Explicit window sizes now use logical pixels, preserving columns on HiDPI.
- Linux session scans tolerate processes exiting while `/proc` is read.
- Linux timeout cleanup allows delayed stop acknowledgement within its existing
  500 ms budget.
- Update rustls to fix RUSTSEC-2026-0285.

## [4.4.0] — 2026-09-10

### Added

- **Recording retention is configurable.** `record-max-bytes`,
  `record-max-files`, and `record-max-directory-bytes` override the 512 MiB /
  50 file / 5 GiB defaults. Unset keeps the default. `record-max-bytes` has a
  1 KiB floor and `record-max-directory-bytes` a 1 MiB floor, rather than
  accepting any non-zero value: a cast that cannot hold its own header would
  stop the recorder from ever starting, and a bare `500` meant as 500 MB would
  make the next recording delete every completed cast in the directory.

## [4.3.1] — 2026-09-04

### Fixed

- **Linux self-update keeps future changelog archives installable.** The
  updater now installs every manifest-verified
  `docs/changelog/CHANGELOG-<major>.x.md` file instead of stopping at the
  current `3.x` archive. It rejects other names and nested entries before they
  can add an install destination.
- Unsafe terminal URLs no longer echo their untrusted payload into logs.
- Modal overlays now appear in the accessibility tree and move accessibility
  focus to the control that owns keyboard input. Confirmations also stay above
  other bottom-bar overlays.
- Settings scroll to keep the focused row visible in short windows and
  ellipsize long rows in narrow windows.
- Search selection and pointer mapping now account for the painted caret, and
  quick-select preserves paths and URLs containing wide glyphs.
- Large sixel images that fit the configured byte budget are accepted even
  when geometric capacity growth would exceed it; Kitty root-frame edits now
  replace the root instead of appending a duplicate frame.
- Update extraction, activation, config-path, and control-directory edge cases
  now fail safely without unbounded reads, busy loops, relative private paths,
  or divergent ownership checks.

### Changed

- Minimum contrast is cached by transformed color pair for each frame. The
  release diagnostic benchmark reduced a 100,000-cell, four-color workload
  from a 76.93 ms median to 1.21 ms (about 63.7 times faster on the test Mac).
- Glyph-atlas evictions coalesce freed regions in batches, layout-picker
  entries are read once per open, and Kitty deletes skip grid and image-origin
  snapshots when there are no relative placements.

## [4.3.0] — 2026-09-04

### Added

- **macOS directional pane focus now answers `Cmd+Opt+Arrow`.** The chord was
  unbound, so a user arriving from iTerm2 or Ghostty pressed it and got
  nothing: no movement, no error, and no hint that a different chord existed.
  Silent failure is the one kind a user cannot recover from by trying harder.

  A plurality among peer terminals, not a consensus. Ghostty ships
  `super+alt+arrow_left=goto_split:left` and iTerm2 documents the same chord
  for Select Split Pane, but WezTerm uses the portable `Ctrl+Shift+Arrow` and
  kitty ships no directional default at all. Ghostty's and WezTerm's defaults
  were read off the installed binaries; iTerm2's and kitty's come from their
  documentation.

  `Ctrl+Cmd+Arrow` keeps working and is not deprecated. Unbinding it would not
  hand the chord to anything better, because a Cmd-bearing chord has no PTY
  encoding and would simply go dead. `Ctrl+Opt+Arrow` is deliberately left
  alone: Ctrl+Option is the VoiceOver modifier, and VO+Arrow moves the
  VoiceOver cursor. Bare `Option+Arrow` still reaches the shell as word motion.
  Linux and Windows keep Terminator's `Alt+Arrow`.

  iTerm2 and Ghostty also cycle splits with `Cmd+[` / `Cmd+]`, which kettle
  does not adopt. Brackets sit behind Option on the German, French, Italian,
  Spanish and Nordic layouts and macOS reports the modifierless character once
  Command is held, so a character binding is unreachable there; binding the
  physical position instead lands on `+` on German, which is already
  `Cmd++` (increase font size). `Ctrl+Shift+N` / `Ctrl+Shift+P` cycles panes on
  every platform and layout.

  Verified with real keystrokes through the window server, not control-API
  injection: a deterministic 2x2 split, asserting the expected target pane for
  every direction from every pane, 16/16 for the new chord and 16/16 for the
  old one. Every new and extended guard was confirmed to fail against its own
  bug, over six mutations, including one that reproduces a README rewrite
  deleting the pane-focus rows while every test stayed green.

### Changed

- Kitty relative-placement deletion now builds one parent-to-children index
  and walks each stored relation once in both the decoder and terminal
  registries. Deleting the root of a full 256-placement chain no longer
  repeatedly rescans every remaining relation and every removed parent.
- **Windows distribution remains retired while retained code stays in CI.**
  Version 3.3.0 remains the final Windows package. The `windows-latest` leg
  still compiles and tests conditional code, including the CLI, ConPTY,
  PowerShell shell integration, and the retained icon resource, without
  producing an installer or release artifact. Two obsolete ignored `pwsh`
  probes were removed; required native and portable regressions remain.

## [4.2.0] — 2026-09-02

### Added

- **A macOS Dock menu: right-click kettle's Dock icon for New Window and New
  Tab, above the list of open windows.** The menu previously showed only what
  macOS supplies on its own — Options, Show All Windows, Hide, Quit — with no
  way to open a window and not even the window-title list nearly every Mac app
  has.

  Two independent causes. Every row above the system section comes from
  `applicationDockMenu:`, an optional `NSApplicationDelegate` method; there is
  no `NSApplication.dockMenu` property, and the Info.plist route needs a
  compiled nib. winit owns the application delegate, implements exactly two
  methods, and neither is that one — so kettle, which had no delegate code at
  all, contributed nothing. Separately, the open-window list is not free for
  apps with ordinary `NSWindow`s: it is gated on `NSApplication.windowsMenu`
  being non-nil, which winit never sets.

  kettle cannot install a delegate of its own. winit's `ApplicationDelegate::get`
  panics on any other delegate object and runs from the swizzled `sendEvent:`
  and both run-loop observers, so a replacement or forwarding delegate dies
  within milliseconds. The fix builds a runtime subclass of winit's own
  delegate class carrying just `applicationDockMenu:` and isa-swizzles the live
  delegate onto it: a true subclass, no added ivars so the instance size is
  unchanged, nothing overridden, and winit's `isKindOfClass:` check keeps
  passing. Both rows map onto actions kettle already had, so no new action
  exists and the command palette is unaffected. Choosing a row also activates
  the app, without which the new window opened behind whatever was frontmost.

  Drift guards pin the parts that would rot silently: the install call site
  must stay free of `#[cfg]`, both platform arms of the module must exist, the
  AppKit menu features must be declared rather than inherited from winit
  through Cargo feature unification, and the winit version pin is held because
  the subclass depends on a private class name that is not public API. A new
  `just dock-menu-smoke` drives the real Dock through accessibility and clicks
  New Window; the manual right-click is recorded in the appearance gate,
  because the Dock is filtered out of automation screenshots.

## [4.1.0] — 2026-08-28

### Fixed

- **`Opt+Backspace` deletes a word again on macOS, and `Opt+Arrow` moves by
  one.** `macos-option-as-alt` decides whether Option composes text (`⌥e` →
  `´`) or acts as Meta. Kettle applied that decision to every key, so under the
  shipped default (`none`) the Alt bit was stripped before the encoder ran and
  `⌥⌫` arrived as a plain Backspace: one character per press. The `ESC DEL`
  encoding was correct all along and simply never saw the modifier.

  Option composes nothing from Backspace, Delete, an arrow, Home/End, Page
  Up/Down, Insert or an F-key, so the policy no longer masks it for those. They
  carry Alt on every setting and from either Option key, which is the line
  kitty draws too. Keys that do produce text — Enter, Space, Tab, Escape and
  every character key — are untouched, so `⌥e` still composes `´` rather than
  sending `ESC ´`, and nothing gains a stray escape prefix.

  The same mask had also made word editing in Kettle's own search bar
  unreachable: `⌥⌫`, `⌥Delete` and `⌥←`/`⌥→` there now delete and move by word,
  which is what that code was always written to do.

  Verified byte-for-byte against the clients this is used with: zsh, Claude
  Code and Codex CLI all delete the previous word on `ESC DEL` and clear the
  line on `^U`, and tmux passes both through. Neovim is the exception and it is
  not a Kettle one — it leaves `<M-BS>` unmapped, so `⌥⌫` does not word-delete
  there in any terminal; `vim.keymap.set("i", "<M-BS>", "<C-w>")` closes it.
  See [Terminal client compatibility](docs/TERMINAL-CLIENT-COMPATIBILITY.md).

### Added

- **`Cmd+Backspace` deletes to the start of the line on macOS.** Super has no
  legacy terminal encoding at all, so the chord previously reached applications
  as nothing, and no config could fix it: `backspace` was not a bindable
  trigger and no action could send literal bytes. Both now exist. `backspace`
  and `delete` (aliases `bs`, `del`) join the keybind grammar, and the new
  `text:BYTES` action writes a literal byte string to the focused pane as
  though typed — Ghostty's spelling, with `\n` `\r` `\t` `\e` `\a` `\b`
  `\f` `\v` `\0` `\xHH` and `\\` escapes, a 256-byte cap, and `=` written
  `\x3d` because a `keybind` line is split on its last `=`.

  macOS ships `keybind = cmd+backspace = text:\x15`, the `^U` that Ghostty and
  iTerm2's Natural Text Editing preset send. It is a binding rather than a key
  encoding so that it works whatever the client negotiated; in Vim's normal
  mode `^U` scrolls, so `keybind = cmd+backspace=unbind` gives the chord back.
  `Cmd+Left`/`Cmd+Right` are documented as one-line opt-ins rather than
  defaults, because `^A` silently edits the buffer in that same mode.

## [4.0.1] — 2026-08-25

### Fixed

- **The bullet Claude Code prints is a circle again, not a coloured square.**
  `⏺` U+23FA is one cell wide and, per Unicode, renders as text unless a
  variation selector asks otherwise. Kettle drew it from Apple Color Emoji: a
  blue-grey rounded square about two cells wide, covering the space after it.
  Ghostty draws the same character as a plain circle.

  Nothing in the shaping stack consulted `Emoji_Presentation`. cosmic-text takes
  the first family in its cascade whose cmap has the codepoint, and neither the
  bundled JetBrains Mono nor the system text faces have this one, so it reached
  the colour-emoji face by elimination. The width was never wrong; only the face
  was.

  Cells that Unicode renders as text now ask for a monochrome symbol face.
  Emoji that are meant to be colourful are untouched, because they are already
  two cells wide and are excluded by the same rule that selects the text ones. A
  system with no monochrome symbol face installed keeps exactly what it had.

- **Closing a split by typing `exit` now gives its rows back to the pane that
  is left.** Splitting away from a full-screen program, then letting the new
  pane's own shell exit, left the surviving pane's terminal at the size it had
  inside the split. Claude Code kept painting into the top half of a
  full-height pane, still running and still updating, simply convinced the
  terminal was 28 rows instead of 57.

  A pane whose child exits is removed by `Mux::reap`, which prunes it from the
  split tree and promotes its sibling into the whole rectangle. Nothing told
  that sibling's PTY. Because the renderer paints from a live layout, the
  survivor looked correct straight away, which is what made this read as a
  redraw problem rather than a resize one. No `TIOCSWINSZ` went out, so the
  kernel sent no `SIGWINCH`, so the program had no reason to repaint at a new
  size.

  Closing the same split with `Ctrl+Shift+W` always worked, because an explicit
  close runs through the action tail that schedules a resize, and the
  confirm-dialog close had already been fixed for this exact reason once
  before. Reaping was the one close path left without it.

- **Splitting away from an agent no longer produces a pane that vanishes.**
  Splitting clones the focused pane's foreground shell so the new pane lands in
  the same place you were working. A shell was judged interactive by its flags
  alone, so `bash /tmp/hook.sh` counted as one. Agents, git hooks and installers
  spawn helpers in exactly that shape and routinely delete the script straight
  after, so the clone ran a script that was already gone and the pane was reaped
  before it drew. Intermittent, because it depended on what the background
  process scan happened to catch in its last sweep.

  A shell given a script-file operand now counts as running and exiting, the
  same as `-c`. The split falls back to the configured shell, which is somewhere
  to work. The rule applies to the POSIX family, where `sh [options] file` is
  standardized; fish, nu, elvish, xonsh, tcsh and csh keep the flags-only rule
  because their value-taking options would otherwise read as scripts. Within the
  POSIX family, options that take a value are consumed, so
  `bash --rcfile /etc/bashrc` and `zsh -o vi` stay interactive, and `-s` reads
  from stdin so `bash -s worker` does too.

  Confirmed against a live window in both directions. With a `bash <script>`
  helper in the foreground, `list_panes` reported the new pane's argv as exactly
  that script; a 40-cycle split loop reproduced a vanishing pane twice before
  the fix and ran clean after it.

- **A split that fails to start now says so.** Both split actions logged a
  spawn failure at `warn` and carried on. At the default log level that is
  invisible, and since a failed split leaves the layout untouched, all the user
  sees is a keystroke that did nothing. That is also what a pane which spawned
  and immediately died looks like, so the two arrive as the same report. A
  failed split now logs at `error` and raises one desktop notice, matching what
  a failed preference write already did.

## [4.0.0] — 2026-08-24

### Changed

- **Windows distribution support ends with 3.3.0.** Version 3.3.0 is the final
  supported Windows release and keeps its x86_64 archive and installer so the
  end-of-life notice reaches existing clients. Version 4.0.0 removes the
  Windows package, installer, performance harnesses, and signed update target.
  The Windows CI job remains as compile and regression coverage for retained
  conditional code, not as a supported-platform claim.
- **The native Ubuntu ARM test machine now runs under direct QEMU/HVF.** The
  migrated Ubuntu 26.04 aarch64 disk keeps the original OS, user, tools, and
  repository state, with its root filesystem expanded from 128 GiB to 256 GiB.
  Both the Xvfb and real GNOME Wayland `search-history` live-window smokes
  passed on native ARM. Each used Vulkan through Mesa llvmpipe, reported as a
  CPU adapter, so the result proves software rendering and does not claim
  accelerated graphics.

- **Picker matches now render as vertical lists.** The command palette, layout
  picker, and SSH launcher share the scrollable menu panel, keep the selected
  result visible, and no longer flatten matches into a clipped bottom strip.

## Older releases

- [3.x](docs/changelog/CHANGELOG-3.x.md)
- [2.x](docs/changelog/CHANGELOG-2.x.md)
- [1.x](docs/changelog/CHANGELOG-1.x.md)
- [0.x](docs/changelog/CHANGELOG-0.x.md)
