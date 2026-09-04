# Terminal emulator research & citations

This is the living analysis behind kettle's design. Every borrowed idea is
attributed to its origin. Sources were cloned and read directly (paths shown
are within each upstream repo).

## Projects analyzed

| Project | Lang | Studied for |
|---|---|---|
| [Alacritty](https://github.com/alacritty/alacritty) | Rust | VT core, grid/scrollback, damage model |
| [vte](https://github.com/alacritty/vte) | Rust | Williams VT500 parser state machine |
| [WezTerm](https://github.com/wez/wezterm) | Rust | `portable-pty`, image protocols, mux |
| [kitty](https://github.com/kovidgoyal/kitty) | C/Py | graphics + keyboard protocols |
| [Ghostty](https://github.com/ghostty-org/ghostty) | Zig | config syntax, theme model, keybinds |
| [Terminator](https://github.com/gnome-terminator/terminator) | Py | splits/tabs/broadcast UX + keybindings |
| [Contour](https://github.com/contour-terminal/contour) | C++ | Sixel parser state machine |
| [Konsole](https://github.com/KDE/konsole) | C++ | VT102 compat, image-source discrimination |
| [st](https://git.suckless.org/st) | C | the minimal-correct VT floor |
| [libvterm](https://github.com/neovim/libvterm) | C | callback/damage model checklist |
| [xterm](https://invisible-island.net/xterm/) | C | `ctlseqs` — canonical control-sequence reference |

## What kettle borrowed, and from where

- **VT parsing, grid, scrollback, damage tracking** — Alacritty.
  `alacritty_terminal/src/term/mod.rs` (`Term`, `renderable_content`,
  `damage`), `grid/mod.rs`. We consume the crate directly rather than
  re-implementing. *(Apache-2.0; see NOTICE.)*
- **Parser** — `vte` Williams state machine
  (`vte/src/ansi.rs` `Processor::advance`).
- **Cross-platform PTY incl. Windows ConPTY** — WezTerm's `portable-pty`
  (`wezterm/pty/src/lib.rs` `MasterPty`, `native_pty_system`).
- **Image protocols** — kitty's spec
  (`kitty/docs/graphics-protocol.rst`, `keyboard-protocol.rst`); Sixel state
  machine modelled on Contour
  (`contour/src/vtbackend/SixelParser.cpp`); iTerm2 `OSC 1337 File=` and the
  three-way image-source split seen in Konsole
  (`konsole/src/Vt102Emulation.h`). *Implemented in `kettle-vt`*: an
  `Extractor` pulls Sixel/kitty/iTerm2 sequences out of the PTY stream before
  the VT parser sees them, decodes to RGBA, and the renderer composites them
  as scroll-anchored GPU textures. kitty advanced ops implemented:
  `a=t` transmit-only (stored for later), `a=p` place-by-id, `a=d`
  delete (all / by id), `z=` z-index ordering. Unicode placeholders,
  animation and relative placements remain future work.
- **Config syntax + theme model** — Ghostty's `key = value` grammar
  (`ghostty/src/config/Config.zig`); themes are the iTerm2-Color-Schemes
  `ghostty/` set that Ghostty itself consumes (it is not vendored in-tree —
  see `ghostty/src/cli/list_themes.zig`). Default **TokyoNight Night**.
- **Keybindings + multiplexer UX** — Terminator
  (`terminatorlib/config.py` default keybindings: split `Ctrl+Shift+O/E`,
  new tab `Ctrl+Shift+T`, close `Ctrl+Shift+W`, search `Ctrl+Shift+F`,
  broadcast/group input).
- **GPU-accelerated, damage-first rendering** — the approach Alacritty
  popularized and WezTerm refined (`wezterm-gui/src/renderstate.rs` dual
  Glium/WebGPU); kettle uses `wgpu` + `glyphon`.
- **Compatibility floor** — st (`st.c` `csihandle`/`strhandle`) defines the
  minimum; xterm `ctlseqs.txt` is the edge-case oracle; libvterm's
  callback/damage taxonomy is our coverage checklist.

## Compatibility tiers (target)

- **Tier 1 (must work):** C0/C1, CSI cursor/erase/insert/delete/scroll, full
  SGR incl. truecolor + underline styles + undercurl, DEC modes
  1/5/6/7/25/1000/1006/1047/1049/2004, OSC 0/1/2/52 — covered by
  `alacritty_terminal`.
- **Tier 2:** focus 1004, sync 2026, OSC 4/8/10-12, charsets G2/G3, mouse
  motion 1002/1003, DECRQSS — mostly covered; validate against `vttest`.
- **Tier 3:** Sixel / kitty graphics / iTerm2 images, grapheme mode 2027,
  ReGIS — tracked in [ROADMAP.md](ROADMAP.md).

## Open questions / next experiments

- Tiled multi-pane GPU rendering (per-pane viewport scissor) — designed,
  landing next; current build cycles focus between panes full-window.
- Hyperlink (OSC 8) click + URL autodetection overlay.
- Shell integration (OSC 133 prompt marks) for prompt jumping.
- SSH multiplexing / detachable session server (WezTerm-style).
- Session restore (serialize the pane tree + cwd).
