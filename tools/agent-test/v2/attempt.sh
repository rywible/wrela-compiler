#!/usr/bin/env bash
# The agent syntax test's (version 2) one way to compile: builds <task-dir>/pkg with the compiler
# at $WRELA_BIN, logs the attempt (its sources, JSON diagnostics and status) under
# <task-dir>/attempts/N/, and prints the diagnostics as a person sees them. It never builds the
# compiler: the runner chose that one, and recorded it (run.py).
#
#   WRELA_BIN=/abs/path/to/wrela tools/agent-test/v2/attempt.sh <task-dir>
set -euo pipefail
if [ -z "${WRELA_BIN:-}" ] || [ ! -x "$WRELA_BIN" ]; then
  echo "attempt.sh: WRELA_BIN must name the compiler under test, an executable" >&2
  exit 2
fi
task="$(cd "$1" && pwd)"
pkg="$task/pkg"
n=1
while [ -d "$task/attempts/$n" ]; do n=$((n + 1)); done
log="$task/attempts/$n"
mkdir -p "$log/src"
rsync -a --prune-empty-dirs --exclude build/ --include '*/' --include '*.wrela' --exclude '*' \
  "$pkg/" "$log/src/"
status=0
"$WRELA_BIN" build "$pkg" -o "$task/build" --json > "$log/diagnostics.json" 2>/dev/null || status=$?
"$WRELA_BIN" build "$pkg" -o "$task/build" 2>&1 || true
echo "$status" > "$log/status"
if [ "$status" = 0 ]; then echo "attempt $n: it builds"; else echo "attempt $n: it doesn't build"; fi
[ "$status" = 0 ]
