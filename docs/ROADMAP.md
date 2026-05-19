# Roadmap

## Done

- [x] Cargo workspace, MIT, CI matrix (Linux/macOS/Windows)
- [x] PTY + `alacritty_terminal` + `vte` core, per-pane reader thread
- [x] `wgpu` + `glyphon` renderer: text, bg, cursor, selection, search rects
- [x] Ghostty-syntax config + ~500 bundled themes (default TokyoNight Night)
- [x] Bundled JetBrains Mono Nerd Font (embedded)
- [x] Terminator-compatible keybindings + live config reload
- [x] Tabs + **binary split tree**, tiled multi-pane GPU rendering, split
      dividers, focus border, tab bar
- [x] Geometry-based focus nav (`Alt+Arrows`), mouse-click focus, ratio
      resize (`Ctrl+Shift+Arrows`), broadcast/group input
- [x] Regex search overlay (`Ctrl+Shift+F`)
- [x] Clipboard (OSC 52 + copy/paste), keyboard input encoding, IME text

## Next (in priority order)

- [ ] Selection by mouse drag; mouse reporting passthrough
- [ ] Sixel + kitty graphics + iTerm2 OSC 1337 image protocols (`kettle-vt`)
- [ ] Hyperlink (OSC 8) + URL autodetection, click to open
- [ ] Shell integration (OSC 133) + prompt jumping
- [ ] Session save/restore (pane tree + cwd) and layouts
- [ ] Ligature tuning + per-style font family overrides
- [ ] SSH multiplexing / detachable mux server
- [ ] macOS app bundle + native menu; Windows installer

## Quality bar each cycle

`cargo fmt` · `cargo clippy -D warnings` · `cargo build` · `cargo test` ·
end-to-end run · docs updated · commit.
