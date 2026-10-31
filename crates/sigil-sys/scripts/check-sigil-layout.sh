#!/bin/sh
# Compiles tests/layout_probe.c against the vendored sigil.h and prints the sizes/offsets that
# src/lib.rs's layout tests must assert. Run after bumping the vendored Sigil, and paste any
# changed numbers (a change means the header changed shape and the Rust structs need updating).
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
out=$(mktemp)
${CC:-cc} -std=c99 -I"$here/vendor/argosy-sigil/include" "$here/tests/layout_probe.c" -o "$out"
"$out"
rm -f "$out"
