if test (count $argv) -ne 2
    echo 'usage: fish-osc.fish INTEGRATION CWD' >&2
    exit 2
end

cd -- $argv[2]; or exit 1
source $argv[1]; or exit 1

# Drive the same named events Fish emits around an interactive command. A
# failing command pins that fish_postexec preserves and reports its status.
emit fish_prompt
emit fish_preexec
false
emit fish_postexec
