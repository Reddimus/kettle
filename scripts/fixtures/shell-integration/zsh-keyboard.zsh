# Interactive: each line below is one command, so preexec runs once per line.
source "$1" || exit 1
# The prompt pushes legacy key reporting; a second prompt replaces its entry.
__kettle_precmd; __kettle_precmd; print -rn -- KETTLE_KEYBOARD_SUBSHELL; ( exit 0 ); print -rn -- KETTLE_KEYBOARD_COMMAND
exit 0
