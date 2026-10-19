#!/usr/bin/env bash
# One check: no arguments runs the suite; one name runs exactly one check.
# Wrela: wrela::<package-path>::<test-name> (compiler/std selects std); a workspace member's
# name goes to `wrela suite`, and `wrela-suite` runs the whole workspace's.
# Rust: module::test; Python: tools::unittest.id; browser: browser::file::test.
set -euo pipefail
cd "$(dirname "$0")/.."
exec python3 tools/check.py "$@"
