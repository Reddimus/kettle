use super::*;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::Processor;

fn config(text: &str) -> Config {
    let (mut cfg, unknown) = Config::parse_collect(text);
    assert!(unknown.is_empty(), "unknown config keys: {unknown:?}");
    cfg.shell_integration = false;
    cfg.login_shell = false;
    cfg
}

fn new_pane(mux: &mut Mux, cfg: &Config) -> u64 {
    mux.new_tab_with_geometry(
        cfg,
        PtyGeometry::from_cell_size(40, 4, 8, 16),
        Arc::new(|| {}),
        &["/bin/cat".into()],
        None,
    )
    .expect("spawn test pane");
    mux.active_focus().unwrap()
}

fn check_mode(pane: &Pane, parser: &mut Processor, enabled: bool) {
    parser.advance(&mut *pane.term.term.lock().unwrap(), b"\x1b[?2027$p");
    let reply = pane
        .rx
        .try_iter()
        .filter_map(|event| match event {
            TermEvent::PtyWrite(reply) => Some(reply),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(
        reply,
        format!("\x1b[?2027;{}$y", if enabled { 1 } else { 2 })
    );
}

#[test]
fn c08_default_emoji_clusters() {
    let mut mux = Mux::new();
    let id = new_pane(&mut mux, &config(""));
    let pane = &mux.panes[&id];
    let mut parser = Processor::new();
    check_mode(pane, &mut parser, true);
    let mut term = pane.term.term.lock().unwrap();
    for sequence in [
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}",
        "\u{1f1fa}\u{1f1f8}",
        "\u{1f44d}\u{1f3fd}",
        "\u{1f469}\u{200d}\u{1f4bb}",
        "\u{1f3f3}\u{fe0f}\u{200d}\u{1f308}",
        "\u{2764}\u{fe0f}",
    ] {
        parser.advance(&mut *term, b"\x1b[H\x1b[2J");
        parser.advance(&mut *term, sequence.as_bytes());
        assert_eq!(term.grid().cursor.point.column, Column(2), "{sequence:?}");
        let cell = &term.grid()[Line(0)][Column(0)];
        assert!(cell.flags.contains(Flags::GRAPHEME | Flags::WIDE_CHAR));
        assert!(
            term.grid()[Line(0)][Column(1)]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER)
        );
        let stored: String = std::iter::once(cell.c)
            .chain(cell.zerowidth().unwrap_or_default().iter().copied())
            .collect();
        assert_eq!(stored, sequence);
    }
}

#[test]
fn c08_config_false_scalar_cells() {
    let mut mux = Mux::new();
    let id = new_pane(&mut mux, &config("grapheme-clustering = false"));
    let pane = &mux.panes[&id];
    let mut parser = Processor::new();
    check_mode(pane, &mut parser, false);
    let mut term = pane.term.term.lock().unwrap();
    parser.advance(&mut *term, "\u{1f469}\u{200d}\u{1f4bb}".as_bytes());
    assert_eq!(term.grid().cursor.point.column, Column(4));
    assert_eq!(term.grid()[Line(0)][Column(2)].c, '\u{1f4bb}');
    assert!(
        !term.grid()[Line(0)][Column(0)]
            .flags
            .contains(Flags::GRAPHEME)
    );
}

#[test]
fn c08_default_mode_can_be_disabled_and_enabled() {
    let mut mux = Mux::new();
    let id = new_pane(&mut mux, &config(""));
    let pane = &mux.panes[&id];
    let mut parser = Processor::new();
    check_mode(pane, &mut parser, true);
    for (sequence, expected) in [(b"\x1b[?2027l", false), (b"\x1b[?2027h", true)] {
        parser.advance(&mut *pane.term.term.lock().unwrap(), sequence);
        check_mode(pane, &mut parser, expected);
    }
}

fn check_resets(text: &str, enabled: bool) {
    let mut mux = Mux::new();
    let id = new_pane(&mut mux, &config(text));
    let pane = &mux.panes[&id];
    let mut parser = Processor::new();
    for reset in [b"\x1bc".as_slice(), b"\x1b[!p"] {
        let toggle = if enabled {
            b"\x1b[?2027l"
        } else {
            b"\x1b[?2027h"
        };
        parser.advance(&mut *pane.term.term.lock().unwrap(), toggle);
        check_mode(pane, &mut parser, !enabled);
        parser.advance(&mut *pane.term.term.lock().unwrap(), reset);
        check_mode(pane, &mut parser, enabled);
    }
}

#[test]
fn c08_resets_restore_true() {
    check_resets("grapheme-clustering = true", true);
}

#[test]
fn c08_resets_restore_false() {
    check_resets("grapheme-clustering = false", false);
}

#[test]
fn c08_reload_affects_only_new_terminals() {
    let mut mux = Mux::new();
    let mut cfg = config("");
    let old = new_pane(&mut mux, &cfg);
    let mut parser = Processor::new();
    check_mode(&mux.panes[&old], &mut parser, true);
    cfg = config("grapheme-clustering = false");
    // Reload applies other grid options through this path as well.
    mux.set_scrollback_limits(cfg.scrollback, cfg.scrollback_bytes);
    let new = new_pane(&mut mux, &cfg);
    check_mode(&mux.panes[&old], &mut parser, true);
    check_mode(&mux.panes[&new], &mut parser, false);
    parser.advance(
        &mut *mux.panes[&old].term.term.lock().unwrap(),
        b"\x1b[?2027l",
    );
    cfg = config("grapheme-clustering = true");
    mux.set_scrollback_limits(cfg.scrollback + 1, cfg.scrollback_bytes);
    let newest = new_pane(&mut mux, &cfg);
    check_mode(&mux.panes[&old], &mut parser, false);
    check_mode(&mux.panes[&new], &mut parser, false);
    check_mode(&mux.panes[&newest], &mut parser, true);
    parser.advance(&mut *mux.panes[&old].term.term.lock().unwrap(), b"\x1bc");
    parser.advance(&mut *mux.panes[&new].term.term.lock().unwrap(), b"\x1b[!p");
    check_mode(&mux.panes[&old], &mut parser, true);
    check_mode(&mux.panes[&new], &mut parser, false);
}
