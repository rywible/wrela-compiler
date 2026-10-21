#!/usr/bin/env python3
"""A toolchain release for this machine (language.md §3.6, `toolchain`).

    tools/package.py --version X.Y.Z --target <triple> --out <dir>

Builds `wrela`, `wrela-host` and `wrela-serve` in release mode, with the source revision
(`git rev-parse HEAD`) embedded so that `wrela --version` says it, and writes into <dir>:

- `wrela-X.Y.Z-<triple>.tar.gz`: `bin/wrela`, `bin/wrela-host`, `bin/wrela-serve` and
  `bin/wasm-opt` (Binaryen's, with the library it needs in `lib/` on macOS), `LICENSE`,
  `LICENSE.binaryen` and `VERSION`. The same files give the same bytes.
- `SHA256SUMS`, the tarball's line.

`wrela` embeds std and the browser runtime, so the tarball is the whole toolchain. Binaryen's
`wasm-opt` comes from its GitHub release by exact URL and is checked against a pinned SHA-256;
`wrela build --optimize` uses this one when it is beside `wrela`. The build goes in
`target/package`, apart from the edit loop's `target/release`, so the embedded revision never
rebuilds that. The downloads are kept in `target/package/downloads`.
"""

import argparse
import gzip
import hashlib
import io
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
BINARIES = ("wrela", "wrela-host", "wrela-serve")
BINARYEN = "version_133"
# Each target's Binaryen build and its SHA-256 (the release's own `.sha256` files say the same).
BINARYEN_BUILDS = {
    "x86_64-unknown-linux-gnu": (
        "x86_64-linux", "2dc9c7813f5375db93d96ead4b78222fcc3e2677bbb832297af4797782a37489"),
    "aarch64-apple-darwin": (
        "arm64-macos", "ad66da82ac13f163e424b1643f16c6dfcccc98b5966296b43e52d3cab04f84a8"),
}
# Binaryen's licence (Apache 2.0), which its release tarballs don't hold, at the release's tag.
BINARYEN_LICENSE = (
    f"https://raw.githubusercontent.com/WebAssembly/binaryen/{BINARYEN}/LICENSE",
    "64c0d02491e16eced74826440ecd2bcf7722d37fa586faf457b2494293afbffe",
)


def fail(message):
    sys.exit(f"error: {message}")


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def fetch(url, digest, keep):
    """The bytes at `url`, which must have SHA-256 `digest`; kept at `keep`, and read from there
    when it already holds them."""
    if keep.is_file() and sha256(keep.read_bytes()) == digest:
        return keep.read_bytes()
    print(f"downloading {url}", file=sys.stderr)
    with urllib.request.urlopen(url, timeout=300) as response:
        data = response.read()
    if sha256(data) != digest:
        raise ValueError(f"{url} has SHA-256 {sha256(data)}, not the pinned {digest}")
    keep.parent.mkdir(parents=True, exist_ok=True)
    keep.write_bytes(data)
    return data


def binaryen(target, downloads):
    """Binaryen's files for `target`: `(path, mode, bytes)` for `bin/wasm-opt` and the shared
    libraries it loads from `lib/`, and its licence's bytes."""
    build, digest = BINARYEN_BUILDS[target]
    name = f"binaryen-{BINARYEN}-{build}.tar.gz"
    url = f"https://github.com/WebAssembly/binaryen/releases/download/{BINARYEN}/{name}"
    data = fetch(url, digest, downloads / name)
    files = []
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
        for member in tar.getmembers():
            parts = member.name.split("/")
            if not member.isfile() or len(parts) != 3:
                continue
            wanted = parts[1:] == ["bin", "wasm-opt"] or (
                parts[1] == "lib" and re.search(r"\.(dylib|so(\.\d+)*)$", parts[2]))
            if wanted:
                files.append((f"{parts[1]}/{parts[2]}", 0o755, tar.extractfile(member).read()))
    if not any(path == "bin/wasm-opt" for path, _, _ in files):
        fail(f"{name} has no bin/wasm-opt")
    url, digest = BINARYEN_LICENSE
    return files, fetch(url, digest, downloads / f"binaryen-{BINARYEN}-LICENSE")


def pack(out, version, target, files):
    """Writes `wrela-<version>-<target>.tar.gz` of `files` (each `(path, mode, bytes)`) and its
    `SHA256SUMS` into `out`: the tarball's path. The same files give the same bytes."""
    name = f"wrela-{version}-{target}.tar.gz"
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.PAX_FORMAT) as tar:
        dirs = sorted({str(Path(p).parent) for p, _, _ in files} - {"."})
        for d in dirs:
            info = tarfile.TarInfo(d)
            info.type, info.mode = tarfile.DIRTYPE, 0o755
            tar.addfile(info)
        for path, mode, data in sorted(files):
            info = tarfile.TarInfo(path)
            info.size, info.mode = len(data), mode
            tar.addfile(info, io.BytesIO(data))
    packed = gzip.compress(raw.getvalue(), compresslevel=9, mtime=0)
    out.mkdir(parents=True, exist_ok=True)
    (out / name).write_bytes(packed)
    (out / "SHA256SUMS").write_text(f"{sha256(packed)}  {name}\n")
    return out / name


def cargo_version():
    text = (ROOT / "Cargo.toml").read_text()
    found = re.search(r'^\[workspace\.package\][^\[]*?^version = "([^"]+)"', text, re.M | re.S)
    if not found:
        fail("Cargo.toml's [workspace.package] has no version")
    return found.group(1)


def output(*command):
    return subprocess.run(command, cwd=ROOT, check=True, capture_output=True,
                          text=True).stdout.strip()


def main(argv):
    parser = argparse.ArgumentParser(description="Builds a toolchain release for this machine.")
    parser.add_argument("--version", required=True, help="the release, X.Y.Z: Cargo's version")
    parser.add_argument("--target", required=True, choices=sorted(BINARYEN_BUILDS),
                        help="this machine's target triple")
    parser.add_argument("--out", required=True, type=Path, help="where the release goes")
    args = parser.parse_args(argv)
    if args.version != cargo_version():
        fail(f"the release is {args.version}, but Cargo's version is {cargo_version()}")
    host = re.search(r"^host: (\S+)$", output("rustc", "-vV"), re.M).group(1)
    if args.target != host:
        fail(f"this machine builds {host}, not {args.target}")
    revision = output("git", "rev-parse", "HEAD")
    if output("git", "status", "--porcelain"):
        print(f"warning: the checkout has changes that {revision[:12]} doesn't", file=sys.stderr)
    target_dir = ROOT / "target/package"
    command = ["cargo", "build", "--release", "--target-dir", str(target_dir), "--bins"]
    for b in BINARIES:
        command += ["-p", b]
    subprocess.run(command, cwd=ROOT, check=True, env={**os.environ, "WRELA_REVISION": revision})
    built = target_dir / "release"
    said = output(str(built / "wrela"), "--version")
    if said != f"wrela {args.version} ({revision})":
        fail(f"the built `wrela --version` says {said!r}")
    files = [(f"bin/{b}", 0o755, (built / b).read_bytes()) for b in BINARIES]
    tools, licence = binaryen(args.target, target_dir / "downloads")
    files += tools
    files += [("LICENSE", 0o644, (ROOT / "LICENSE").read_bytes()),
              ("LICENSE.binaryen", 0o644, licence),
              ("VERSION", 0o644, f"{args.version}\n{revision}\n".encode())]
    tarball = pack(args.out, args.version, args.target, files)
    print(f"wrote {tarball} and {args.out / 'SHA256SUMS'}")


if __name__ == "__main__":
    main(sys.argv[1:])
