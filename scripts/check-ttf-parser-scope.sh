#!/usr/bin/env bash
# Guard the temporary RUSTSEC-2026-0192 exception.
#
# Kettle still reaches `ttf-parser` through glyphon/cosmic-text/fontdb while
# upstreams work through RazrFalcon/fontdb#90. This script keeps that exception
# narrow: if another dependency path appears, CI fails and the audit ignore must
# be re-reviewed.
#
# v2.34.0 note: every check below runs against `cargo tree -i ttf-parser` —
# the inverse tree of packages that REACH ttf-parser — not the whole graph.
# `sctk-adwaita` itself is back (winit's `wayland-csd-adwaita-notitle`
# feature, restoring GNOME Wayland titlebar decorations) but without its
# `ab_glyph` text renderer it does not depend on ttf-parser, so it never
# appears in this tree and correctly does not trip the guard. If someone
# upgrades to the full `wayland-csd-adwaita` (ab_glyph) feature, the
# sctk-adwaita → ab_glyph → owned_ttf_parser path DOES enter this tree and
# both the exact-path check and the forbidden-crate loop fail — exactly the
# review trigger we want. The manifest side is pinned by the
# `winit_wayland_csd_stays_notitle` test in kettle-ui.

set -euo pipefail

expected=$(cat <<'EOF'
ttf-parser
fontdb
cosmic-text
glyphon
kettle-render
kettle
kettle-ui
kettle
EOF
)

if ! tree=$(CARGO_TERM_COLOR=never cargo tree -q -i ttf-parser --prefix none --format '{p}' 2>&1); then
    if grep -q "did not match any packages" <<<"$tree"; then
        echo "ttf-parser is no longer in the dependency graph."
        echo "Remove the RUSTSEC-2026-0192 ignores from deny.toml and audit.yml, then close #36."
        exit 0
    fi
    printf '%s\n' "$tree" >&2
    exit 1
fi

actual=$(awk '/^[A-Za-z0-9_.-]+ v[0-9]/ { print $1 }' <<<"$tree")

if [ "$actual" != "$expected" ]; then
    cat >&2 <<EOF
::error::ttf-parser dependency scope changed.

Expected the only unresolved path to remain:
$expected

Actual cargo tree path:
$tree

Re-review RUSTSEC-2026-0192 before keeping the advisory ignore.
EOF
    exit 1
fi

for forbidden in owned_ttf_parser sctk-adwaita ab_glyph; do
    if grep -q "$forbidden" <<<"$tree"; then
        echo "::error::$forbidden re-entered the ttf-parser dependency path" >&2
        exit 1
    fi
done

echo "ttf-parser scope OK: only glyphon -> cosmic-text -> fontdb remains."
