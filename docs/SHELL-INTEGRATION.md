# Shell integration (OSC 133)

Kettle understands **OSC 133** prompt marks. When your shell emits them, Kettle
records where each prompt started so you can jump
between commands:

| Action | Default bind |
|---|---|
| Jump to previous prompt | `Ctrl+Up` |
| Jump to next prompt | `Ctrl+Down` |

(Rebind with `keybind = ctrl+up=prev_prompt` / `next_prompt`.)

Marks are parsed out of the PTY stream ahead of the VT engine, so they never
corrupt the screen even on terminals/apps that don't understand them.

## Automatic integration: default PowerShell only

Since v2.30.0, `shell-integration = on` is the default. Automatic injection is
currently implemented only when Windows Kettle selects `pwsh` or
`powershell` as its default shell:

- **OSC 7** (current working directory) — so the tab/title tracks `cd` and new
  tabs / splits inherit the directory.
- **OSC 133** prompt marks (and **OSC 9;9**) — so prompt-jump (`Ctrl+Up` /
  `Ctrl+Down`) and prompt-aware close-confirmation work.

Kettle launches that shell with an encoded copy of `kettle.ps1`. The user's
PowerShell profile loads first; Kettle then wraps the resulting prompt, so
Starship, oh-my-posh, and posh-git remain in place. No `$PROFILE` edit is
required. `cmd.exe` is not injected because Kettle can read its changing
process working directory directly.

Automatic injection does **not** currently cover:

- native Linux or macOS bash, zsh, or fish;
- a Linux shell launched through `command = wsl.exe ...`;
- any explicit `command = ...`, including explicit PowerShell; or
- any shell when `shell-integration = off`.

Those cases need the manual snippet below. In particular, install the snippet
inside the WSL distribution whose shell will emit the marks; changing the
Windows PowerShell profile does not configure a Linux shell in WSL.

See the **`shell-integration`** key in [CONFIG.md](CONFIG.md) (bool, default
`on`) to toggle it.

## Enabling it manually in your shell

Use this section for Unix shells, WSL shells, an explicit `command =`, or a
deliberately disabled automatic hook. Default PowerShell users can normally
skip it.

### One-liner (recommended)

Kettle embeds the canonical snippets in the binary. Install one with the
command for your shell:

```sh
kettle --shell-integration bash       >> ~/.bashrc
kettle --shell-integration zsh        >> ~/.zshrc
kettle --shell-integration fish       >> ~/.config/fish/config.fish
kettle --shell-integration powershell >> $PROFILE       # PowerShell 5+ / 7+
```

The same snippets live at `shell-integration/kettle.{bash,zsh,fish,ps1}` and
ship in the release archives. Use the generated form instead of copying a
second implementation from this guide; updates then stay in one place.

## Completion list

`completion-overlay = auto` lets the bundled integration present the shell's
own candidates in a compact list above the active command. If the prompt is too
close to the top to leave a detached lane, Kettle hides the list rather than
moving it beside or below the input. It never follows or covers the command
line. Kettle never invents matches: the active shell supplies candidates and
quoting rules, and still owns execution. The bundled Fish and PowerShell
adapters preserve their shells' quoting and insertion rules while the detached
card is active. Fish inserts a captured singleton directly rather than querying
the provider a second time, so a changing provider cannot reopen the stock
pager beside the command line. The escaped result returned by Fish supplies the
exact editor spelling, including expandable `~user` and variable completions.
Kettle restores the ordinary unique-result space and leaves Fish's native
no-space continuations open.

The list appears only for the focused pane at an ordinary shell prompt with
enough rows above the command. It stays hidden in alternate-screen programs,
scrollback, input-method composition, short splits, and while Kettle has a
modal open. Clicking the card dismisses it without acting on terminal content
hidden underneath. Tab still works when the list is hidden. Prompt and command-line
changes discard stale replies. Losing window focus clears pending requests in
every in-process pane, including panes armed by broadcast input, while scrolling
away from the live command hides the card until the viewport returns. Fish and
PowerShell identify every prompt session and completion keypress, including
clear replies. Kettle advances that request only after the key enters the PTY
queue and preserves individual keys in remote batches, so delayed output or
backpressure cannot desynchronize the shell and terminal. Each prompt also
advertises which Tab directions Kettle owns; Fish updates that mask when its Vi
keymap changes, so a custom binding never consumes an adapter request. Counter
exhaustion disables the side channel instead of reusing an identity. A legacy
cooperative publisher without these identities stays hidden until the next
prompt after focus loss.

Fish and PowerShell install the list automatically only when Tab still has the
shell's stock completion binding. A custom user or plugin binding is never
replaced and does not publish a detached list, because Kettle cannot safely
sequence requests for a handler it does not own. Fish supports its default and
Vi insert key maps. PowerShell queries
`TabExpansion2` once per cycle, then uses that same cached result for both the
PSReadLine edit and the visible row. Tab and Shift Tab cycle the same list. The
list is cleared when the command line is accepted or a new prompt starts. Both
automatic adapters retain a bounded prefix: at most 2048 candidates, 16 KiB per
source row, and 256 KiB in aggregate. They publish only the 64-row page around
the current selection. Larger or oversized results stop at that bounded prefix
instead of invoking an inline shell pager. For a multi-candidate Fish result,
the first Tab opens only the detached list and leaves the command line unchanged;
later Tab or Shift Tab presses select from that retained result.

Automatic bindings stay off inside tmux and screen because those multiplexers
can consume the private metadata while still passing the capability variable
to a nested shell. Existing shells keep the mode negotiated when they started;
changing `completion-overlay` applies to newly opened shells.

Bash and Zsh integrations expose a cooperative bridge instead of claiming
their many completion-widget variants. A widget can call:

```sh
kettle_completion_show completion source selected \
  label-1 description-1 label-2 description-2
kettle_completion_clear
```

`selected` is empty or a zero-based row. The bridge is bounded to 64 rows and
percent-encodes text before it enters the terminal stream. Set
`completion-overlay = off` to advertise no capability to new shells and retain
their ordinary Tab behavior.

The snippets also report the working directory at each prompt. This keeps tab
titles current and lets new tabs and splits inherit the right directory.

### Windows installer

If you installed via the bundled `install.ps1`, you can let the
installer wire up `$PROFILE` for you in one go (no manual
`>> $PROFILE` step needed):

```powershell
# From the extracted release .zip folder:
.\install.ps1 -WithShellIntegration
```

The flag appends the installed `kettle.ps1` to `$PROFILE` inside managed
markers. Reinstalling is a no-op, and uninstalling removes only that block.
The snippet also guards against duplicate prompt wrappers when a profile is
reloaded.

When PSReadLine still owns the stock Enter and Tab bindings, the snippet adds
command marks and the detached completion list. Customized bindings remain
untouched.

## Marks

The four `OSC 133` marks bracket one command. What kettle gets from them is a
prompt boundary it can jump to, and — because `C` without a matching `D` means
a command is still running — a reliable answer to "is this pane busy?".

```mermaid
sequenceDiagram
    autonumber
    participant Shell
    participant Kettle
    Shell->>Kettle: OSC 133#59;A — prompt start
    Note over Kettle: jump target for<br/>Ctrl+Up / Ctrl+Down
    Shell->>Kettle: OSC 7 — current working directory
    Note over Kettle: hostname validated against this machine,<br/>so an ssh session's remote cwd is never adopted
    Shell->>Kettle: OSC 133#59;B — end of prompt, input starts
    Note over Kettle: pane is idle at a prompt →<br/>close skips the confirm dialog
    Shell->>Kettle: OSC 133#59;C — command started
    Note over Kettle: pane counts as busy
    Shell->>Kettle: OSC 133#59;D#59;code — finished, with exit code
    Note over Kettle: back to idle
```

A shell with no integration never sends `C` or `D`, so it always counts as
busy and its close behaviour is unchanged.

- `OSC 133;A` — prompt start (used for jump targets)
- `OSC 133;B` — end of prompt / input start
- `OSC 133;C` — command started executing
- `OSC 133;D;<code>` — command finished (exit code)
- `OSC 7` — current working directory, reported every prompt.
  Both `file://host/percent-encoded` and
  `kitty-shell-cwd://host/raw` schemes are accepted; the hostname is
  validated against this machine, so an **ssh session's remote cwd is
  never adopted locally**. Windows paths travel URL-form
  (`file://HOST/C:/Users/...`) and normalize back to drive-letter form.

The OSC 133 marks also make close-confirmation prompt-aware: a pane idle at an
integrated-shell prompt — marks seen, no command running — skips the
`ask-before-closing` dialog; a shell without integration always counts
as busy, so its behavior is unchanged.
