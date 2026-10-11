#!/usr/bin/env bash
# The check right before a squash-merge into main: does the branch still build with main as it is
# now? Two branches that each pass the gate can still break together without a textual conflict
# (one renames a field or deletes a module, the other adds a use of it). This builds their merge.
#
#   tools/premerge.sh       merges origin/main into the branch's last commit in a throwaway tree
#                           (target/premerge/tree; nothing is pushed and the branch is unchanged),
#                           then runs `cargo check --workspace --all-targets` there and `wrela
#                           check` on every package whose files either side changed: its own
#                           directory or a package it depends on. A change to the compiler or to
#                           std checks every package. About a minute once warm (with the shared
#                           constants cache); the first run builds the CLI (several minutes),
#                           and a change to std or the compiler can bake constants again.
#
# It exits 1 if main conflicts with the branch (merge main in and resolve it), and 1 if the merge
# doesn't build or a package doesn't check. It checks what is committed, not the working tree.
# The tests stay the gate's and the long tier's: this only builds.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-$(python3 tools/resources.py)}"
# The throwaway tree's build keeps its own target, at the same path each run, so it stays warm.
work="$root/target/premerge"
export CARGO_TARGET_DIR="$work/target"
# A package's constants come from the session's shared cache when there is one (a floor's history
# bakes in minutes otherwise); it's keyed on the code that computes each, so a stale value can't
# be served.
if [ -z "${WRELA_CONST_CACHE:-}" ] && [ -d /mnt/project-files/.wrela-consts ]; then
  export WRELA_CONST_CACHE=/mnt/project-files/.wrela-consts
fi

git fetch -q origin main
if ! tree=$(git merge-tree --write-tree HEAD origin/main); then
  echo "premerge: the branch conflicts with origin/main: merge main into it and resolve that first"
  exit 1
fi
merged=$(git commit-tree "$tree" -p HEAD -p origin/main -m "premerge: HEAD with origin/main")
base=$(git merge-base HEAD origin/main)
if [ -d "$work/tree" ]; then
  git -C "$work/tree" checkout -q --force --detach "$merged"
else
  mkdir -p "$work"
  git worktree add -q --force --detach "$work/tree" "$merged"
fi
echo "premerge: $(git log --oneline -1 HEAD | cut -c1-70) with origin/main ($(git rev-parse --short origin/main))"

cd "$work/tree"
start=$SECONDS
cargo check -q --workspace --all-targets
echo "  cargo check: $((SECONDS - start))s"

# Every package that reads a file either side changed (since the two diverged).
start=$SECONDS
packages=$(git diff --name-only "$base" "$merged" | python3 -c '
import pathlib, re, sys
changed = [l.strip() for l in sys.stdin if l.strip()]
# The compiler and std reach every package.
everything = any(c.startswith("compiler/") and not c.startswith("compiler/tests/") for c in changed)
# Packages that must fail to check (the diagnostics and conformance cases) are the gate'"'"'s.
skip = ("compiler/tests/diagnostics/", "compiler/tests/conformance/")
deps, roots = {}, []
for toml in pathlib.Path(".").rglob("wrela.toml"):
    d = toml.parent
    if "target" in d.parts or "build" in d.parts:
        continue
    deps[d.resolve()] = [(d / p).resolve() for p in re.findall(r"path\s*=\s*\"([^\"]+)\"", toml.read_text())]
    if not str(d).startswith(skip):
        roots.append(d)
def closure(d, seen):
    if d not in seen:
        seen.add(d)
        for e in deps.get(d, []):
            closure(e, seen)
    return seen
here = pathlib.Path(".").resolve()
for d in sorted(roots):
    dirs = closure(d.resolve(), set())
    if everything or any((here / c).is_relative_to(x) for c in changed for x in dirs):
        print(d)
')
if [ -n "$packages" ]; then
  cargo build -q --release -p wrela --bin wrela
  # The packages at once, as many as the cores; each failure with its first diagnostics.
  failures=$(echo "$packages" | xargs -P "$CARGO_BUILD_JOBS" -I{} sh -c \
    '"$1" check "$2" > /dev/null 2>&1 || echo "$2"' _ "$CARGO_TARGET_DIR/release/wrela" {})
  echo "  wrela check: $(echo "$packages" | wc -l) packages, $((SECONDS - start))s"
  for p in $failures; do
    echo "  wrela check $p: fails"
    "$CARGO_TARGET_DIR/release/wrela" check "$p" 2>&1 | head -20 | sed 's/^/    /'
  done
  [ -z "$failures" ] || exit 1
else
  echo "  wrela check: no package reads a changed file"
fi
echo "premerge: the merge builds"
