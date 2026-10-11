"""The gate's memory of slow tests that passed (tools/check.sh), so a gate on a slow machine runs
only the slow tests whose inputs changed.

A slow test (one that took SLOW seconds or more the last time it ran) runs alone, in a process of
its own under `strace`, which records each file the process and its children open, stat or run.
When it passes, the gate keeps an entry: the test, the hash of the Rust sources its program is
built from, the environment variables the code reads, and the content of every file it touched.
The next gate skips the test when an entry matches all of them: nothing that could change what it
does has changed since it passed.

    sources (Rust key) ─┐
    environment ────────┼─► entry for the test ──► the same at the next gate? ──► skip it
    each file it read ──┘                                       otherwise    ──► run it

The Rust key is the sources of the test's crate and of the crates it is built with (all the
workspace's, if the test ran one of its programs). A test that builds a wrela program can be keyed
instead on the files each build made (compiler/tests' `recorded_build` records them): the next
gate makes those builds again with the `wrela` CLI, and when the files are the same, a change to
the compiler's crates (COMPILE_ONLY) or to wrela sources only the builds read can't change what
the test does. A build tool's "early cutoff". That holds when the compiler did all its work for
the test in those builds, so such an entry is made only when the test lexed no wrela text
outside them (wrela-syntax says so in the record), read no wrela source outside them, ran none
of the workspace's programs, and its code names no compiler crate. A build made again is traced
too, and remembered by the same `wrela` CLI's bytes and the files it read: the gate after makes
it again only when one of those changed.

    sources but the compiler's ─┐
    environment ────────────────┤
    each file it read ──────────┼─► entry ──► the same, and each build ──► skip it
      (but what only its builds read)        makes the same files?
    each build's files ─────────┘

What the entry leaves out, and why that is safe:
- The test program itself: it is a function of the Rust sources (and their `include_str!`
  files), which are in the key. So are the other programs a test runs (the `wrela` CLI).
- What it wrote (its scratch files, `target/`): its outputs, not its inputs.
- Caches whose entries are keyed by their content (the native host's compiled code, Mesa's
  shader cache, a package's constants in `build/consts`): a hit gives the same result as a miss.
- The other modules of a multi-file test program, which it doesn't name (`clearing.rs` doesn't
  name `last_green`): their code doesn't run in its process.

Where the memory is (`stores`): WRELA_GATE_MEMO's directory; else, in a Claude project, one in
~/.cache that the project's shared folder keeps in step with the other machines' (`sync`, in a
process of its own: a read on the shared mount can stall for good, so the gate reads and writes
only this machine's copy); else target/gate-memo.

Linux only (strace; and `unshare`, which gives each process its own `target/tmp`, so that two
processes of one test program don't build into the same scratch directory). Elsewhere the gate
runs every test, as before.
"""

import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

# A test that took this long, or longer, runs alone and is remembered: CPU seconds alone, or
# seconds beside the other tests if it hasn't run alone lately (Store.timing).
SLOW = 5.0
# An entry not used or made for this long is removed.
KEEP_DAYS = 14

VERSION = "v1"
# Part of every entry's key: the code that decides what an entry keeps and how it is matched
# (`meaning`, set once this file is read), so the entries made before a change to it are not
# used, and a change to anything else here keeps them.
MEANING = None


def sha(data):
    return hashlib.sha256(data if isinstance(data, bytes) else data.encode()).hexdigest()


def available():
    """Whether this machine can trace and isolate a test: Linux, strace, and a mount namespace."""
    if not os.path.isdir("/proc") or not shutil.which("strace") or not shutil.which("unshare"):
        return False
    try:
        return subprocess.run(
            ["unshare", "--mount", "--propagation", "private",
             "strace", "-f", "--seccomp-bpf", "-qq", "-o", "/dev/null", "true"],
            capture_output=True, timeout=10).returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


# ---- what a test touched, from strace's log -------------------------------------------------

# Syscalls with a path, and where it is: the index of the path among the call's strings, after a
# directory descriptor or not. `write` names those that change the file system.
CALLS = {
    "open": "read", "openat": "read", "openat2": "read", "creat": "write",
    "stat": "stat", "lstat": "stat", "newfstatat": "stat", "statx": "stat", "access": "stat",
    "faccessat": "stat", "faccessat2": "stat", "readlink": "stat", "readlinkat": "stat",
    "statfs": "stat", "execve": "read", "execveat": "read",
    "mkdir": "write", "mkdirat": "write", "unlink": "write", "unlinkat": "write",
    "rmdir": "write", "rename": "write2", "renameat": "write2", "renameat2": "write2",
    "link": "write2", "linkat": "write2", "symlink": "write", "symlinkat": "write",
    "truncate": "write", "utimensat": "write", "chmod": "write", "fchmodat": "write",
}
WRITE_FLAGS = ("O_WRONLY", "O_RDWR", "O_CREAT", "O_TRUNC", "O_APPEND")

LINE = re.compile(r"^(\d+) +(\w+)\((.*)\) += (-?\d+|\?)(.*)$")
UNFINISHED = re.compile(r"^(\d+) +(.*) <unfinished \.\.\.>$")
RESUMED = re.compile(r"^(\d+) +<\.\.\. \w+ resumed>(.*)$")
# A string argument, and the directory descriptor before it if any (`AT_FDCWD</dir>, "x"`).
STRING = re.compile(r'(?:(?:AT_FDCWD|\d+)<((?:[^>\\]|\\.)*)>, )?"((?:[^"\\]|\\.)*)"')
RESULT_PATH = re.compile(r"^<((?:[^>\\]|\\.)*)>")


def unescape(s):
    """A string as strace printed it (C escapes), as a path."""
    out, i = bytearray(), 0
    raw = s.encode("utf-8", "surrogateescape")
    while i < len(raw):
        c = raw[i]
        if c == 0x5C and i + 1 < len(raw):  # backslash
            n = chr(raw[i + 1])
            if n == "x":
                out.append(int(raw[i + 2:i + 4], 16))
                i += 4
                continue
            if n in "01234567":
                j = i + 1
                while j < min(i + 4, len(raw)) and chr(raw[j]) in "01234567":
                    j += 1
                out.append(int(raw[i + 1:j], 8))
                i = j
                continue
            out.append({"n": 10, "t": 9, "r": 13, "v": 11, "f": 12}.get(n, ord(n)))
            i += 2
            continue
        out.append(c)
        i += 1
    return out.decode("utf-8", "surrogateescape")


def lines(log):
    """The calls in strace's log of `-f -y` (one per line, an interrupted call joined to the rest
    of it): (syscall, arguments, result, what follows the result)."""
    waiting = {}
    for line in log.splitlines():
        if m := UNFINISHED.match(line):
            waiting[m[1]] = f"{m[1]} {m[2]}"
            continue
        if m := RESUMED.match(line):
            if m[1] not in waiting:
                continue
            line = waiting.pop(m[1]) + m[2]
        if m := LINE.match(line):
            yield m[2], m[3], m[4], m[5]


# The paths compiler/tests' `recorded_build` looks at to mark a build's start and end.
BUILD_BEGIN = "/wrela-gate-memo/build-begin"
BUILD_END = "/wrela-gate-memo/build-end"


def events(log):
    """Each path the traced processes touched, in the order of the calls: (path, how), how one of
    "read" (opened to read), "run", "list" (a directory opened, so its list read), "stat" (asked
    about), "write" (made, changed or removed). A path is absolute."""
    for call, args, result, rest in lines(log):
        kind = CALLS.get(call)
        if kind is None:
            continue
        strings = [(unescape(d) if d else None, unescape(p)) for d, p in STRING.findall(args)]
        if not strings:
            continue
        if call in ("execve", "execveat"):
            strings, kind = strings[:1], "run"  # the program, not its arguments
        if kind == "write2":
            paths, kind = strings[:2], "write"
        else:
            paths = strings[:1]
        how = kind
        if call in ("open", "openat", "openat2"):
            flags = args
            if any(f in flags for f in WRITE_FLAGS):
                how = "write"
            elif "O_DIRECTORY" in flags:
                how = "list"
            elif "O_PATH" in flags:
                how = "stat"
        for directory, path in paths:
            full = path if os.path.isabs(path) or not directory else os.path.join(directory, path)
            yield os.path.normpath(full), how
        # An open that succeeded names the file it opened, links followed: that file too.
        if result not in ("?", "") and not result.startswith("-") and (m := RESULT_PATH.match(rest.strip())):
            if call in ("open", "openat", "openat2", "creat"):
                yield os.path.normpath(unescape(m[1])), how


RANK = {"stat": 0, "read": 1, "run": 1, "list": 2, "write": 3}


def touched(log):
    """What the traced processes touched: {path: how}, how as `events` gives it ("run" as
    "read"), the most a path was touched. One written is never an input, however else it was
    touched."""
    seen = {}
    for path, how in events(log):
        how = "read" if how == "run" else how
        if seen.get(path) == "write":
            continue
        if how == "write" or RANK[how] > RANK.get(seen.get(path), -1):
            seen[path] = how
    return seen


def unplaced(seen):
    """Whether a run read a relative path its trace doesn't say the directory of (a call with no
    directory descriptor, after a `chdir`, as `mkdir -p` does): such a path can't be checked
    again. (What it wrote isn't an input.)"""
    return any(how != "write" and not os.path.isabs(p) for p, how in seen.items())


class Trace:
    """What a test's trace says beyond the paths it touched: the programs it ran, and the paths
    touched only while one of its builds ran (between `recorded_build`'s marks; builds on other
    threads can overlap)."""

    def __init__(self, log):
        self.seen = touched(log)
        self.ran = []
        inside, outside, depth = set(), set(), 0
        for path, how in events(log):
            if path == BUILD_BEGIN:
                depth += 1
            elif path == BUILD_END:
                depth = max(0, depth - 1)
            else:
                (inside if depth else outside).add(path)
                if how == "run":
                    self.ran.append(path)
        self.in_builds_only = inside - outside


# ---- each file's state ----------------------------------------------------------------------


class Digests:
    """Each path's state, computed once per gate: its content's hash, its list's, or that it's
    absent."""

    def __init__(self):
        self.known = {}

    def of(self, path, how):
        key = (path, how)
        if key not in self.known:
            self.known[key] = self.compute(path, how)
        return self.known[key]

    @staticmethod
    def compute(path, how):
        try:
            if os.path.isdir(path):
                if how == "list":
                    return "list:" + sha("\n".join(sorted(os.listdir(path))))
                return "dir"
            if os.path.isfile(path):
                h = hashlib.sha256()
                with open(path, "rb") as f:
                    for block in iter(lambda: f.read(1 << 20), b""):
                        h.update(block)
                return "file:" + h.hexdigest()
            return "other" if os.path.lexists(path) else "absent"
        except OSError:
            return "unreadable"


class Inputs:
    """Which paths a test's entry keeps: not its outputs, nor the machine's own state."""

    def __init__(self, root, target, ignore=()):
        self.root = os.path.realpath(root)
        home = os.path.expanduser("~")
        self.ignore = [os.path.realpath(p) for p in (
            target, "/proc", "/sys", "/dev", "/tmp", "/run", "/var/tmp",
            os.path.join(home, ".cache"), os.environ.get("TMPDIR") or "/tmp", *ignore)]

    def kept(self, path):
        under = lambda d: path == d or path.startswith(d + os.sep)
        # The checkout's files are inputs wherever it is (in /tmp, say), its target/ aside, and a
        # package's constants cache (`build/consts`, keyed by each constant's code and inputs).
        if under(self.root):
            if f"{os.sep}build{os.sep}consts{os.sep}" in path[len(self.root):] + os.sep:
                return False
            return not any(under(i) for i in self.ignore if i.startswith(self.root + os.sep))
        return not any(under(i) for i in self.ignore)

    def name(self, path):
        """A path as an entry names it: from the checkout's root inside it (so entries are shared
        between checkouts at other paths), else absolute."""
        if path == self.root or path.startswith(self.root + os.sep):
            return "repo:" + os.path.relpath(path, self.root)
        return path

    def path(self, name):
        return os.path.join(self.root, name[5:]) if name.startswith("repo:") else name

    def changed_since(self, seen, since):
        """Whether a path touched and not written was changed at or after `since` (a time): while
        a run that read it ran, so what it read may not be what it is now."""
        for path, how in seen.items():
            if how == "write" or not self.kept(path):
                continue
            try:
                if os.stat(path).st_mtime >= since:
                    return True
            except OSError:
                pass
        return False

    def of(self, seen, digests):
        """The entry's inputs: {name: state} for each path touched and not written."""
        out = {}
        for path, how in sorted(seen.items()):
            if how == "write" or not self.kept(path):
                continue
            out[self.name(path)] = digests.of(path, how)
        return out


# ---- the Rust sources and the environment ---------------------------------------------------

ENV_NAMES = [
    re.compile(r'env::var(?:_os)?\(\s*"([A-Za-z_][A-Za-z0-9_]*)"'),
    re.compile(r'environ(?:\.get\(|\[)\s*["\']([A-Za-z_][A-Za-z0-9_]*)'),
    re.compile(r'getenv\(\s*["\']([A-Za-z_][A-Za-z0-9_]*)'),
]
# Variables the GPU's driver and the Rust runtime read, by prefix.
ENV_PREFIXES = ("LP_", "GALLIVM_", "MESA_", "VK_", "LIBGL_", "WGPU_", "RUST_MIN_STACK")
# Variables the gate sets per run (a log's path, a cache's), not inputs.
ENV_NOT = {"WRELA_GPU_WAITS", "WRELA_NATIVE_CACHE", "WRELA_GATE_MEMO", "WRELA_BUILD_RECORD"}


def env_names(texts):
    names = set()
    for text in texts:
        for pattern in ENV_NAMES:
            names.update(pattern.findall(text))
    return names


# The crates that only turn a program's sources into its build. What they do reaches a test that
# builds a program only through the build's files, which `recorded_build` records, unless the
# test runs them otherwise (it lexes outside its builds, or its code names them).
COMPILE_ONLY = {"wrela-syntax", "wrela-sema", "wrela-ir", "wrela-lower", "wrela-wasm", "wrela-wgsl"}
# A test's code naming one of those it can use without lexing (wrela-syntax's lexer says when it
# lexes outside a build).
NAMES_COMPILER = re.compile(r"\bwrela_(?:sema|ir|lower|wasm|wgsl)\b")
# The workspace's files outside its crates that every crate is built with.
ROOT_FILES = ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/")


def rust_code(text, tests=True):
    """A Rust file's code as rustc builds it: its comments left out and, without `tests`, each
    item marked `#[cfg(test)]` (rustc builds those only into the crate's own unit tests, never
    into a crate built with it), and its spacing as one space. A change to what's left out can't
    change a test built with the file, so it doesn't change the file's key. An item it can't place the end of exactly stays
    (in part): that only keeps a key changing."""
    toks, i, n = [], 0, len(text)
    while i < n:
        c = text[i]
        if text.startswith("//", i):
            i = text.find("\n", i)
            i = n if i < 0 else i
        elif text.startswith("/*", i):
            depth, i = 1, i + 2
            while i < n and depth:
                if text.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif text.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
        elif c.isspace():
            j = i
            while j < n and text[j].isspace():
                j += 1
            toks.append(("ws", text[i:j]))
            i = j
        elif c.isalpha() or c == "_":
            j = i
            while j < n and (text[j].isalnum() or text[j] == "_"):
                j += 1
            word, hashes = text[i:j], 0
            while j + hashes < n and text[j + hashes] == "#":
                hashes += 1
            if word in ("r", "br", "cr") and j + hashes < n and text[j + hashes] == '"':
                close = '"' + "#" * hashes
                end = text.find(close, j + hashes + 1)
                end = n if end < 0 else end + len(close)
                toks.append(("lit", text[i:end]))
                i = end
            else:
                toks.append(("word", word))
                i = j
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            toks.append(("lit", text[i:j + 1]))
            i = j + 1
        elif c == "'" and (text.startswith("\\", i + 1) or text.startswith("'", i + 2)):
            j = i + 2 if text[i + 1] == "\\" else i + 1
            j = text.find("'", j + 1)
            j = n - 1 if j < 0 else j
            toks.append(("lit", text[i:j + 1]))
            i = j + 1
        else:
            toks.append(("p", c))
            i += 1
    if not tests:
        code = [k for k, t in enumerate(toks) if t[0] != "ws"]
        marker = [("p", "#"), ("p", "["), ("word", "cfg"), ("p", "("), ("word", "test"),
                  ("p", ")"), ("p", "]")]
        drop, at = set(), 0
        while at < len(code):
            if [toks[k] for k in code[at:at + len(marker)]] != marker:
                at += 1
                continue
            # Generics' `<..>` count as brackets in a signature or a type: not after its `=`.
            start, at, depth, angle, value = code[at], at + len(marker), 0, 0, False
            while at < len(code):
                kind, t = toks[code[at]]
                if kind == "p" and t == "=" and depth == 0:
                    value = True
                elif kind == "p" and t == "<" and not value:
                    angle += 1
                elif kind == "p" and t == ">" and not value and toks[code[at - 1]] != ("p", "-"):
                    angle -= 1
                elif kind == "p" and t in "([{":
                    depth += 1
                elif kind == "p" and t in ")]}":
                    if depth == 0:
                        break  # the end of what holds the item: not the item's
                    depth -= 1
                    if depth == 0 and t == "}":
                        at += 1
                        break
                elif kind == "p" and (t == ";" or t == "," and angle <= 0) and depth == 0:
                    at += 1
                    break
                at += 1
            end = code[at - 1] if at > 0 else start
            drop.update(range(start, end + 1))
        toks = [t for k, t in enumerate(toks) if k not in drop]
    # Space between tokens only parts them (no code reads its own lines: `line!()` is unused).
    out = []
    for kind, t in toks:
        if kind != "ws":
            out.append(t)
        elif out and out[-1] != " ":
            out.append(" ")
    return "".join(out).strip()


class Sources:
    """The files the test programs are built from, by crate: each crate's files (`git ls-files`
    under its directory, and those rustc's dep-info says it read elsewhere, by `include_str!`),
    the workspace's manifests, and each file's hash, computed once."""

    def __init__(self, root, target, packages=None):
        self.root = os.path.realpath(root)
        # {directory from the root: {"name": .., "deps": [directories]}}; None: one crate of all.
        self.packages = self.metadata(self.root) if packages is None else packages
        out = subprocess.run(["git", "ls-files", "-co", "--exclude-standard", "-z"],
                             cwd=self.root, capture_output=True, check=True).stdout
        tracked = sorted(set(f for f in out.decode("utf-8", "surrogateescape").split("\0") if f))
        self.common = [f for f in tracked if f in ROOT_FILES or f.startswith(ROOT_FILES[3])]
        self.by_package = {d: set() for d in self.packages or {}}
        dirs = sorted(self.by_package, key=len, reverse=True)
        for f in tracked:
            owner = next((d for d in dirs if f.startswith(d + "/")), None)
            if owner is not None:
                self.by_package[owner].add(f)
        # What each crate read outside its directory, by its dep-info (`<crate>-<hash>.d`).
        crates = {c: d for d, p in (self.packages or {}).items() for c in p.get("crates", [])}
        for dep in self.dep_files(target):
            owner = crates.get(os.path.basename(dep).rsplit("-", 1)[0])
            if owner is None:
                continue
            for rel in self.dep_paths(dep):
                if not rel.startswith(owner + "/"):
                    self.by_package[owner].add(rel)
        self.hashes = {}
        self.code_hashes = {}
        self.texts = {}
        self.names = {}

    @staticmethod
    def metadata(root):
        """The workspace's crates by `cargo metadata`, or None if it can't say."""
        try:
            out = subprocess.run(["cargo", "metadata", "--no-deps", "--offline", "--format-version", "1"],
                                 cwd=root, capture_output=True, check=True, timeout=60).stdout
            m = json.loads(out)
        except (OSError, subprocess.SubprocessError, ValueError):
            return None
        rel = lambda manifest: os.path.relpath(os.path.dirname(os.path.realpath(manifest)), root)
        packages = {}
        for p in m["packages"]:
            packages[rel(p["manifest_path"])] = {
                "name": p["name"],
                "deps": sorted({os.path.relpath(os.path.realpath(d["path"]), root)
                                for d in p["dependencies"] if d.get("path")}),
                "crates": sorted({t["name"].replace("-", "_") for t in p["targets"]}),
            }
        return packages

    @staticmethod
    def dep_files(target):
        deps = os.path.join(target, "release", "deps")
        try:
            return [os.path.join(deps, n) for n in os.listdir(deps) if n.endswith(".d")]
        except OSError:
            return []

    def dep_paths(self, dep):
        """The files in the checkout (not in target/) a dep-info file names, from the root."""
        try:
            with open(dep, encoding="utf-8", errors="replace") as f:
                text = f.read()
        except OSError:
            return []
        out = []
        for word in text.replace("\\ ", "\0").split():
            word = os.path.normpath(os.path.join(self.root, word.rstrip(":").replace("\0", " ")))
            if word.startswith(self.root + os.sep):
                rel = os.path.relpath(word, self.root)
                if not rel.startswith("target" + os.sep) and not rel.endswith(".d"):
                    out.append(rel)
        return out

    def closure(self, package):
        """The workspace's crates `package` (a directory from the root) is built with: it and those
        it depends on, directly or not. All of them if it isn't one of the workspace's crates."""
        if not self.packages or package not in self.packages:
            return set(self.by_package)
        seen, todo = set(), [package]
        while todo:
            d = todo.pop()
            if d not in seen and d in self.packages:
                seen.add(d)
                todo.extend(self.packages[d]["deps"])
        return seen

    def compile_only(self, package):
        return bool(self.packages) and self.packages.get(package, {}).get("name") in COMPILE_ONLY

    def files_of(self, packages):
        return sorted(set(self.common).union(*(self.by_package.get(d, ()) for d in packages)))

    def hash(self, rel):
        if rel not in self.hashes:
            try:
                with open(os.path.join(self.root, rel), "rb") as f:
                    self.hashes[rel] = sha(f.read())
            except OSError:
                self.hashes[rel] = "absent"
        return self.hashes[rel]

    def code_hash(self, rel, tests):
        """The hash of a file's code: a Rust file's by `rust_code`, any other's bytes."""
        if not rel.endswith(".rs"):
            return self.hash(rel)
        if (rel, tests) not in self.code_hashes:
            try:
                with open(os.path.join(self.root, rel), encoding="utf-8") as f:
                    code = sha(rust_code(f.read(), tests).encode())
            except (OSError, ValueError):
                code = self.hash(rel)
            self.code_hashes[(rel, tests)] = code
        return self.code_hashes[(rel, tests)]

    def text(self, rel):
        if rel not in self.texts:
            try:
                with open(os.path.join(self.root, rel), encoding="utf-8", errors="replace") as f:
                    self.texts[rel] = f.read()
            except OSError:
                self.texts[rel] = ""
        return self.texts[rel]

    def names_in(self, rel):
        """The names a Rust file's code (its comments aside) uses as paths (`name::`) or imports
        from its crate or its parent (`use crate::{.., name, ..}`): once a gate."""
        if rel not in self.names:
            code = "\n".join(l for l in self.text(rel).splitlines() if not l.lstrip().startswith("//"))
            names = set(re.findall(r"\b(\w+)::", code))
            for group in re.findall(r"use (?:super|crate)::\{([^}]*)", code):
                names.update(re.findall(r"\b\w+\b", group))
            self.names[rel] = names
        return self.names[rel]

    def for_test(self, files, program_dir, test):
        """Of `files`, those a test's code can come from: all but the modules of its program (a
        test program of several files, `program_dir`) that neither its module nor the program's
        root names. None for `program_dir` means none are left out."""
        if program_dir is None:
            return files
        prefix = program_dir.rstrip("/") + "/"
        modules = {}
        for f in files:
            if f.startswith(prefix) and f.endswith(".rs"):
                rest = f[len(prefix):]
                name = rest.split("/")[0].removesuffix(".rs")
                if name != "main":
                    modules.setdefault(name, []).append(f)
        own = test.split("::")[0] if "::" in test else None
        if own is not None and own not in modules:
            return files  # a module this can't place: keep them all
        wanted, todo = set(), ["main"] + ([own] if own else [])
        while todo:
            name = todo.pop()
            if name in wanted:
                continue
            wanted.add(name)
            for f in [prefix + "main.rs"] if name == "main" else modules.get(name, []):
                todo.extend(other for other in modules
                            if other not in wanted and other in self.names_in(f))
        left_out = {f for name, fs in modules.items() if name not in wanted for f in fs}
        return [f for f in files if f not in left_out]

    def key(self, files, own=None):
        """The files' key, by their code: a Rust file outside `own` (the directory of the package
        whose test runs) without its tests. None for `own` keeps every file's tests."""
        tests = lambda f: own is None or f.startswith(own.rstrip("/") + "/")
        return sha("\n".join(f"{f} {self.code_hash(f, tests(f))}" for f in files))


# ---- the store ------------------------------------------------------------------------------


def write_atomically(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    # Unique across machines too, which share a folder but not their process ids.
    tmp = f"{path}.{os.getpid()}-{time.time_ns() % 10**9}.tmp"
    mode, how = ("w", {"encoding": "utf-8"}) if isinstance(data, str) else ("wb", {})
    with open(tmp, mode, **how) as f:
        f.write(data)
    os.replace(tmp, path)


def stores(environ=os.environ):
    """The memory's directory on this machine, and the shared one it is kept in step with (None if
    none): WRELA_GATE_MEMO's; else, where a Claude project's shared folder can be written, a
    directory in ~/.cache and the folder's `.gate-memo`; else target/gate-memo."""
    own = environ.get("WRELA_GATE_MEMO")
    if own and own != "off":
        return own, None
    shared = environ.get("WRELA_SHARED_FOLDER") or "/mnt/project-files"
    if os.path.isdir(shared) and os.access(shared, os.W_OK):
        return os.path.join(os.path.expanduser("~"), ".cache", "wrela", "gate-memo"), \
            os.path.join(shared, ".gate-memo")
    return os.path.join(environ.get("CARGO_TARGET_DIR", "target"), "gate-memo"), None


# How long a sync may take; what it hasn't copied by then waits for the next one.
SYNC_FOR = 600


def sync(src, dst, lock_dir, deadline=SYNC_FOR):
    """Copies each file of the store `src` that `dst` lacks, or has older, to `dst`, with its time
    kept. One sync from `src` to `dst` at a time (a lock in `lock_dir`, on this machine's disk).
    A read on a shared mount can stall for good, so the files are listed and copied on threads,
    several at once, and the sync ends at `deadline` (seconds) whatever is still stalled. Returns
    how many files it copied."""
    import fcntl
    import threading

    os.makedirs(lock_dir, exist_ok=True)
    lock = open(os.path.join(lock_dir, f".sync-{sha(os.path.abspath(src) + chr(0) + os.path.abspath(dst))[:12]}"), "w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        return 0  # another sync of the same two is under way
    end = time.monotonic() + deadline
    todo, done, copied = [], threading.Event(), [0]

    def listing():
        for dirpath, _, names in os.walk(src):
            todo.extend(os.path.join(dirpath, n) for n in names
                        if not n.endswith(".tmp") and not n.startswith("."))
        done.set()

    def copy():
        while time.monotonic() < end:
            try:
                path = todo.pop()
            except IndexError:
                if done.is_set():
                    return
                time.sleep(0.05)
                continue
            to = os.path.join(dst, os.path.relpath(path, src))
            try:
                when = os.path.getmtime(path)
                if os.path.exists(to) and os.path.getmtime(to) >= when:
                    continue
                with open(path, "rb") as f:
                    data = f.read()
                write_atomically(to, data)
                os.utime(to, (when, when))
                copied[0] += 1
            except OSError:
                continue

    threads = [threading.Thread(target=listing, daemon=True)] + \
        [threading.Thread(target=copy, daemon=True) for _ in range(8)]
    for t in threads:
        t.start()
    for t in threads[1:]:
        t.join(max(0, end - time.monotonic()))
    return copied[0]


def sync_later(src, dst, lock_dir, then=None):
    """Starts a sync of `src` into `dst` (and then of `then`, a (src, dst) pair) in a process of
    its own, and doesn't wait for it."""
    pairs = [(src, dst)] + ([then] if then else [])
    args = [a for pair in pairs for a in pair]
    subprocess.Popen([sys.executable, os.path.abspath(__file__), "sync", lock_dir, *args],
                     start_new_session=True, stdin=subprocess.DEVNULL,
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


class Store:
    """Entries and timings, one file each, under `dir`: shared by the gates of every checkout
    and machine that use the same directory (a file is written whole, then renamed into place)."""

    def __init__(self, dir):
        self.dir = os.path.join(dir, VERSION)

    def timing(self, test):
        """The test's cost: the CPU time it took alone (in the last KEEP_DAYS days), else the time
        it took beside the other tests; None if not known."""
        try:
            with open(os.path.join(self.dir, "timings", sha(test)[:24] + ".json"), encoding="utf-8") as f:
                t = json.load(f)
        except (OSError, ValueError):
            return None
        if not isinstance(t, dict):
            return None
        valid = lambda n: isinstance(n, (int, float)) and not isinstance(n, bool) \
            and 0 <= n < float("inf")
        alone, alone_when = t.get("alone"), t.get("alone_when")
        if valid(alone) and valid(alone_when) and time.time() - alone_when < KEEP_DAYS * 86400:
            return alone
        shared = t.get("shared")
        return shared if valid(shared) else None

    def set_timing(self, test, seconds, alone):
        """Records a test's cost: the CPU time it took `alone`, or the time it took beside the
        other tests. Beside them, a test's time is no measure of its work: it shares the cores,
        and waits for the GPU lock (the lock's own tests wait minutes, using no CPU). So it is
        used only for a test that hasn't run alone lately."""
        path = os.path.join(self.dir, "timings", sha(test)[:24] + ".json")
        try:
            with open(path, encoding="utf-8") as f:
                t = json.load(f)
        except (OSError, ValueError):
            t = {}
        if not isinstance(t, dict):
            t = {}
        if alone:
            t.update(alone=round(seconds, 2), alone_when=round(time.time()))
        else:
            t["shared"] = round(seconds, 2)
        write_atomically(path, json.dumps({**t, "test": test}))

    def entries_dir(self, test, rust):
        return os.path.join(self.dir, "entries", sha(test)[:24], rust[:24])

    def find(self, test, rust, env, inputs_of, also=None):
        """An entry of `test` under the Rust key `rust` that matches the environment `env` (a
        variable's value, or None if unset), each input's state now (`inputs_of(name, how)`) and
        `also(entry)` if given, or None."""
        d = self.entries_dir(test, rust)
        try:
            names = sorted(os.listdir(d), key=lambda n: -os.path.getmtime(os.path.join(d, n)))
        except OSError:
            return None
        for n in names:
            if not n.endswith(".json"):
                continue
            try:
                with open(os.path.join(d, n), encoding="utf-8") as f:
                    entry = json.load(f)
            except (OSError, ValueError):
                continue
            if not isinstance(entry, dict) or entry.get("test") != test:
                continue
            saved_env, inputs = entry.get("env"), entry.get("inputs")
            if not isinstance(saved_env, dict) or not isinstance(inputs, dict):
                continue
            try:
                matches = not any(env(k) != v for k, v in saved_env.items()) \
                    and all(inputs_of(name, state) for name, state in inputs.items()) \
                    and (also is None or also(entry))
            except (KeyError, AttributeError, TypeError, ValueError):
                continue
            if matches:
                try:
                    os.utime(os.path.join(d, n))
                except OSError:
                    pass
                return entry
        return None

    def save(self, test, rust, env, inputs, seconds, builds=()):
        entry = {"test": test, "rust": rust, "env": env, "inputs": inputs, "builds": list(builds),
                 "seconds": round(seconds, 2), "when": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
        body = json.dumps(entry, sort_keys=True, indent=0)
        write_atomically(os.path.join(self.entries_dir(test, rust), sha(body)[:24] + ".json"), body)

    def prune(self):
        """Removes the entries and builds not used or made for KEEP_DAYS days: once a day (a walk
        of the store takes half a second)."""
        mark = os.path.join(self.dir, ".pruned")
        try:
            if time.time() - os.path.getmtime(mark) < 86400:
                return
        except OSError:
            pass
        os.makedirs(self.dir, exist_ok=True)
        with open(mark, "w"):
            pass
        old = time.time() - KEEP_DAYS * 86400
        for top in (os.path.join(self.dir, "entries"), os.path.join(self.dir, "builds")):
            for dirpath, dirnames, filenames in os.walk(top, topdown=False):
                for n in filenames:
                    p = os.path.join(dirpath, n)
                    try:
                        if os.path.getmtime(p) < old:
                            os.remove(p)
                    except OSError:
                        pass
                if dirpath != top:
                    try:
                        os.rmdir(dirpath)
                    except OSError:
                        pass


# ---- one test, alone and traced -------------------------------------------------------------


def under(pid):
    """The live processes under `pid`, from Linux's /proc."""
    children = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            with open(f"/proc/{d}/stat") as f:
                state, ppid = f.read().rsplit(")", 1)[1].split()[:2]
        except (OSError, ValueError):
            continue
        if state != "Z":
            children.setdefault(int(ppid), []).append(int(d))
    found, todo = [], [pid]
    while todo:
        for c in children.get(todo.pop(), []):
            found.append(c)
            todo.append(c)
    return found


def stop_all():
    """Kills every process under this one, and reaps them. A test's process escapes the tree when
    strace, its parent, dies first, so this process is their subreaper: an orphan under it comes
    back to it, and the next pass kills it."""
    for _ in range(10):
        pids = under(os.getpid())
        if not pids:
            return
        for p in pids:
            try:
                os.kill(p, signal.SIGKILL)
            except ProcessLookupError:
                pass
        time.sleep(0.05)
        try:
            while os.waitpid(-1, os.WNOHANG)[0]:
                pass
        except ChildProcessError:
            pass


def subreaper():
    """Makes this process the subreaper of the processes it starts (Linux's prctl)."""
    import ctypes

    PR_SET_CHILD_SUBREAPER = 36
    ctypes.CDLL(None, use_errno=True).prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0)


def isolated_command(exe, test, scratch, tmp, log):
    """The command that runs one test of the program `exe` alone: in a mount namespace of its
    own, where `tmp` (the test programs' scratch directory, target/tmp) is the empty directory
    `scratch`, under strace writing to `log`."""
    script = 'mkdir -p "$1" "$2" && mount --bind "$1" "$2" && shift 2 && exec "$@"'
    trace = ["strace", "-f", "--seccomp-bpf", "-qq", "-y", "-e", "trace=%file", "-o", log]
    return ["unshare", "--mount", "--propagation", "private", "sh", "-c", script, "sh", scratch, tmp,
            *trace, exe, "-q", "--include-ignored", "--exact", test]


class Rebuilds:
    """The builds the entries keyed on their builds' files name, made again now by the `wrela` CLI
    (`wrela build <package> -o <dir> <args>`, the build `recorded_build` says makes the same
    files), each once per gate: each one's files' hash, or None if it failed."""

    def __init__(self, cli, root, out, memo=None):
        self.cli, self.root, self.out, self.memo = cli, root, out, memo
        self.known = {}
        self.cli_key = None

    @staticmethod
    def files_hash(dir):
        """compiler/tests' `files_hash` of the files `wrela build` wrote into `dir` (it lists them
        in `.wrela-build`)."""
        try:
            with open(os.path.join(dir, ".wrela-build"), encoding="utf-8") as f:
                names = sorted(l for l in f.read().splitlines() if l)
            h = hashlib.sha256()
            for name in names:
                with open(os.path.join(dir, name), "rb") as f:
                    data = f.read()
                h.update(name.encode() + b"\0" + len(data).to_bytes(8, "little") + data)
            return h.hexdigest()
        except OSError:
            return None

    def key(self, pkg, args):
        """A build's key in the store: the CLI's bytes, the variables its code reads, the
        package and the arguments."""
        if self.cli_key is None:
            m = self.memo
            texts = [m.sources.text(f) for f in m.sources.files_of(m.sources.closure("compiler/cli"))
                     if f.endswith(".rs")]
            env = m.env_of(env_names(texts) - ENV_NOT)
            self.cli_key = sha(json.dumps([MEANING, m.digests.of(self.cli, "read"), env]))
        return sha(json.dumps([self.cli_key, self.memo.inputs.name(pkg), args]))

    def make(self, pkg, args):
        """The build's files' hash: remembered, if the same CLI read the same files for it
        before; else made, traced, and remembered."""
        if self.cli is None:
            return None
        dir = os.path.join(self.out, sha(json.dumps([pkg, args]))[:16])
        command = [self.cli, "build", pkg, "-o", dir, *args]

        def build():
            shutil.rmtree(dir, ignore_errors=True)
            os.makedirs(self.out, exist_ok=True)
            return command

        if self.memo is None:
            r = subprocess.run(build(), cwd=self.root, capture_output=True)
            return self.files_hash(dir) if r.returncode == 0 else None
        return self.memo.traced(self.key(pkg, args), build, self.root, lambda: self.files_hash(dir))

    def prefetch(self, builds):
        """Makes `builds` ((package, args) pairs) not yet made, as many at once as there are cores."""
        from concurrent.futures import ThreadPoolExecutor
        from resources import cpu_count
        todo = sorted({(p, tuple(a)) for p, a in builds} - set(self.known))
        with ThreadPoolExecutor(max_workers=cpu_count()) as pool:
            for key, h in zip(todo, pool.map(lambda k: self.make(k[0], list(k[1])), todo)):
                self.known[key] = h

    def hash(self, pkg, args):
        key = (pkg, tuple(args))
        if key not in self.known:
            self.known[key] = self.make(pkg, args)
        return self.known[key]


# How an entry keys a test on the Rust sources: on those of its crate and the crates it is built
# with ("crates"); on every crate's, when it ran one of the workspace's programs ("all"); or on
# those of the crates but the compiler's, and the files of each build it made ("built").
SCOPES = ("crates", "all", "built")


class Memo:
    """The memory for one gate: the store, the sources' hashes, the files' states."""

    def __init__(self, root, target, dir, cli=None, sources=None):
        self.root = os.path.realpath(root)
        self.target = os.path.realpath(target)
        self.store = Store(dir)
        self.sources = sources or Sources(self.root, target)
        self.inputs = Inputs(self.root, target, ignore=[dir])
        self.digests = Digests()
        self.rebuilds = Rebuilds(cli, self.root, os.path.join(target, "gate-memo-builds"), self)

    @staticmethod
    def package(test_id):
        return test_id.split(":")[0]

    def files(self, scope, test_id, program_dir, test):
        """The Rust sources a test's entry in `scope` is keyed on."""
        s, pkg = self.sources, self.package(test_id)
        crates = set(s.by_package) if scope == "all" else s.closure(pkg)
        if scope == "built":
            crates = {d for d in crates if d == pkg or not s.compile_only(d)}
        return s.for_test(s.files_of(crates), program_dir, test)

    def rust(self, scope, test_id, program_dir, test):
        # MEANING too: what an entry keeps and how it is matched are part of its meaning.
        return sha(f"{scope}\n{MEANING}\n" + self.sources.key(
            self.files(scope, test_id, program_dir, test), own=self.package(test_id)))

    def env_of(self, names):
        return {n: os.environ.get(n) for n in sorted(names)}

    def code_env_names(self, test_id, program_dir, test, inputs=()):
        """The variables the test's code can read: those its Rust sources and the scripts it read
        name, and the driver's."""
        files = self.files("crates", test_id, program_dir, test)
        texts = [self.sources.text(f) for f in files if f.endswith(".rs")]
        for name in inputs:
            if name.endswith(".py") or name.endswith(".sh"):
                try:
                    with open(self.inputs.path(name), encoding="utf-8", errors="replace") as f:
                        texts.append(f.read())
                except OSError:
                    pass
        names = env_names(texts) | {k for k in os.environ if k.startswith(ENV_PREFIXES)}
        return names - ENV_NOT

    def still(self, name, state):
        how = "list" if state.startswith("list:") else "read"
        return self.digests.of(self.inputs.path(name), how) == state

    def traced(self, key, command, cwd, result, output=None):
        """`result()` after the command `command()` gives ran in `cwd` (None if it failed),
        remembered in the store under `key`, the caller's for the command: when it read the same
        files before, the result then, and it doesn't run. A run whose inputs changed while it
        ran, or that read a path it can't place, isn't remembered. What it read in `output`,
        the directory it writes its result in (listing it to clear it), isn't an input."""
        path = os.path.join(self.store.dir, "builds", key[:24] + ".json")
        try:
            with open(path, encoding="utf-8") as f:
                entry = json.load(f)
            if all(self.still(n, state) for n, state in entry["inputs"].items()):
                os.utime(path)  # used: kept by `prune`
                return entry["result"]
        except (OSError, ValueError, KeyError, AttributeError):
            pass
        fd, log = tempfile.mkstemp(prefix="wrela-traced-")
        os.close(fd)
        started = time.time()
        r = subprocess.run(["strace", "-f", "--seccomp-bpf", "-qq", "-y", "-e", "trace=%file",
                            "-o", log, *command()], cwd=cwd, capture_output=True)
        out = result() if r.returncode == 0 else None
        try:
            if out is not None:
                with open(log, encoding="utf-8", errors="surrogateescape") as f:
                    seen = touched(f.read())
                if output is not None:
                    mine = os.path.abspath(output)
                    seen = {p: h for p, h in seen.items()
                            if p != mine and not p.startswith(mine + os.sep)}
                # Its inputs as they are after it (what it wrote isn't one), unless one changed
                # while it ran.
                if not unplaced(seen) and not self.inputs.changed_since(seen, started):
                    inputs = self.inputs.of(seen, Digests())
                    write_atomically(path, json.dumps({"inputs": inputs, "result": out}))
            os.remove(log)
        except OSError:
            pass
        return out

    def builds_of(self, entry):
        return [(self.inputs.path(b["pkg"]), b["args"]) for b in entry.get("builds", [])]

    def builds_same(self, entry):
        return all(self.rebuilds.hash(p, a) == b["files"]
                   for (p, a), b in zip(self.builds_of(entry), entry.get("builds", [])))

    def remembered(self, test_id, program_dir, test, builds=True):
        """Whether `test` passed before with everything it touched as it is now. Without
        `builds`, only by an entry that made no build; with, by any, each build made again."""
        env = lambda k: os.environ.get(k)
        for scope in SCOPES:
            also = (self.builds_same if builds else lambda e: not e.get("builds")) \
                if scope == "built" else None
            if self.store.find(test_id, self.rust(scope, test_id, program_dir, test), env,
                               self.still, also) is not None:
                return True
        return False

    def builds_wanted(self, test_id, program_dir, test):
        """The builds the entries in scope "built" that match all else name, to make at once."""
        out = []
        rust = self.rust("built", test_id, program_dir, test)

        def note(entry):
            out.extend(self.builds_of(entry))
            return False

        self.store.find(test_id, rust, lambda k: os.environ.get(k), self.still, note)
        return out

    def scope_of(self, test_id, program_dir, test, trace, record):
        """How to key a passing run's entry, and the builds it made if keyed on them."""
        exe = next(iter(trace.ran), None)
        if any(p != exe and p.startswith(self.target + os.sep) for p in trace.ran):
            return "all", []
        pkg = self.package(test_id)
        if self.sources.compile_only(pkg) or any(r.get("outside") for r in record):
            return "crates", []
        builds = []
        for r in record:
            if r.get("args") is None or not self.inputs.kept(r["pkg"]) or \
                    not r["pkg"].startswith(self.root + os.sep):
                return "crates", []  # a build no command makes, or of a package it wrote
            b = {"pkg": self.inputs.name(r["pkg"]), "args": r["args"], "files": r["files"]}
            if b not in builds:
                builds.append(b)
        # Wrela sources the test read outside its builds: the compiler may have read them too.
        for path, how in trace.seen.items():
            if how != "write" and is_wrela(path) and path not in trace.in_builds_only:
                return "crates", []
        own = [f for f in self.files("crates", test_id, program_dir, test)
               if f.startswith(pkg + "/") and f.endswith(".rs")]
        if any(NAMES_COMPILER.search(self.sources.text(f)) for f in own):
            return "crates", []
        return "built", builds

    def remember(self, test_id, program_dir, test, log_text, seconds, record_text=""):
        """Keeps a passing run's entry, from its strace log and its builds' record: unless a file
        it read changed while it ran, or it named one the trace can't place. Its scope, or None
        when it keeps none."""
        started = time.time() - seconds
        trace = Trace(log_text)
        if unplaced(trace.seen) or self.inputs.changed_since(trace.seen, started):
            return None
        record = [json.loads(l) for l in record_text.splitlines() if l.strip()]
        scope, builds = self.scope_of(test_id, program_dir, test, trace, record)
        seen = trace.seen
        if scope == "built":
            # What only the builds read is the builds' inputs: their files stand for them.
            seen = {p: h for p, h in seen.items()
                    if not (p in trace.in_builds_only and (is_wrela(p) or h in ("list", "stat")))}
        # As they are after it ran, not when the gate began.
        inputs = self.inputs.of(seen, Digests())
        rust = self.rust(scope, test_id, program_dir, test)
        env = self.env_of(self.code_env_names(test_id, program_dir, test, inputs))
        self.store.save(test_id, rust, env, inputs, seconds, builds=builds)
        return scope


def is_wrela(path):
    return path.endswith(".wrela") or os.path.basename(path) == "wrela.toml"


# ---- the gate's tests, the slow ones alone -------------------------------------------------


def run(logs, native_cache, mode, dir, args):
    """Runs the gate's tests (`tools/check.sh`'s `run_tests`, with `args`): each test program at
    once for its quick tests, and each slow test alone, traced, the longest first, unless it is
    remembered (mode "on"; "fresh" runs every one and keeps what it learns). Prints what failed,
    what was remembered, and how many passed; returns the exit status."""
    # A gate that ends early (its tools/check.sh killed, its terminal closed) takes its tests with
    # it: left running, they'd keep the machine's cores busy for every later gate.
    parent = os.getppid()
    subreaper()

    def end(signum, _frame):
        stop_all()
        sys.exit(128 + signum)

    for signum in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, end)
    # The shared store: what the other machines learned comes into this machine's copy, for the
    # next gate (this one doesn't wait for it); what this one learns goes back once it ends.
    if dir == "auto":
        dir, shared = stores()
    else:
        shared = None
    if shared:
        sync_later(shared, dir, dir)
    try:
        return run_tests(logs, native_cache, mode, dir, args, parent)
    except KeyboardInterrupt:
        stop_all()
        return 130
    except BaseException:
        stop_all()
        raise
    finally:
        if shared:
            sync_later(dir, shared, dir, then=(shared, dir))


def run_tests(logs, native_cache, mode, dir, args, parent):
    root = os.getcwd()
    target = os.path.abspath(os.environ.get("CARGO_TARGET_DIR", "target"))
    native_cache = os.path.abspath(native_cache)
    programs, cli = [], None
    with open(f"{logs}/tests.json") as artifacts:
        for line in artifacts:
            m = json.loads(line)
            if m.get("reason") == "compiler-artifact" and m.get("executable"):
                if m["profile"]["test"]:
                    programs.append(m)
                elif m["target"]["kind"] == ["bin"] and m["target"]["name"] == "wrela":
                    cli = m["executable"]  # built from the sources the tests are
    memo = Memo(root, target, dir, cli)
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import runner
    from resources import cpu_count

    cores = cpu_count()
    # Each slow test's scratch directory (its target/tmp), kept after a failure to look at.
    alone_root = os.path.join(target, "tmp-alone")
    shutil.rmtree(alone_root, ignore_errors=True)

    def env_for(pkg):
        # As tools/runner.py runs a program, for a slow test alone (its scratch is target/tmp,
        # its own in its mount namespace).
        return {**os.environ, "CARGO_MANIFEST_DIR": pkg, "WRELA_NATIVE_CACHE": native_cache,
                "WRELA_GPU_SHARED": str(runner.gpu_shared(cores)),
                "WRELA_GPU_WAITS": f"{logs}/gpu-waits"}

    def listed(exe, pkg, more):
        out = subprocess.run([exe, "--list", "--format", "terse", *args, *more], cwd=pkg,
                             env=env_for(pkg), capture_output=True, text=True, check=True).stdout
        return [l[:-len(": test")] for l in out.splitlines() if l.endswith(": test")]

    # Each program's tests (listed all at once); the slow ones (by their last time) apart.
    def split(m):
        pkg = os.path.dirname(m["manifest_path"])
        exe = m["executable"]
        src = m["target"]["src_path"]
        # A test program of several files (tests/<name>/main.rs): its modules apart.
        program_dir = None
        if os.path.basename(src) == "main.rs" and os.path.basename(os.path.dirname(src)) != "src":
            program_dir = os.path.relpath(os.path.dirname(src), root)
        prefix = f"{os.path.relpath(pkg, root)}:{m['target']['name']}:{m['target']['kind'][0]}:"
        names = listed(exe, pkg, [])
        slow = [n for n in names if (memo.store.timing(prefix + n) or 0) >= SLOW]
        skips = [a for n in slow for a in ("--skip", n)]
        # A skip holds a name, not only matches it: a test whose name holds a slow one's would be
        # skipped too, so it runs alone as well.
        kept = set(listed(exe, pkg, skips)) if slow else set(names)
        aside = [(prefix + n, n, n in slow) for n in names if n not in kept]
        return m, pkg, exe, program_dir, prefix, skips, kept, aside

    from concurrent.futures import ThreadPoolExecutor
    with ThreadPoolExecutor(max_workers=cores) as pool:
        split_programs = list(pool.map(split, programs))

    # The quick tests: bounded by the available CPUs, the end-to-end suite first,
    # each test's time in libtest's JSON (`--report-time`, a stable libtest's unstable option:
    # RUSTC_BOOTSTRAP). They start now, while the slow tests' memory is looked up beside them.
    # This bounds processes, not every thread: runner.start keeps up to four test threads in
    # each program, so work can continue while a test waits for its shared GPU slot.
    runs, scratch_root = [], runner.scratch_root()
    scratches = {}
    quick = [p for p in sorted(split_programs, key=lambda p: not runner.is_first(p[0])) if p[6]]

    def start_quick():
        m, pkg, _, _, prefix, skips, _, _ = quick.pop(0)
        command, env, _, scratch = runner.start(m, scratch_root, logs, native_cache, cores)
        out = open(f"{logs}/run-{len(runs)}", "w+")
        proc = subprocess.Popen(
            [*command, *args, *skips, "-Z", "unstable-options", "--format", "json",
             "--report-time"], cwd=pkg, env={**env, "RUSTC_BOOTSTRAP": "1"},
            stdout=out, stderr=subprocess.STDOUT)
        scratches[prefix] = scratch
        runs.append(("quick", runner.name_of(m), proc, out, time.monotonic(), prefix))

    while quick and len(runs) < cores:
        start_quick()

    # The slow tests remembered, or to run alone. Those remembered by their builds' files once the
    # builds are made again, all at once.
    alone, remembered, maybe = [], [], []
    for _, pkg, exe, program_dir, _, _, _, aside in split_programs:
        for test_id, n, slow in aside:
            if mode == "on" and slow and memo.remembered(test_id, program_dir, n, builds=False):
                remembered.append(n)
            else:
                maybe.append((memo.store.timing(test_id) or 0, test_id, n, exe, pkg, program_dir))
    by_builds = []
    if mode == "on" and maybe:
        memo.rebuilds.prefetch([b for _, t, n, _, _, d in maybe for b in memo.builds_wanted(t, d, n)])
    for a in maybe:
        _, test_id, n, _, _, program_dir = a
        if mode == "on" and (memo.store.timing(test_id) or 0) >= SLOW and \
                memo.remembered(test_id, program_dir, n):
            by_builds.append(n)
        else:
            alone.append(a)

    # The slow tests: the longest first, sharing the CPU process budget with quick programs.
    alone.sort(key=lambda a: -a[0])
    todo = list(alone)
    started = {}

    def start_alone():
        _, test_id, n, exe, pkg, program_dir = todo.pop(0)
        i = len(started)
        scratch = os.path.join(alone_root, str(i))
        log = f"{logs}/trace-{i}"
        out = open(f"{logs}/alone-{i}", "w+")
        record = f"{logs}/builds-{i}.jsonl"
        proc = subprocess.Popen(isolated_command(exe, n, scratch, os.path.join(target, "tmp"), log),
                                cwd=pkg, env={**env_for(pkg), "WRELA_BUILD_RECORD": record},
                                stdout=out, stderr=subprocess.STDOUT)
        started[test_id] = (n, program_dir, log, scratch, record)
        runs.append(("alone", n, proc, out, time.monotonic(), test_id))

    failed, ran = False, 0
    waiting = list(runs)
    def fill_workers():
        while len(waiting) < cores and (todo or quick):
            if todo:
                start_alone()
            else:
                start_quick()
            waiting.append(runs[-1])

    fill_workers()
    cpu = {}

    def done(r):
        """Whether a run has ended. A slow test's is reaped here, for its CPU time (its process's
        and its children's: strace's, and the test's under it)."""
        if r[0] == "quick":
            return r[2].poll() is not None
        pid, status, usage = os.wait4(r[2].pid, os.WNOHANG)
        if pid == 0:
            return False
        r[2].returncode = os.waitstatus_to_exitcode(status)
        cpu[r[5]] = usage.ru_utime + usage.ru_stime
        return True

    while waiting:
        time.sleep(0.05)
        if os.getppid() != parent:
            print("the gate that ran these tests has ended: stopping them", file=sys.stderr)
            stop_all()
            return 1
        for r in [r for r in waiting if done(r)]:
            waiting.remove(r)
            kind, name, proc, out, start, extra = r
            took = time.monotonic() - start
            out.seek(0)
            text = out.read()
            if kind == "quick":
                passed, failures, other = 0, [], []
                for line in text.splitlines():
                    try:
                        e = json.loads(line)
                    except ValueError:
                        other.append(line)
                        continue
                    if e.get("type") != "test":
                        continue
                    if e.get("event") == "ok":
                        passed += 1
                        if e.get("exec_time", 0) >= 1:
                            memo.store.set_timing(extra + e["name"], e["exec_time"], alone=False)
                    elif e.get("event") == "failed":
                        failures.append(e)
                ran += passed
                if proc.returncode != 0:
                    failed = True
                    for e in failures:
                        print(e.get("stdout", ""))
                        print(f"test {e['name']} failed")
                    if not failures:
                        print("\n".join(other))
                    print(f"{name} failed; scratch kept at {scratches[extra]} (for six hours)")
                else:
                    shutil.rmtree(scratches[extra], ignore_errors=True)
                    if passed and took >= 1:
                        print(f"  {name}: {took:.1f}s")
            else:
                n, program_dir, log, scratch, record = started[extra]
                passed = re.search(r"test result: ok\. 1 passed", text) is not None
                if proc.returncode != 0 or not passed:
                    failed = True
                    print(text)
                    print(f"{name} failed (alone; its files are in {scratch})")
                else:
                    ran += 1
                    with open(log, encoding="utf-8", errors="surrogateescape") as f:
                        text = f.read()
                    try:
                        with open(record, encoding="utf-8") as f:
                            built = f.read()
                    except OSError:
                        built = ""
                    kept = memo.remember(extra, program_dir, n, text, took, built)
                    memo.store.set_timing(extra, cpu[extra], alone=True)
                    shutil.rmtree(scratch, ignore_errors=True)
                    print(f"  {name}: {took:.1f}s, alone ({cpu[extra]:.0f}s of CPU)"
                          + ("" if kept else "; not remembered: a file it read changed as it ran"))
            out.close()
        fill_workers()
    if remembered or by_builds:
        print(f"  {len(remembered) + len(by_builds)} slow tests passed before with everything they read "
              f"as it is now, so they didn't run (tools/check.sh --fresh runs them): "
              f"{', '.join(sorted(remembered + by_builds))}")
    if by_builds:
        print(f"    of those, {len(by_builds)} by their builds' files, made again the same: "
              f"{', '.join(sorted(by_builds))}")
    print(f"  {ran} tests passed")
    memo.store.prune()
    return tests_status(failed, ran, remembered, by_builds)


def tests_status(failed, ran, remembered, by_builds):
    """The gate's status after tests ran or were accepted from either kind of memory."""
    return 1 if failed or ran + len(remembered) + len(by_builds) == 0 else 0


def meaning():
    """A hash of the code that decides what an entry keeps and how it is matched: what a trace
    gives, which inputs and sources count, the keys, the builds' check. Its syntax tree, without
    comments or docstrings, so that a change to how it reads alone doesn't drop every entry."""
    import ast
    import inspect
    import textwrap

    deciding = [unescape, lines, events, touched, unplaced, Trace, Digests, Inputs, env_names,
                rust_code, Sources, Store.entries_dir, Store.find, Store.save, Rebuilds, Memo, is_wrela]
    constants = [CALLS, WRITE_FLAGS, LINE.pattern, UNFINISHED.pattern, RESUMED.pattern,
                 STRING.pattern, RESULT_PATH.pattern, BUILD_BEGIN, BUILD_END, ENV_NAMES,
                 ENV_PREFIXES, ENV_NOT, COMPILE_ONLY, NAMES_COMPILER.pattern, SCOPES]
    parts = [repr(sorted(c) if isinstance(c, (set, dict)) else c) for c in constants]
    for o in deciding:
        tree = ast.parse(textwrap.dedent(inspect.getsource(o)))
        for node in ast.walk(tree):
            body = getattr(node, "body", None)
            if isinstance(body, list) and body and isinstance(body[0], ast.Expr) and \
                    isinstance(body[0].value, ast.Constant) and isinstance(body[0].value.value, str):
                node.body = body[1:] or [ast.Pass()]
        parts.append(ast.dump(tree))
    return sha("\n".join(parts))[:16]


MEANING = meaning()


if __name__ == "__main__":
    if len(sys.argv) >= 6 and sys.argv[1] == "run":
        _, _, logs, native_cache, mode, dir, *rest = sys.argv
        sys.exit(run(logs, native_cache, mode, dir, rest))
    if len(sys.argv) == 2 and sys.argv[1] == "available":
        sys.exit(0 if available() else 1)
    if len(sys.argv) >= 5 and sys.argv[1] == "sync" and len(sys.argv) % 2 == 1:
        _, _, lock_dir, *pairs = sys.argv
        for i in range(0, len(pairs), 2):
            sync(pairs[i], pairs[i + 1], lock_dir)
        os._exit(0)  # not waiting for a read the mount stalled
    if len(sys.argv) == 2 and sys.argv[1] == "pull":
        own, shared = stores()
        if shared:
            sync_later(shared, own, own)
        sys.exit(0)
    print("usage: tools/memo.py run <logs> <native cache> on|fresh <dir>|auto <test args>... "
          "| available | pull | sync <lock dir> <from> <to>...", file=sys.stderr)
    sys.exit(2)
