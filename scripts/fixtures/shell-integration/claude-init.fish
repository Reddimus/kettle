if status is-interactive
  if functions -q claude
    set -gx KETTLE_OWN_CLAUDE 1:$fish_pid
  else
function claude
  command '/kettle-stand-in/bin/kettle' agent-setup --launch-claude -- $argv
end
    set -gx KETTLE_OWN_CLAUDE 0:$fish_pid
  end
end
