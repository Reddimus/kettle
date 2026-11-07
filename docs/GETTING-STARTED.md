# Getting started with kettle

Set up Kettle, learn the common keys, and change preferences in **Settings**.

## 1. Install

- **Linux** — one line, no `sudo`:
  ```sh
  curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh | sh
  ```
  This path needs a current `curl`, GNU `tar`, OpenSSL 3.0 or newer, and
  Python 3. It stops before extraction if the bounded signed-release checks
  cannot run.
  Then press **Super** and type **kettle**.
- **macOS** — download `kettle-macos-universal.zip`, unzip, drag `kettle.app`
  to `~/Applications` (create it if needed), or `/Applications` when writable.
  The official app is notarized and opens normally.

See [INSTALL.md](INSTALL.md) for Nix, the generated-but-not-yet-published
Homebrew/AUR metadata, and SHA-256 verification.

## 2. Your first window

kettle opens a normal terminal with your usual shell. Type commands like you
would anywhere. Run `kettle` commands here; from another macOS terminal,
use `~/Applications/kettle.app/Contents/MacOS/kettle` if `kettle` is not on
your `PATH`.

A single-tab window hides the tab strip by default. Creating another tab shows
it; set `tab-bar = always` to keep it visible. Horizontal and vertical strips
expose their tab titles, selected tab, Close buttons, New tab, and New tab menu
to screen readers. The menu button beside `+` opens shell choices and shortcuts.

If the shell or directly launched command fails, its pane stays open with
"Process exited with code N". Press **Enter** or click **Restart** to run the
same command with its original working directory and environment in that pane.
Press **Escape**, click **Close**, or use the close-pane chord to close it.
Successful processes close automatically. Set `exit-behavior = hold` to keep
successful exits too, or `exit-behavior = close` to close every exit.

### Profiles, layouts and SSH

The **▾** menu beside New tab and the macOS **Shell** menu offer
**New Tab with Profile**, **Save Layout…**, **Open Layout…** and **SSH launcher**.
The command palette (`Ctrl+Shift+K`) also offers these actions and one entry
per profile. Profiles open a separate process/window so their font, theme and
other application settings apply safely. When none exist, **Create Profile…**
opens the profiles directory; add a `NAME.config` file there (copy your current
config as a starting point). Reopen the menu to discover new files.

**Save Layout…** accepts 1–128 ASCII letters, digits, dots, underscores or
hyphens (except `.` and `..`). Enter saves, Escape cancels. Reusing a name
replaces the saved arrangement. Open Layout lists saved names, with a hint to
save when empty. Layouts keep normal windows, tabs, splits and directories;
quick-terminal windows are excluded. Opening a layout launches new shells or
saved commands, without prior terminal output or running process state.

Use the palette's broadcast actions to send input to all panes, a group or a
window, and choose broadcast off to stop. See
[Sessions, profiles and layouts](../README.md#sessions-profiles-and-layouts)
for restore behavior and [CONFIG.md](CONFIG.md) for configuration paths and SSH
host entries.

## 3. Change settings — no file editing required

Press **`Ctrl + ,`** (or **right-click → Settings…**) to open the **Settings**
panel. Use the keyboard:

- **↑ / ↓** — move between options
- **← / →** — change the highlighted option (font size, theme opacity,
  scrollbar, bell, cursor, …)
- **Tab** — cycle categories, including Agents and Keybinds. In **Keybinds**,
  pick an action, press a chord (with a modifier) to bind it live, and it's
  written to your config.
- **Esc** — close

Changes save automatically. Most apply immediately; shell startup settings
affect new panes. See [Settings](SETTINGS.md) for exceptions. For advanced
options not in the panel, the
**Advanced** path opens the config file with your operating system's default app. Full reference:
[SETTINGS.md](SETTINGS.md) and [CONFIG.md](CONFIG.md).

Unknown config keys and invalid values show a dismissible notice inside the
window at startup and reload. It names up to three filename:line locations and
keys, with a count of additional issues. The affected settings use defaults;
valid settings still apply. Run `kettle --check-config` to see every issue.

## 4. Splits, tabs, and getting around

Kettle starts with these pane and tab keys:

| Do this | Press |
|---|---|
| Split left/right | `Ctrl+Shift+E`, or `Cmd+D` on macOS |
| Split top/bottom | `Ctrl+Shift+O`, or `Shift+Cmd+D` on macOS |
| New tab | `Ctrl+Shift+T` |
| Close pane | `Ctrl+Shift+W` |
| Move focus between panes, Linux | `Alt + Arrow` |
| Move focus between panes, macOS | `Cmd+Opt+Arrow` |
| Cycle panes | `Ctrl+Shift+N` / `Ctrl+Shift+P` |
| Resize the split (a program such as Codex or Neovim gets the keys while it runs) | `Shift + Arrow` |
| Next / previous tab | `Ctrl+PageDown` / `Ctrl+PageUp` |
| Search screen + scrollback | `Ctrl+Shift+F` |
| Settings | `Ctrl+,` |
| Command palette (search every action) | `Ctrl+Shift+K` |

See the full list any time with `kettle --list-keybinds`, or press
`Ctrl+Shift+K` and type what you want.

A fresh window opens at about 164 columns (a `160x45` baseline fitted to your
monitor; about 154 on a 1366 px wide laptop). That is on purpose: agent TUIs
lay out side panels above ~110 columns, and Claude Code's fullscreen diff panel
auto-opens at 144. Set `window-width` / `window-height` for a different size,
or `window-state = maximise`. A split pane is narrower than the window; zoom it (`Ctrl+Shift+X`)
when a tool wants the full width. See
[TERMINAL-CLIENT-COMPATIBILITY.md](TERMINAL-CLIENT-COMPATIBILITY.md#claude-code-diff-panel)
for the panel's exact requirements.

On Linux, the focus chord is edge-aware: if no visible split exists in the
arrow's direction, Kettle passes `Alt+Arrow` to the program instead. This keeps
terminal-app shortcuts such as Codex's `Alt+Left`/`Alt+Right` word motion and
`Alt+Up` previous-message editor usable without giving up fast split
navigation. A zoomed pane (`Ctrl+Shift+X`, or the `scaled_zoom` action) hides
its siblings, so every `Alt+Arrow` reaches the program until you leave the zoom;
a one-pane tab passes it through as well.

macOS leaves `Option+Arrow` to the terminal, so split focus lives on
`Cmd+Opt+Arrow`, the chord iTerm2 and Ghostty both use. `Ctrl+Cmd+Arrow` does
the same thing and stays bound. `Ctrl+Shift+N` / `Ctrl+Shift+P` cycles panes on
every platform.

Right-click a terminal to reach Copy, Paste and Select All first. A link adds
Open Link and Copy Link before those actions. Split, Pane, Tab, Window, Title,
Group and Appearance commands live in submenus. Click a category or press Right
to enter it; Left or Escape returns to its parent. Hovering a category also
opens it. Menus and the command palette show native shortcut symbols on macOS
(for example, ⌘V and ⇧⌘D); other platforms keep Ctrl+Shift+V notation.

Pasting text that contains a newline or a control character other than tab
asks for confirmation when the receiving program has not enabled bracketed
paste. Cancel is selected initially. Choose Paste to send the original text,
or Paste as one line to replace line breaks and other control characters with
spaces (tabs stay tabs). A short preview escapes control characters so they
are visible. Bracketed paste keeps multi-line text and strips embedded ESC
bytes so pasted text cannot close the bracket early. Set `paste-protection =
always` to confirm multi-line text even in editors, or `paste-protection = off`
to disable confirmation. See [configuration](CONFIG.md).

## 5. Search screen and scrollback

Press **`Ctrl+Shift+F`** to open the bottom search bar. Enter a Rust regular
expression (up to 4096 UTF-8 bytes). One that fails to parse, such as `call(x`,
is searched as literal text. A valid expression that exceeds the bounded engine
budget shows **Pattern too complex**.

- `Enter` / `Shift+Enter` finds in the default / opposite direction.
- `F3` / `Shift+F3` always finds next / previous, and so do `Cmd+G` /
  `Shift+Cmd+G` (`Ctrl+G` / `Ctrl+Shift+G` on Linux).
- **Wrap** controls whether stepping crosses the history boundary.
- **Regex** switches between regular expressions and literal text. Click it or
  press `Alt+R` on Linux and Windows, `Cmd+Alt+R` on macOS; regex starts on,
  including the literal fallback for invalid patterns.
- Click **Case** to cycle modes. **Smart** ignores case until the pattern contains an uppercase character;
  **Match** always matches case; **Ignore** never matches case.
- **Invert** flips the default direction. `Escape` closes the bar while keeping
  the selected result anchored on screen.

The query shows its current match and total, such as **3/122**, **No matches**,
or **Counting…** until the count finishes. Capacity-limited searches show
**Results limited**.

The bar is a lane below the grid, not a dialog over it: while it is open you
can still drag-select text, double-click words, open links, scroll, and
right-click for the menu, exactly as in Terminator. Keyboard focus stays in the
query. `Ctrl+Shift+C` copies the query's own selection if it has one, otherwise
the text you selected in the grid. Kettle's other shortcuts keep working, such
as a new tab or font zoom, and `Ctrl+Z` (`Cmd+Z` on macOS) undoes an edit to
the query. Clicking
another split moves the bar to that pane and searches it with the same query and
toggles. A program that has turned
on mouse reporting receives your clicks while the bar is open, just as it does
without it.

The query editor follows Unicode grapheme boundaries for caret movement and
deletion, so combining marks and emoji sequences are not split. Kettle
remembers the last query for each pane within its current window. The status
shows boundary, invalid, too-complex, or **Results limited** feedback alongside
the count. Results limited means a pathological
soft-wrapped line, an output-interrupted navigation, or the nearby-highlight
cap prevented an exact count or made ordering uncertain; Kettle will not claim a possibly wrong first,
last, or miss. Ordinary work-budget pauses resume automatically. This keeps
large or infinite scrollback responsive.

When you scroll into history, click the **↓ 1,234 lines** pill at the pane's
bottom-right to return to live output. The default history budget is 64 MB per
pane with a 10,000-line cap. Settings shows **64 MB** under **Scrollback memory**;
`scrollback-bytes` and `scrollback` let you change the two limits independently.
Selection and non-current search highlights are strengthened for visibility in
faint themes, and the current search result has a distinct underline.

## 6. If something looks wrong

- **Text too big/small?** Open Settings (`Ctrl+,`) → Appearance → Font size,
  or use `Ctrl + +` / `Ctrl + -`.
- **Want the defaults back?** Delete your config file (its location is shown by
  `kettle --config-path`) and relaunch.
- **Found a bug?** On Linux and macOS, crash logs are written to
  `$XDG_STATE_HOME/kettle/crash/` when set and otherwise
  `~/.local/state/kettle/crash/`. GPU device-loss records are written to
  `$XDG_CACHE_HOME/kettle/diagnostics/` when set and otherwise
  `~/.cache/kettle/diagnostics/`; they contain adapter/recovery metadata and no
  terminal contents. Attach the relevant file to an issue.

Welcome aboard. For everything else, [CONFIG.md](CONFIG.md) is the full
reference and [README.md](../README.md) has the feature tour.
