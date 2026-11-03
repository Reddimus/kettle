if status is-interactive
  if functions -q codex; or abbr -q codex
    set -gx KETTLE_OWN_CODEX 1
  else
function codex
  command '/kettle-stand-in/bin/kettle' agent-setup --launch-codex -- $argv
end
    set -gx KETTLE_OWN_CODEX 0
  end
end
