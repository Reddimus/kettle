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
```

## Marks

- `OSC 133;A` — prompt start (used for jump targets)
- `OSC 133;B` — end of prompt / input start
- `OSC 133;C` — command started executing
- `OSC 133;D;<code>` — command finished (exit code)

Origin: FinalTerm's shell-integration convention, adopted by iTerm2, kitty,
WezTerm and Ghostty (see [RESEARCH.md](RESEARCH.md)).
