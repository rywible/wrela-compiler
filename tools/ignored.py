"""The tests the gate leaves out, by their `#[ignore]` reason: `long:`, `measure:` and `heavy:`.

    python3 tools/ignored.py    prints `<kind> <name>` for each, as tools/check.sh reads them

A name is `module::test`, or `test` in a test program of one file. tools/hooks.py reads the
measurements' too (a `cargo test` of one is allowed)."""

import pathlib
import re
import sys


def ignored(root="."):
    """(kind, name) for each test of the Rust sources under `root` whose `#[ignore]` reason
    starts `long:`, `measure:` or `heavy:`."""
    root = pathlib.Path(root)
    out = []
    for f in sorted([*(root / "compiler").rglob("*.rs"), *(root / "runtime").rglob("*.rs")]):
        rel = f.relative_to(root)
        if "target" in rel.parts:
            continue
        lines = f.read_text(encoding="utf-8", errors="replace").splitlines()
        next_functions = [None] * len(lines)
        name = None
        for i in range(len(lines) - 1, -1, -1):
            next_functions[i] = name
            if m := re.search(r"fn (\w+)\(", lines[i]):
                name = m.group(1)
        inside = False
        for i, line in enumerate(lines):
            if m := re.search(r'#\[ignore = "(long|measure|heavy):', line):
                name = next_functions[i]
                # A test file at the top of `tests/` is a test program of its own (no module). A
                # unit test in `src/` is in its file's module, under the directories it's in, and
                # in its `mod tests` (a crate's root file names no module, nor does a `mod.rs`).
                if rel.parent.name == "tests":
                    out.append((m[1], name))
                elif "src" in rel.parts:
                    path = [*rel.parts[rel.parts.index("src") + 1 : -1], rel.stem]
                    module = "".join(f"{p}::" for p in path if p not in ("lib", "main", "mod"))
                    out.append((m[1], f"{module}{'tests::' if inside else ''}{name}"))
                else:
                    out.append((m[1], f"{rel.stem}::{name}"))
            inside |= bool(re.match(r"\s*mod tests\b", line))
    return out


if __name__ == "__main__":
    for kind, name in ignored(sys.argv[1] if len(sys.argv) > 1 else "."):
        print(kind, name)
