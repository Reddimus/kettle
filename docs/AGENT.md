# Agent-first kettle

kettle is designed so AI agents (Claude Code, Codex, …) work great with it **both
ways**:

- **Interactively** — run the agent *inside* a kettle pane, like any terminal
  program. (Always worked; nothing to configure.)
- **Non-interactively / programmatically** — an agent *drives* kettle: run a
  command headlessly and read its output, or attach to a running kettle window
  to read the screen and type into panes.

This doc covers the programmatic surface: `kettle exec`, the control server +
`kettle ctl`, and the `kettle mcp` MCP server. It is OFF by default — the
control server only starts when you opt in.

## The three entry points

```
kettle exec  -- <argv…>     # headless one-shot: run a command, stream its output
kettle ctl   <method> …     # drive a RUNNING kettle (list panes, read, send, run)
kettle mcp                  # MCP server: expose all of the above as agent tools
```

## Control plane

```mermaid
flowchart LR
    subgraph agent["AI agent (Claude Code / Codex)"]
        cc["MCP client"]
    end
    subgraph oneshot["Headless one-shot"]
        exec["kettle exec<br/>(no window)"]
        pty1["real PTY<br/>(VT emulation)"]
        exec --> pty1
    end
    subgraph gui["Running kettle (GUI)"]
        srv["control server<br/>(off by default)"]
        app["App main thread<br/>(self.mux)"]
        panes["panes / Terminals"]
        srv -->|UserEvent::Ctl| app
        app --> panes
    end
    cc -->|kettle_run| exec
    cc -->|"list_panes / read_screen / screenshot<br/>send_text / send_keys<br/>wait_for / run_command"| mcp["kettle mcp"]
    mcp -->|spawn| exec
    mcp -->|"kettle-ctl client"| ipc["local IPC<br/>Unix socket / Windows named pipe"]
    ctl["kettle ctl"] -->|kettle-ctl client| ipc
    ipc --> srv
    classDef off fill:#1e1e2e,stroke:#cba6f7,color:#cdd6f4;
    class srv,ipc off;
```

The protocol, transport, discovery, and client live in the **`kettle-ctl`** crate
(UI-free). The GUI hosts the server side; `kettle ctl` and `kettle mcp` host the
client side. This split is deliberate: when the optional `kettle-muxd` session
daemon (see [MUX-SERVER-DESIGN.md](MUX-SERVER-DESIGN.md)) lands, it can re-host
the same server side without breaking any client — the discovery registry
already reserves a `kind` field (`"gui"` today, `"muxd"` later).

## `kettle exec` — headless one-shot

Run a command under a real PTY (full VT emulation) with no GPU and no window,
and stream its output to real stdout. Propagates the child's exit code (124 on
`--timeout`, 125 on an internal error).

```sh
kettle exec -- python -c "print(2+2)"           # → 4
kettle exec --strip-ansi -- ls --color=always   # plain text, escapes stripped
kettle exec --json -- some-tui                   # NDJSON: start/output/title/exit
kettle exec --timeout 5 -- slow-thing            # kill + exit 124 after 5s
kettle exec --record run.cast -- make            # also save an asciicast trace
```

Output modes: raw (default, verbatim PTY bytes — includes a terminal's normal
control sequences), `--strip-ansi` (plain text, good for assertions), `--json`
(one JSON object per line).

### ConPTY caveats (Windows)

On Windows the child runs under a ConPTY (pseudoconsole). Two consequences:

- **Raw mode includes ConPTY's startup handshake** (`ESC[6n`, mode-set
  sequences) and is a *re-rendered* screen, not byte-verbatim. For assertions
  use `--strip-ansi` (or the MCP `kettle_run` tool, which strips by default).
- **A command that exits in well under ~50 ms** can have its output collapsed by
  ConPTY's screen-differ before kettle ever sees it. `kettle exec` adds a short
  settle-drain to mitigate this, but a near-instant command may still under-emit.

### Stdin forwarding

On Unix and WSL, when `kettle exec` is launched with piped or redirected stdin,
it forwards that input to the child PTY:

```sh
printf 'hello\n' | kettle exec --strip-ansi -- sh -c 'read x; echo "got:$x"'
```

Interactive terminal stdin is not stolen from the user, and `/dev/null` stays
closed rather than being treated as useful input. EOF on the input pipe closes
the PTY input side.

Native Windows ConPTY stdin forwarding remains disabled: closing or otherwise
driving conin from a non-interactive parent currently makes console children
exit with `STATUS_CONTROL_C_EXIT` on the Windows CI runner. WSL uses the Unix
path above. The MCP `kettle_run` tool still gives the child no stdin by design;
use `kettle exec` directly for Unix/WSL stdin-driven one-shot commands.

## Control server + `kettle ctl`

The control server lets another process inspect and drive a *running* kettle
window. It is **off by default**; enable it per launch or in config:

```sh
kettle --agent-server full        # this launch only
# or in ~/.config/kettle/config (or %APPDATA%\kettle\config on Windows):
#   agent-server = full
```

Modes: `off` (no server), `read-only` (read the screen / list panes / subscribe),
`full` (also send text + run commands).

Then drive it with `kettle ctl`:

```sh
kettle ctl get_state                                   # version, theme, pid, mode
kettle ctl list_panes                                  # id / tab / cwd / size / focus
kettle ctl read_screen                                 # focused pane's visible text
kettle ctl read_screen --pane 3 --json '{"scrollback_lines":200}'
kettle ctl send_text --text "ls -la"                   # type into the focused pane
kettle ctl send_keys --keys "enter"                    # …then press Enter
kettle ctl send_keys --keys "escape,:,w,q,enter"       # press keys/chords (v2.20)
kettle ctl wait_for --text "INSERT" --json '{"timeout_ms":5000}'   # block until on screen
kettle ctl run_command --text "cargo build"            # run + wait for the result
kettle ctl events                                       # stream the event feed (NDJSON)
kettle ctl get_state --pid 12345                        # target a specific kettle
```

Note: `--text` is literal — backslash escapes like `\n` are **not** decoded —
so press Enter with `send_keys`, not a trailing `\n`.

### Methods (protocol v1)

| Method | Mode | Result |
|---|---|---|
| `get_state` | read-only | version, pid, mode, theme, focused pane, `windows` (count), `focused_window` (seq) |
| `list_tabs` | read-only | every window's tabs: `window` (seq), index, title, active, pane ids |
| `list_panes` | read-only | every window's panes: id, `window` (seq), tab, title, cwd, cols/rows, focused, argv, child_pid, agent_attached, read_only |
| `read_screen` | read-only | text + cursor + `cursor_visible` (DEC ?25) + history (params: `pane`, `scrollback_lines`) |
| `subscribe` | read-only | switches the connection to the event stream |
| `wait_for` | read-only | v2.20: block until the screen matches (`text` substring / `regex` / `quiet_ms` settle — AND when combined; `timeout_ms` default 30 000). Returns `{matched, elapsed_ms, polls}`; a timeout is `matched: false`, not an error. Runs on the connection thread, polling ≥50 ms — the UI is never blocked. The screen-text regex runs against per-line right-trimmed, newline-joined text — use `(?m)` end-of-line anchors rather than end-of-string |
| `send_text` | full | type text into a pane (`pane`, `text`) |
| `send_keys` | full | v2.20: press named keys / chords (`pane`, `keys: ["escape","ctrl+c","down","G",…]`). Tokens: key names (`escape`, `enter`, `tab`, `backspace`, `delete`, `insert`, `space`, arrows, `home`/`end`, `pageup`/`pagedown`, `f1`–`f12`), chords with `ctrl`/`alt`/`shift`/`super` (+ aliases), or single characters (case preserved). Encoded through the same path as GUI keystrokes against the pane's live modes (DECCKM-aware); all tokens parse before any byte is sent |
| `run_command` | full | run `command` in a pane, reply with `{exit_code, duration_ms, output}` |

**Multi-window (v2.18)**: a kettle process can host several OS windows.
`list_tabs` / `list_panes` enumerate them all, ordered by window seq;
`index`, `tab`, `active`, and `focused` are *within-window* values — the
`window` field disambiguates. Pane ids are process-global and stable across
tab moves/tear-offs, and an explicit `pane` param targets a pane in **any**
window (without one, the focused window's focused pane is used).

`run_command` correlates the shell's OSC 133 command-end marker to learn the
exit code. **Without shell integration** there is no marker, so the call returns
`{timed_out: true, …}` after `timeout_s` (default 15) with a hint to run
`kettle --shell-integration <shell>`. Output is still captured either way.

A pane the user has toggled **Read only** (right-click menu /
`toggle_read_only`) rejects `send_text`, `send_keys` and `run_command` with
the `read_only` error code — the agent is input like any other, and the
user's lock wins.

### Driving an interactive app (v2.20)

`send_keys` + `wait_for` together make interactive TUIs scriptable without
sleep-and-pray. Editing a file in vim from an agent:

```sh
kettle ctl send_text  --text "vim notes.txt"
kettle ctl send_keys  --keys "enter"
kettle ctl wait_for   --json '{"quiet_ms":300,"timeout_ms":10000}'   # vim painted
kettle ctl send_keys  --keys "i"                                     # insert mode
kettle ctl wait_for   --text "-- INSERT --"
kettle ctl send_text  --text "hello from an agent"
kettle ctl send_keys  --keys "escape,:,w,q,enter"                    # save + quit
kettle ctl wait_for   --json '{"regex":"(?m)\\$$","quiet_ms":200,"timeout_ms":5000}'   # prompt is back
```

The same flow over MCP uses `kettle_send_keys` / `kettle_wait_for`. Read the
screen between steps with `read_screen` — its `cursor` + `cursor_visible`
(DEC ?25) tell you where input would land and whether the app is showing a
cursor at all (vim's command line, fzf and less hide it).

Events (after `subscribe`): `command_finished`, `pane_focus`, `title`,
`agent_attached`, `tab_moved` (`{from_window, to_window, tab}` — a tab was
torn off / moved to another window), and `lag` (when a slow subscriber's
queue overflowed).

### When an agent attaches a pane

A pane targeted by a control connection shows the `agent-badge` prefix (default
`"[agent] "`) in its per-pane titlebar, and an `agent_attached` event fires. Set
`agent-badge = ` (empty) to disable, or to any glyph you like (`agent-badge = 🤖 `).

## `kettle mcp` — MCP server

Expose all of the above as Model Context Protocol tools, so Claude Code/Codex get
kettle as native tools.

```sh
claude mcp add kettle -- kettle mcp
```

Or a project-scoped `.mcp.json`:

```json
{ "mcpServers": { "kettle": { "command": "kettle", "args": ["mcp"] } } }
```

Tools: `kettle_run` (headless one-shot — needs no running kettle),
`kettle_list_panes`, `kettle_read_screen`, `kettle_screenshot`,
`kettle_send_text`, `kettle_send_keys`, `kettle_wait_for`,
`kettle_run_command` (these drive a running kettle, so start it with
`kettle --agent-server full`). When no server is found, the control-backed
tools return an actionable error pointing at `--agent-server`.

The control surface also exposes `screenshot`, which saves a live PNG using the
same renderer readback path as the UI screenshot action. It works in
`agent-server=read-only`; by default it captures the focused pane crop, and
`--json '{"full_window":true}'` captures the whole window.

`kettle mcp --self-test` runs an in-process handshake + `tools/list` + one
`kettle_run`, for CI.

## Local Smoke Checks

Two optional scripts cover the agent workflows that depend on tools installed on
the developer's machine, so they are intentionally not CI gates:

```sh
scripts/check-agent-cli-smoke.sh
```

Runs Codex CLI, Claude Code CLI, clean Neovim, and configured Neovim/AstroNvim
through `kettle exec` when those commands are present on `PATH`; missing tools
are reported as skips.

```sh
just live-render-smoke
```

Starts a real Kettle window with `text-renderer = grid`, captures several live
screenshots through `kettle ctl screenshot`, and fails if cursor blink changes a
broad region instead of a cursor-sized box. This needs a visible X11/Wayland
desktop session.

```sh
just linux-perf
```

Runs the Linux Hyperfine peer gate when `terminator` and `ghostty` are installed
(`alacritty` is included when present). It builds the release binary, launches
each terminal for a `/bin/true` startup probe and a ~4 MiB ASCII flood probe,
then fails if Kettle does not beat Terminator or stay within 10% of Ghostty.
This is also desktop-local because it opens real GUI terminal windows.

## Security & threat model

- **Off by default.** No server, no socket, no registry entry unless you opt in.
- **Local only.** The transport is a Unix domain socket (mode `0600`) or a
  Windows named pipe with the default DACL (creator/owner + admins). There is
  **no TCP** at this layer. The protection boundary is "the same local user (and
  elevated admins)" — identical to the kettle process itself.
- **Capability split.** `read-only` cannot send keystrokes or run commands; only
  `full` can. A single `require_full` gate guards every mutating method (a
  drift-guard test pins this).
- **Auditable.** Every connection and every mutating method is logged. When the
  dev-record recorder is active, each agent action is annotated in the `.cast`
  trace as an `m` marker (`kettle:agent <method> conn=N`).

If you don't want any of this, do nothing — it stays off.

## Future work

- Re-hosting the server on the `kettle-muxd` session daemon
  ([MUX-SERVER-DESIGN.md](MUX-SERVER-DESIGN.md)) — clients are unaffected.
- An "agent waiting for input" surfacing (the command-notify plumbing already
  exists).
