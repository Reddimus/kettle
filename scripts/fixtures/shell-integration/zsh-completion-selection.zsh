emulate -L zsh
source "$1" >/dev/null

for skipped in '' $'\xff'; do
  output=$(kettle_completion_show completion zsh 1 "$skipped" '' beta '' gamma '')
  if [[ $output != *';completion;0;zsh;beta;;gamma;'* ]]; then
    print -u2 'selection must follow beta after a skipped label'
    exit 1
  fi
  output=$(kettle_completion_show completion zsh 0 "$skipped" '' beta '' gamma '')
  if [[ $output != *';completion;;zsh;beta;;gamma;'* ]]; then
    print -u2 'selection must clear when its row is skipped'
    exit 1
  fi
  output=$(kettle_completion_show completion zsh 2 "$skipped" '' beta '' gamma '')
  if [[ $output != *';completion;1;zsh;beta;;gamma;'* ]]; then
    print -u2 'selection must follow gamma after a skipped label'
    exit 1
  fi
done
print KETTLE_ZSH_SELECTION_OK
