#!/usr/bin/env bash
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
exec python3 "$(dirname "$0")/check-ttf-parser-scope.py"
