if [[ ! -o interactive ]]; then
  print -u2 -- 'zsh prompt fixture requires an interactive shell'
  exit 1
fi
if [[ -o promptsubst ]]; then
  print -u2 -- 'zsh prompt fixture requires stock NO_PROMPT_SUBST behavior'
  exit 1
fi

PS1='USER> '
source "$1" || exit 1

print -rn -- 'KETTLE_ZSH_RENDER_BEGIN'
print -Pn -- "$PS1"
print -rn -- 'KETTLE_ZSH_RENDER_END'
