# shellcheck disable=SC1090 # The harness supplies the integration path.
source "$1" || exit 1
trap - DEBUG
cd "$2" || exit 1
__kettle_osc7
