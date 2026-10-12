#!/usr/bin/env python3
"""Build static Wrela console/game releases through shared admission; requires Pillow."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
MAX_FILE = 25 * 1024 * 1024
MAX_FILES = 20_000


def component(value):
    if not isinstance(value, str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", value):
        raise ValueError(f"invalid release component: {value!r}")
    return value


def origin(domain):
    if not isinstance(domain, str) or not re.fullmatch(r"[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?", domain):
        raise ValueError("invalid custom domain")
    if ".." in domain or "." not in domain:
        raise ValueError("invalid custom domain")
    return "https://" + domain


def read_config(path):
    data = tomllib.loads(Path(path).read_text())
    seen = set()
    component(data["console"]["project"])
    origin(data["console"]["domain"])
    source = (ROOT / data["console"]["package"]).resolve()
    if not source.is_relative_to(ROOT) or not (source / "wrela.toml").is_file():
        raise ValueError("the console package is not inside the repository")
    for game in data["games"]:
        key = component(game["id"])
        if key in seen:
            raise ValueError("duplicate game id")
        seen.add(key)
        component(game["project"])
        origin(game["domain"])
        component(game["session"])
        if any(c in game["title"] for c in "\t\r\n") or not game["title"]:
            raise ValueError("invalid catalog title")
        if game["session"] not in game["builds"]:
            raise ValueError("the session build is missing")
        for name, package in game["builds"].items():
            component(name)
            source = (ROOT / package).resolve()
            if not source.is_relative_to(ROOT) or not (source / "wrela.toml").is_file():
                raise ValueError("a build package is not inside the repository")
    if not seen:
        raise ValueError("the release needs at least one game")
    return data


def headers(parent, children=(), embedded=False):
    # Assets are public. The origin-owned OPFS is never uploaded or served.
    ancestors = parent if embedded else "'none'"
    frame_src = " ".join(children) if children else "'none'"
    policy = "cross-origin-isolated=(self" + "".join(f' "{c}"' for c in children) + ")"
    return f"""/*
  Cross-Origin-Opener-Policy: same-origin
  Cross-Origin-Embedder-Policy: require-corp
  Cross-Origin-Resource-Policy: cross-origin
  Permissions-Policy: {policy}
  X-Content-Type-Options: nosniff
  Referrer-Policy: origin
  Content-Security-Policy: default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; worker-src 'self' blob:; connect-src 'self'; img-src 'self' blob:; frame-src {frame_src}; frame-ancestors {ancestors}; object-src 'none'; base-uri 'self'
/*.wasm
  Content-Type: application/wasm
"""


def release_files(directory):
    files = []
    for path in sorted(directory.rglob("*")):
        if path.is_symlink():
            raise ValueError("release assets cannot be symbolic links")
        if not path.is_file():
            continue
        relative = path.relative_to(directory)
        if any(part in {"storage", "results", ".native", "node_modules"} for part in relative.parts):
            raise ValueError(f"private/runtime output in release: {relative}")
        if any(part.startswith(".") for part in relative.parts):
            raise ValueError(f"hidden build metadata in release: {relative}")
        if path.stat().st_size > MAX_FILE:
            raise ValueError(f"Cloudflare file limit exceeded: {relative}")
        files.append(path)
    if len(files) > MAX_FILES:
        raise ValueError("Cloudflare file-count limit exceeded")
    return files


def copy_licenses(package, output):
    shutil.copy2(ROOT / "LICENSE", output / "LICENSE")
    for license_file in (ROOT / package / "fonts").glob("OFL-*.txt"):
        shutil.copy2(license_file, output / license_file.name)


def build_package(package, output):
    subprocess.run([str(ROOT / "tools/wrela"), "build", str(ROOT / package), "-o", str(output)],
                   cwd=ROOT, check=True)


def retain_releases(previous, target):
    """Keep pinned assets for clients that still hold an older console/catalog."""
    if not previous.exists():
        return
    release_files(previous)
    source = previous / "releases"
    if not source.exists():
        return
    for version in source.iterdir():
        if not version.is_dir() or not re.fullmatch(r"[a-f0-9]{20}", version.name):
            raise ValueError("invalid retained release")
        destination = target / "releases" / version.name
        if destination.exists():
            old = {str(p.relative_to(version)): p.read_bytes() for p in release_files(version)}
            new = {str(p.relative_to(destination)): p.read_bytes() for p in release_files(destination)}
            if old != new:
                raise ValueError("an immutable release cannot change")
        else:
            shutil.copytree(version, destination)


def package(config, output, covers, previous=None):
    """Only this invocation's empty output is populated; never copy game storage/captures."""
    if output.exists() and any(output.iterdir()):
        raise ValueError("release output must be empty; choose a new directory")
    output.mkdir(parents=True, exist_ok=True)
    console = config["console"]
    home = output / console["project"]
    build_package(console["package"], home)
    copy_licenses(console["package"], home)
    parent = origin(console["domain"])
    catalog = []
    packages = [home]
    for game in config["games"]:
        game_output = output / game["project"]
        game_output.mkdir()
        copy_licenses("", game_output)
        for name, source in game["builds"].items():
            build_package(source, game_output / name)
        child = origin(game["domain"])
        packages.append(game_output)
        (game_output / "_headers").write_text(headers(parent, embedded=True))
        (game_output / "_redirects").write_text(f"/ {parent}/ 302\n")
        cover = covers.get(game["id"])
        if not cover or not cover.is_file():
            raise ValueError(f"missing Wrela-rendered cover for {game['id']}")
        from PIL import Image
        with Image.open(cover) as image:
            image = image.convert("RGBA")
            image.thumbnail((960, 540), Image.Resampling.LANCZOS)
            width, height = image.size
            pixels = image.tobytes()
        filename = "cover-" + hashlib.sha256(pixels).hexdigest()[:16] + ".rgba"
        (home / filename).write_bytes(pixels)
        catalog.append("\t".join([game["id"], game["title"], child + "/" + game["session"] + "/",
                                   filename, str(width), str(height)]))
    # Fingerprint complete compiled output. Game URLs remain immutable across deployments,
    # while a generic storage namespace in each bootstrap stays stable across releases.
    digest = hashlib.sha256(json.dumps(config, sort_keys=True).encode())
    for directory in packages:
        for marker in directory.rglob(".wrela-build"):
            marker.unlink()
        for file in sorted(directory.rglob("*")):
            if file.is_file():
                digest.update(str(file.relative_to(output)).encode())
                digest.update(file.read_bytes())
    release = digest.hexdigest()[:20]
    for index, line in enumerate(catalog):
        parts = line.split("\t")
        game = next(g for g in config["games"] if g["id"] == parts[0])
        parts[2] = origin(game["domain"]) + "/releases/" + release + "/" + game["session"] + "/"
        catalog[index] = "\t".join(parts)
    (home / "catalog.tsv").write_text("\n".join(catalog) + "\n")
    (home / "_headers").write_text(headers(parent, [origin(g["domain"]) for g in config["games"]]))
    page = home / "index.html"
    page.write_text(page.read_text().replace("background: #000;", "background: #14223A;"))
    for project in [console, *config["games"]]:
        directory = output / project["project"]
        namespace = "console" if project is console else project["id"]
        if project is console:
            entry = (directory / "index.html").read_text()
            entry = entry.replace("<head>", f'<head>\n<base href="/releases/{release}/">')
            entry = entry.replace("<head>", '<head>\n<meta name="wrela-storage" content="console">')
        else:
            entry = None
            for document in directory.rglob("index.html"):
                document.write_text(document.read_text().replace("<head>", f'<head>\n<meta name="wrela-storage" content="{namespace}">'))
        versioned = directory / "releases" / release
        versioned.mkdir(parents=True)
        for asset in list(directory.iterdir()):
            if asset.name not in {"releases", "_headers", "_redirects"}:
                asset.rename(versioned / asset.name)
        if entry is not None:
            (directory / "index.html").write_text(entry)
        header_file = directory / "_headers"
        header_file.write_text(header_file.read_text() + "/releases/*\n  Cache-Control: public, max-age=31536000, immutable\n/\n  Cache-Control: no-cache\n/index.html\n  Cache-Control: no-cache\n")
    reports = []
    for project in [console, *config["games"]]:
        target = output / project["project"]
        if previous is not None:
            retain_releases(previous / project["project"], target)
        files = release_files(target)
        reports.append({"project": project["project"], "domain": project["domain"],
                        "files": len(files), "bytes": sum(p.stat().st_size for p in files)})
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    report = {"source": commit, "release": release, "projects": reports}
    (output / "release.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=ROOT / "examples/console.toml")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--previous", type=Path, help="prior verified release bundle, to retain pinned assets")
    parser.add_argument("--cover", action="append", default=[], metavar="GAME=PNG")
    args = parser.parse_args()
    # Admission covers all builds and packaging; nested tools/wrela sees the same lease.
    from jobs import admitted, run
    if not admitted():
        return run("build", [sys.executable, str(Path(__file__).resolve()), *sys.argv[1:]])
    covers = {}
    for value in args.cover:
        key, sep, path = value.partition("=")
        if not sep:
            parser.error("--cover must be GAME=PNG")
        covers[component(key)] = Path(path).resolve()
    try:
        report = package(read_config(args.config), args.output.resolve(), covers, args.previous.resolve() if args.previous else None)
    except (ValueError, KeyError, OSError, ImportError, subprocess.CalledProcessError) as error:
        print(f"console release: {error}", file=sys.stderr)
        return 1
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
