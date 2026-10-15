#!/usr/bin/env python3
"""Build static Wrela console/game releases for Cloudflare Workers static assets; requires Pillow
and Binaryen's wasm-opt. tools/deploy.py deploys one."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
MAX_FILE = 25 * 1024 * 1024
MAX_FILES = 20_000
# The project wrapper owns these paths beside its game build directories.
RESERVED_BUILD_NAMES = {"releases", "_headers", "_redirects", "index.html", "LICENSE", "deploys.json",
                        "files.json"}
# The project's own hosts beside the console's domain, which no game may take.
RESERVED_HOSTS = {"www", "docs", "api", "studio"}
# Covers ship as WebP at this quality: a 960x540 cover is 30 to 110 KB, not 2 MB of pixels.
COVER_QUALITY = 90
# The Workers runtime each release's Workers run on.
COMPATIBILITY_DATE = "2026-10-12"


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
    workers = {component(data["console"]["worker"])}
    domains = {data["console"]["domain"]}
    origin(data["console"]["domain"])
    source = (ROOT / data["console"]["package"]).resolve()
    if not source.is_relative_to(ROOT) or not (source / "wrela.toml").is_file():
        raise ValueError("the console package is not inside the repository")
    for game in data["games"]:
        key = component(game["id"])
        if key in seen:
            raise ValueError("duplicate game id")
        seen.add(key)
        if component(game["worker"]) in workers or game["domain"] in domains:
            raise ValueError(f"{key} shares a worker or a domain")
        workers.add(game["worker"])
        domains.add(game["domain"])
        origin(game["domain"])
        if game["domain"].split(".")[0] in RESERVED_HOSTS:
            raise ValueError(f"{game['domain']} is one of the project's own hosts")
        component(game["session"])
        if any(c in game["title"] for c in "\t\r\n") or not game["title"]:
            raise ValueError("invalid catalog title")
        if game["session"] not in game["builds"]:
            raise ValueError("the session build is missing")
        cover = (ROOT / game["cover"]).resolve()
        if not cover.is_relative_to(ROOT) or not cover.is_file():
            raise ValueError(f"missing Wrela-rendered cover for {key}")
        for name, package in game["builds"].items():
            if name in RESERVED_BUILD_NAMES:
                raise ValueError(f"reserved build name: {name!r}")
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
/*.wgsl
  Content-Type: text/plain; charset=utf-8
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
    # Released WASM goes through Binaryen; its trap locations go beside it, as game.lines.
    subprocess.run([str(ROOT / "tools/wrela"), "build", str(ROOT / package), "-o", str(output),
                    "--optimize"], cwd=ROOT, check=True)


def file_list(directory):
    """Each file under `directory` and its SHA-256, by path: what a later deploy fetches to keep it."""
    return {str(path.relative_to(directory)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in release_files(directory)}


def prefetch_list(game, output, release):
    """What the console fetches of a game ahead of time, once it is idle: each build's page,
    runtime, WASM and shaders, the session first. Not the files a game streams as it plays, nor
    its trap locations, which only a crash reads."""
    root = origin(game["domain"]) + "/releases/" + release + "/"
    builds = sorted(game["builds"], key=lambda name: name != game["session"])
    urls = []
    for name in builds:
        directory = output / game["worker"] / name
        for path in sorted(release_files(directory)):
            relative = path.relative_to(directory)
            if relative.parts[0] == "files" or path.suffix == ".lines":
                continue
            urls.append(root + name + "/" + relative.as_posix())
    return "\n".join(urls) + "\n"


def wrangler_config(project):
    """A worker's Wrangler configuration: its release's assets, on its custom domain only."""
    return {"name": project["worker"], "compatibility_date": COMPATIBILITY_DATE,
            "assets": {"directory": "./" + project["worker"]},
            "routes": [{"pattern": project["domain"], "custom_domain": True}],
            "workers_dev": False, "preview_urls": False}


def package(config, output):
    """Only this invocation's empty output is populated; never copy game storage/captures."""
    if output.exists() and any(output.iterdir()):
        raise ValueError("release output must be empty; choose a new directory")
    output.mkdir(parents=True, exist_ok=True)
    console = config["console"]
    home = output / console["worker"]
    build_package(console["package"], home)
    copy_licenses(console["package"], home)
    parent = origin(console["domain"])
    catalog = []
    packages = [home]
    for game in config["games"]:
        game_output = output / game["worker"]
        game_output.mkdir()
        copy_licenses("", game_output)
        for name, source in game["builds"].items():
            build_package(source, game_output / name)
        child = origin(game["domain"])
        packages.append(game_output)
        (game_output / "_headers").write_text(headers(parent, embedded=True))
        (game_output / "_redirects").write_text(f"/ {parent}/ 302\n")
        from PIL import Image
        with Image.open(ROOT / game["cover"]) as image:
            image = image.convert("RGB")
            image.thumbnail((960, 540), Image.Resampling.LANCZOS)
            width, height = image.size
            # The console has the host decode it (platform/host.wrela `decode_image`).
            encoded = io.BytesIO()
            image.save(encoded, "WEBP", quality=COVER_QUALITY, method=6)
        art = encoded.getvalue()
        filename = "cover-" + hashlib.sha256(art).hexdigest()[:16] + ".webp"
        (home / filename).write_bytes(art)
        catalog.append("\t".join([game["id"], game["title"], child + "/" + game["session"] + "/",
                                   filename, str(width), str(height)]))
    for project in [console, *config["games"]]:
        directory = output / project["worker"]
        namespace = "console" if project is console else project["id"]
        for document in directory.rglob("index.html"):
            document.write_text(document.read_text().replace(
                "<head>", f'<head>\n<meta name="wrela-storage" content="{namespace}">'))
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
    for game in config["games"]:
        (home / f"prefetch-{game['id']}.txt").write_text(prefetch_list(game, output, release))
    (home / "_headers").write_text(headers(parent, [origin(g["domain"]) for g in config["games"]]))
    for project in [console, *config["games"]]:
        directory = output / project["worker"]
        if project is console:
            entry = (directory / "index.html").read_text()
            entry = entry.replace("<head>", f'<head>\n<base href="/releases/{release}/">')
        else:
            entry = None
        versioned = directory / "releases" / release
        versioned.mkdir(parents=True)
        for asset in list(directory.iterdir()):
            if asset.name not in {"releases", "_headers", "_redirects"}:
                asset.rename(versioned / asset.name)
        if entry is not None:
            (directory / "index.html").write_text(entry)
        (versioned / "files.json").write_text(json.dumps(file_list(versioned), sort_keys=True))
        header_file = directory / "_headers"
        header_file.write_text(header_file.read_text() + "/releases/*\n  Cache-Control: public, max-age=31536000, immutable\n/\n  Cache-Control: no-cache\n/index.html\n  Cache-Control: no-cache\n/deploys.json\n  Cache-Control: no-cache\n")
        (output / f"{project['worker']}.wrangler.json").write_text(
            json.dumps(wrangler_config(project), indent=2) + "\n")
    reports = []
    for project in [console, *config["games"]]:
        files = release_files(output / project["worker"])
        reports.append({"worker": project["worker"], "domain": project["domain"],
                        "console": project is console, "files": len(files),
                        "bytes": sum(p.stat().st_size for p in files)})
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    report = {"source": commit, "release": release, "workers": reports}
    (output / "release.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=ROOT / "games/console.toml")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    # Resolve caller-relative paths before building from this checkout.
    args.config = args.config.resolve()
    args.output = args.output.resolve()
    os.chdir(ROOT)
    try:
        report = package(read_config(args.config), args.output)
    except (ValueError, KeyError, OSError, ImportError, subprocess.CalledProcessError) as error:
        print(f"console release: {error}", file=sys.stderr)
        return 1
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
