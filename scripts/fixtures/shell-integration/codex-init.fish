if status is-interactive; and not functions -q codex
function codex
  command '/kettle-stand-in/bin/kettle' agent-setup --launch-codex -- $argv
end
end
