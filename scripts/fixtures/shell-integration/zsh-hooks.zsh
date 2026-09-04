typeset -gi __fixture_user_precmd=0
typeset -gi __fixture_user_preexec=0
PS1='USER> '
precmd() { (( ++__fixture_user_precmd )); }
preexec() { (( ++__fixture_user_preexec )); }

source "$1" || exit 1
source "$1" || exit 1

typeset -i precmd_matches=0
typeset -i preexec_matches=0
typeset hook
for hook in "${precmd_functions[@]}"; do
  [[ "$hook" == __kettle_precmd ]] && (( ++precmd_matches ))
done
for hook in "${preexec_functions[@]}"; do
  [[ "$hook" == __kettle_preexec ]] && (( ++preexec_matches ))
done
if (( precmd_matches != 1 || preexec_matches != 1 )); then
  print -u2 -- 'kettle hooks were not registered exactly once'
  exit 1
fi
if [[ "${functions[precmd]}" != *'__fixture_user_precmd'* ||
      "${functions[preexec]}" != *'__fixture_user_preexec'* ]]; then
  print -u2 -- 'kettle clobbered the user precmd or preexec function'
  exit 1
fi
if [[ "$PS1" != $'%{\e]133;B\a%}USER> ' ]]; then
  print -u2 -- 're-sourcing kettle duplicated or removed the prompt marker'
  exit 1
fi

print -r -- 'KETTLE_ZSH_HOOKS_OK'
