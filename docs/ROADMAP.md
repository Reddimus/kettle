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

- [x] Mouse drag text selection + wheel scrollback
- [x] Hyperlinks: OSC 8 + URL autodetection, underline + Ctrl/Cmd-click to open

- [x] Image protocols: Sixel + kitty graphics + iTerm2 OSC 1337, extracted
      ahead of the VT parser, decoded to RGBA, GPU-composited, scroll-anchored

- [x] Mouse reporting passthrough (X10 + SGR 1006): click/drag/wheel to
      vim/tmux/htop/fzf; local selection/scroll when tracking is off
- [x] Shell integration (OSC 133 A/B/C/D) + jump-to-prompt
      (`Ctrl+Up`/`Ctrl+Down`); bash/zsh/fish snippets in
      docs/SHELL-INTEGRATION.md

- [x] Session save/restore: OSC 7 cwd capture, tab/split tree + per-pane
      cwd serialized to session.json, restored on launch, autosaved on
      structural changes + exit

- [x] SSH multiplexing: per-pane argv, `Ctrl+Shift+S` SSH launcher with
      configured `ssh-host` tab-complete, SSH tabs persisted in sessions

- [x] Automated test harness (19 deterministic tests across vt/config/ui:
      decoders, extractor, config, keybinds, split-tree, session round-trip,
      8 MiB perf guard) + CI fmt-check, headless GPU smoke, CLI smoke on all
      three OSes — see docs/TESTING.md

- [x] Offscreen GPU self-test (compiles WGSL + renders a pass with no
      window) run in CI on Linux/macOS/Windows — real cross-platform GPU
      validation
- [x] OS packaging + release workflow: Linux tar+`.desktop`, macOS `.app`
      bundle, Windows zip, built on real runners and attached to GitHub
      releases on tag (see docs/INSTALL.md)

- [x] UX polish: safe bracketed paste (newline-normalized,
      injection-guarded), focus-aware hollow cursor, config cursor blink,
      visual bell flash, double-click word / triple-click line selection
      with auto-copy on release

- [x] kitty graphics advanced ops: `a=t` transmit-only store, `a=p`
      place-by-id, `a=d` delete (all / by id), `z=` z-index ordering

- [x] Per-style font family overrides (`font-family-bold/italic/
      bold-italic`) + ligature toggle (Advanced vs Basic shaping)

## Next (in priority order)
- [ ] Broader `vttest` conformance sweep + fixes
- [ ] kitty graphics: Unicode placeholders, animation frames, relative
      placements
- [ ] Ligature tuning + per-style font family overrides
- [ ] Detachable mux server (remote attach); broader `vttest` sweep
- [ ] Code-signed/notarized macOS build; Windows MSI; native macOS menu

## Quality bar each cycle

`cargo fmt` · `cargo clippy -D warnings` · `cargo build` · `cargo test` ·
end-to-end run · docs updated · commit.
