#!/usr/bin/env bash
# Build and run the de265_internals analytics smoke test.
# Usage: ./de265_internals_build_and_run.sh [prefix]
#   prefix defaults to $HOME/git/streamlens/vendor/libde265-prefix
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PREFIX="${1:-$HOME/git/streamlens/vendor/libde265-prefix}"
FIXTURE="$HOME/git/streamlens/fixtures/hevc_8bit.265"
OUT="$SCRIPT_DIR/de265_internals_smoke"

echo "=== Building de265_internals smoke test ==="
c++ -std=c++17 \
    "$SCRIPT_DIR/de265_internals_smoke.cc" \
    -I"$PREFIX/include" \
    -L"$PREFIX/lib" -lde265 \
    -o "$OUT"

echo "=== Running de265_internals smoke test ==="
"$OUT" "$FIXTURE"
