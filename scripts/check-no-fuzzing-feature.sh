#!/usr/bin/env bash
# The `fuzzing` features export internal entry points for cargo-fuzz (see
# fuzz/Cargo.toml). No shipped binary needs them, so the product's graph, on
# every target platform and without dev-dependencies, must build none.
set -euo pipefail

tree=$(cargo tree --locked -e features,no-dev -p kettle -p kettle-media-worker --target all)
if found=$(grep 'feature "fuzzing"' <<<"$tree"); then
    echo "error: a shipped binary's dependency graph enables a fuzzing feature:" >&2
    echo "$found" >&2
    exit 1
fi
echo "No shipped binary enables a fuzzing feature."
