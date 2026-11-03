if status is-interactive
  if functions -q codex
    set -gx KETTLE_OWN_CODEX 1:$fish_pid
  else
function codex
  command '/kettle-stand-in/bin/kettle' agent-setup --launch-codex -- $argv
end
    set -gx KETTLE_OWN_CODEX 0:$fish_pid
  end
end
