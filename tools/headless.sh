#!/bin/sh
# Runs a page in headless Chrome stable (real GPU, throwaway profile) and waits for it to finish.
# The page signals completion by PUTting results/DONE (any body). Console output from the page
# goes to <page-dir>/results/console.log, so WGSL compile errors and exceptions are visible.
#
# Usage: tools/headless.sh <page-dir relative to the repo root> [hash] [timeout-seconds]
#   tools/headless.sh runtime/browser/tests/first-light '#run' 120
#
# Needs the server on 127.0.0.1:8417 (python3 tools/serve.py 8417).
# Only one run uses the GPU at a time (see the lock below); others wait their turn.

set -u
PAGE="${1:?usage: headless.sh <page-dir> [hash] [timeout-seconds]}"
HASH="${2:-#run}"
TIMEOUT="${3:-600}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"   # the repo root
RESULTS="$ROOT/$PAGE/results"
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
PROFILE="$(mktemp -d /tmp/wrela-chrome.XXXXXX)"

curl -s -o /dev/null "http://127.0.0.1:8417/" || { echo "the server isn't running on 8417 (python3 tools/serve.py 8417)" >&2; exit 2; }

# One GPU user at a time. On 2026-10-01, ~12 concurrent headless runs starved WindowServer of GPU
# time for over 5 s; macOS's watchdog killed it and the whole desktop reset. Runs now queue on a
# lock (a directory, so creating it is atomic). A lock whose holder has died is reclaimed.
LOCK=/tmp/wrela-gpu.lock
WAITED=0
while ! mkdir "$LOCK" 2>/dev/null; do
  HOLDER="$(cat "$LOCK/pid" 2>/dev/null)"
  if [ -n "$HOLDER" ] && ! kill -0 "$HOLDER" 2>/dev/null; then rm -rf "$LOCK"; continue; fi
  [ "$WAITED" -eq 0 ] && echo "waiting for the GPU lock (held by ${HOLDER:-?}: $(cat "$LOCK/page" 2>/dev/null))" >&2
  WAITED=$((WAITED + 2))
  [ "$WAITED" -ge 3600 ] && { echo "gave up waiting for the GPU lock after an hour" >&2; exit 3; }
  sleep 2
done
echo $$ > "$LOCK/pid"
echo "$PAGE $HASH" > "$LOCK/page"
trap 'rm -rf "$LOCK"' EXIT INT TERM
[ "$WAITED" -gt 0 ] && echo "got the GPU lock after ${WAITED}s" >&2

mkdir -p "$RESULTS"
rm -f "$RESULTS/DONE"

"$CHROME" --headless=new --user-data-dir="$PROFILE" --no-first-run --no-default-browser-check \
  --disable-extensions --disable-background-timer-throttling --disable-renderer-backgrounding \
  --enable-logging=stderr --v=0 --window-size=1920,1080 \
  "http://127.0.0.1:8417/$PAGE/$HASH" 2> "$RESULTS/chrome.log" &
PID=$!

START=$(date +%s)
STATUS=1
while :; do
  if [ -f "$RESULTS/DONE" ]; then STATUS=0; break; fi
  if ! kill -0 "$PID" 2>/dev/null; then echo "chrome exited early" >&2; break; fi
  if [ $(( $(date +%s) - START )) -ge "$TIMEOUT" ]; then echo "timed out after ${TIMEOUT}s" >&2; break; fi
  sleep 1
done
kill "$PID" 2>/dev/null
sleep 1
kill -9 "$PID" 2>/dev/null
rm -rf "$PROFILE"

# Page console lines only (Chrome logs them as "INFO:CONSOLE:<line>] ...").
grep 'CONSOLE' "$RESULTS/chrome.log" | sed -e 's/^.*CONSOLE:[0-9]*\] //' -e 's/, source: .*$//' > "$RESULTS/console.log"
rm -f "$RESULTS/chrome.log"
echo "elapsed $(( $(date +%s) - START ))s; console: $RESULTS/console.log"
tail -20 "$RESULTS/console.log"
exit $STATUS
