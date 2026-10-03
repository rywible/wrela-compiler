#!/usr/bin/env bash
# The agent syntax test's one way to compile: builds a task's package (<task-dir>/pkg) with the
# release CLI, logs the attempt (its sources and JSON diagnostics) under <task-dir>/attempts/N/,
# and prints the diagnostics as a person sees them. Exit status: 0 it builds, 1 it doesn't.
#
#   tools/agent-test/attempt.sh <task-dir>
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
task="$(cd "$1" && pwd)"
pkg="$task/pkg"
# Brought up to date every time: an old binary would test an old compiler.
(cd "$root" && cargo build -q --release -p wrela)
target="${CARGO_TARGET_DIR:-target}"
case "$target" in /*) ;; *) target="$root/$target" ;; esac
wrela="$target/release/wrela"
n=1
while [ -d "$task/attempts/$n" ]; do n=$((n + 1)); done
log="$task/attempts/$n"
mkdir -p "$log/src"
rsync -a --prune-empty-dirs --exclude build/ --include '*/' --include '*.wrela' --exclude '*' \
  "$pkg/" "$log/src/"
status=0
"$wrela" build "$pkg" -o "$task/build" --json > "$log/diagnostics.json" 2>/dev/null || status=$?
"$wrela" build "$pkg" -o "$task/build" 2>&1 || true
echo "$status" > "$log/status"
if [ "$status" = 0 ]; then echo "attempt $n: it builds"; else echo "attempt $n: it doesn't build"; fi
[ "$status" = 0 ]
