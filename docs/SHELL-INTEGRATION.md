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
kettle --shell-integration bash >> ~/.bashrc
kettle --shell-integration zsh  >> ~/.zshrc
kettle --shell-integration fish >> ~/.config/fish/config.fish
```

The same snippets live at `shell-integration/kettle.{bash,zsh,fish}` in the
source tree (also shipped in the Linux release tarball). The verbatim bodies
follow below in case you want to read or tweak them first.

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

## Marks

- `OSC 133;A` — prompt start (used for jump targets)
- `OSC 133;B` — end of prompt / input start
- `OSC 133;C` — command started executing
- `OSC 133;D;<code>` — command finished (exit code)

Origin: FinalTerm's shell-integration convention, adopted by iTerm2, kitty,
WezTerm and Ghostty (see [RESEARCH.md](RESEARCH.md)).
