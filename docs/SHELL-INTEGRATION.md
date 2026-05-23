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

## Enabling it in your shell

Most shells need a one-line hook. If you already use **Starship**, kitty's
shell integration, or iTerm2's, those emit OSC 133 and kettle picks them up
automatically — nothing else to do.

### One-liner (recommended)

kettle ships the snippets embedded in the binary — install with one command:

```sh
kettle --shell-integration bash       >> ~/.bashrc
kettle --shell-integration zsh        >> ~/.zshrc
kettle --shell-integration fish       >> ~/.config/fish/config.fish
kettle --shell-integration powershell >> $PROFILE     # PowerShell 5+ / 7+
```

The same snippets live at `shell-integration/kettle.{bash,zsh,fish,ps1}` in
the source tree (also shipped in the Linux release tarball and the Windows
zip). The verbatim bodies follow below in case you want to read or tweak
them first.

### bash — add to `~/.bashrc`

```bash
__kettle_pc() { printf '\033]133;D;%s\007\033]133;A\007' "$?"; }
PROMPT_COMMAND="__kettle_pc${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
PS1='\[\033]133;B\007\]'"$PS1"
trap 'printf "\033]133;C\007"' DEBUG
```

### zsh — add to `~/.zshrc`

```zsh
precmd()  { print -Pn '\e]133;D;%?\a\e]133;A\a'; }
preexec() { print -Pn '\e]133;C\a'; }
PS1='%{$(print -Pn "\e]133;B\a")%}'"$PS1"
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

Cycle 730 added a PowerShell snippet (`shell-integration/kettle.ps1`)
emitted by `kettle --shell-integration powershell`. Works on
Windows PowerShell 5.1+ (preinstalled on Windows 10+) and on
PowerShell Core 7+ (any OS). To find your profile path:
`echo $PROFILE`, then `code $PROFILE` to edit (it may not exist yet
— PowerShell will create it on first save).

```powershell
if (-not $global:__kettle_prompt_installed) {
    # Stash the user's pre-existing `prompt` so the kettle wrapper
    # calls into it (preserves starship / oh-my-posh / posh-git).
    $global:__kettle_original_prompt = (Get-Item function:prompt -ErrorAction SilentlyContinue)

    function global:prompt {
        $code = $LASTEXITCODE
        if ($null -eq $code) { $code = 0 }
        $esc = [char]27; $bel = [char]7
        # D (last exit) + A (this prompt's start).
        [Console]::Write("$esc]133;D;$code$bel$esc]133;A$bel")
        $rendered = if ($null -ne $global:__kettle_original_prompt) {
            & $global:__kettle_original_prompt
        } else {
            "PS $($ExecutionContext.SessionState.Path.CurrentLocation)$('>' * ($NestedPromptLevel + 1)) "
        }
        [Console]::Write("$esc]133;B$bel")   # B = end of prompt.
        return $rendered
    }

    # C = command started. PSReadLine ships with PS 5.1+ on Windows;
    # silently skipped if the user has it disabled.
    if (Get-Module -ListAvailable PSReadLine) {
        Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
            [Console]::Write([char]27 + ']133;C' + [char]7)
            [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
        }
    }
    $global:__kettle_prompt_installed = $true
}
```

The `$global:__kettle_prompt_installed` flag makes the snippet
idempotent: re-sourcing `$PROFILE` (after a config tweak, after a
new shell session loads it) won't stack multiple prompt wrappers.

## Marks

- `OSC 133;A` — prompt start (used for jump targets)
- `OSC 133;B` — end of prompt / input start
- `OSC 133;C` — command started executing
- `OSC 133;D;<code>` — command finished (exit code)

Origin: FinalTerm's shell-integration convention, adopted by iTerm2, kitty,
WezTerm and Ghostty (see [RESEARCH.md](RESEARCH.md)).
