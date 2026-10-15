#!/usr/bin/env bash
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
here="$(dirname "$0")"
python3 "$here/test-ttf-parser-scope.py"
exec python3 "$here/check-ttf-parser-scope.py"
