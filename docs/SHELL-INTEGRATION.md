# Shell integration (OSC 133)

kettle understands the FinalTerm/iTerm2/kitty **OSC 133** prompt marks. When
your shell emits them, kettle records where each prompt started so you can jump
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

Most shells need a one-line hook. If you already use **Starship**, kitty's
shell integration, or iTerm2's, those emit OSC 133 and kettle picks them up
automatically — nothing else to do.

### One-liner (recommended)

kettle ships the snippets embedded in the binary — install with one command:

```sh
kettle --shell-integration bash       >> ~/.bashrc
kettle --shell-integration zsh        >> ~/.zshrc
kettle --shell-integration fish       >> ~/.config/fish/config.fish
kettle --shell-integration powershell >> $PROFILE       # PowerShell 5+ / 7+
```

The same snippets live at `shell-integration/kettle.{bash,zsh,fish,ps1}` in
the source tree (also shipped in the Linux release tarball and the Windows
zip).

> **v2.20:** the shipped snippets also report the **working directory via
> OSC 7** every prompt (percent-encoded, hostname-tagged — PowerShell
> included), which powers new-tab/split cwd inheritance and "Open
> folder". The OSC 133 marks additionally make close-confirmation
> prompt-aware (an idle prompt skips the dialog). The minimal manual
> blocks below cover the OSC 133 prompt marks only; use the one-liner
> above (or copy from `shell-integration/`) for the full version.

### Windows / PowerShell — hands-free alternative

If you installed via the bundled `install.ps1`, you can let the
installer wire up `$PROFILE` for you in one go (no manual
`>> $PROFILE` step needed):

```powershell
# From the extracted release .zip folder:
.\install.ps1 -WithShellIntegration
```

`-WithShellIntegration` reads `kettle.ps1` from
`%LOCALAPPDATA%\Programs\kettle\shell-integration\` and appends it
to `$PROFILE` (wrapped in `# >>> kettle …` / `# <<<` markers so the
uninstall path can cleanly remove just that block later). Idempotent
— re-running with the flag is a no-op if the markers are already
there. `.\install.ps1 -Uninstall` strips the block from `$PROFILE`
on its own; `appwiz.cpl` → kettle → Uninstall does the same.

The `kettle.ps1` snippet itself also has an internal
`$global:__kettle_prompt_installed` guard, so re-sourcing `$PROFILE`
(or accidentally appending twice) won't stack multiple prompt
wrappers.

### bash — add to `~/.bashrc`

The snippet below is the OSC 133 half only — enough for prompt marks,
`jump_to_prompt`, and command-status colouring. The shipped
`shell-integration/kettle.bash` *also* reports the working directory over
OSC 7, which is what makes a new tab or split open where you already are, and
what puts a directory name on a tab. If you want that too — and you probably
do — source the shipped file instead of pasting this:

```bash
source /path/to/kettle/shell-integration/kettle.bash
```

```bash
# Capture the status first and hand it back, so anything chained after this
# still sees the real exit code rather than the printf's.
__kettle_pc() {
  local __kettle_status=$?
  printf '\033]133;D;%s\007\033]133;A\007' "$__kettle_status"
  return "$__kettle_status"
}
PROMPT_COMMAND="__kettle_pc${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
PS1='\[\033]133;B\007\]'"$PS1"
trap 'printf "\033]133;C\007"' DEBUG
```

### zsh — add to `~/.zshrc`

```zsh
autoload -Uz add-zsh-hook
__kettle_precmd()  { print -Pn '\e]133;D;%?\a\e]133;A\a'; }
__kettle_preexec() { print -Pn '\e]133;C\a'; }
add-zsh-hook precmd __kettle_precmd
add-zsh-hook preexec __kettle_preexec
PS1=$'%{\e]133;B\a%}'"$PS1"
```

### fish — add to `~/.config/fish/config.fish`

```fish
function __kettle_prompt --on-event fish_prompt
    printf '\e]133;A\a'
end
function __kettle_preexec --on-event fish_preexec
    printf '\e]133;C\a'
end
function __kettle_postexec --on-event fish_postexec
    printf '\e]133;D;%d\a' $status
end
# `B` (end-of-prompt) goes inside the prompt itself — fish doesn't
# have a dedicated event for it. Append the marker to your prompt's
# trailing output. With the default prompt, prefix your existing
# `fish_prompt` definition's final `echo`/`printf` so the marker
# emits *after* all the prompt text but *before* the user starts
# typing. (Most users don't need B for jump-to-prompt — A alone
# is enough; B is only useful if a tool wants to know where the
# user's input area starts.)
```

### PowerShell — add to `$PROFILE`

The PowerShell snippet (`shell-integration/kettle.ps1`) is
emitted by `kettle --shell-integration powershell`. Works on
Windows PowerShell 5.1+ (preinstalled on Windows 10+) and on
PowerShell Core 7+ (any OS). To find your profile path:
`echo $PROFILE`, then `code $PROFILE` to edit (it may not exist yet
— PowerShell will create it on first save).

Unlike bash/zsh/fish, there's no minimal inline snippet for
PowerShell here — the real prompt-wrapper (stash-and-forward the
user's existing `prompt` function, restore OSC 7 cwd reporting,
guard against a throwing prompt) is easy to get subtly wrong by
hand. **Always install it with the one-liner:**

```powershell
kettle --shell-integration powershell >> $PROFILE
```

(A pre-2.30-ish hand-transcription of this snippet that captures
`Get-Item function:prompt` directly, instead of its `.ScriptBlock`,
and calls it back with `&` — with no `try`/`catch` — will recurse
forever the moment the wrapper redefines `prompt`, since `&` on a
`FunctionInfo` re-resolves the *live* `prompt` function rather than
invoking a frozen copy. `shell-integration/kettle.ps1` fixes this by
capturing `.ScriptBlock` and wrapping the callback in `try`/`catch`;
always regenerate from the one-liner above rather than copying an
inline snippet by hand.)

The generated snippet guards itself with a
`$global:__kettle_prompt_installed` flag, so re-sourcing `$PROFILE`
(after a config tweak, after a new shell session loads it) won't
stack multiple prompt wrappers.

When PSReadLine's Enter key still uses its stock `AcceptLine` function, the
snippet wraps it to emit OSC 133;C. If the profile or another module installed
a different Enter binding, Kettle leaves that binding intact; PSReadLine does
not expose a previously registered ScriptBlock that Kettle could safely call.

## Marks

- `OSC 133;A` — prompt start (used for jump targets)
- `OSC 133;B` — end of prompt / input start
- `OSC 133;C` — command started executing
- `OSC 133;D;<code>` — command finished (exit code)
- `OSC 7` — current working directory, reported every prompt (v2.20).
  Both `file://host/percent-encoded` and kitty's
  `kitty-shell-cwd://host/raw` schemes are accepted; the hostname is
  validated against this machine, so an **ssh session's remote cwd is
  never adopted locally**. Windows paths travel URL-form
  (`file://HOST/C:/Users/...`) and normalize back to drive-letter form.

The OSC 133 marks also make close-confirmation **prompt-aware** (v2.20,
Ghostty `confirm-close-surface` semantics): a pane idle at an
integrated-shell prompt — marks seen, no command running — skips the
`ask-before-closing` dialog; a shell without integration always counts
as busy, so its behavior is unchanged.

Origin: FinalTerm's shell-integration convention, adopted by iTerm2, kitty,
WezTerm and Ghostty (see [RESEARCH.md](RESEARCH.md)).
