#!/usr/bin/env python3
"""Build static Wrela console/game releases for Cloudflare Workers static assets; requires Pillow
and Binaryen's wasm-opt. tools/deploy.py deploys one."""
import argparse
import hashlib
import html
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib

import pages

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
    description = data["console"].get("description")
    if not isinstance(description, str) or not description or any(c in description for c in "\r\n"):
        raise ValueError("the console needs a one-line description")
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
    if "docs" in data:
        docs = data["docs"]
        if component(docs["worker"]) in workers or docs["domain"] in domains:
            raise ValueError("the docs share a worker or a domain")
        origin(docs["domain"])
        text = docs.get("description")
        if not isinstance(text, str) or not text or any(c in text for c in "\r\n"):
            raise ValueError("the docs need a one-line description")
        for package in [docs["package"], *docs["packages"]]:
            source = (ROOT / package).resolve()
            if not source.is_relative_to(ROOT) or not (source / "wrela.toml").is_file():
                raise ValueError(f"a docs package is not inside the repository: {package}")
    return data


def headers(parent, children=(), embedded=False):
    # Assets are public. The origin-owned OPFS is never uploaded or served.
    ancestors = parent if embedded else "'none'"
    frame_src = " ".join(children) if children else "'none'"
    connect_src = " ".join(["'self'", *children])
    policy = "cross-origin-isolated=(self" + "".join(f' "{c}"' for c in children) + ")"
    # The console fetches a game's files ahead (`prefetch_list`) as readable CORS requests with
    # credentials, as the game's own page fetches them, so the browser's cache keeps them for it.
    shared = f"\n  Access-Control-Allow-Origin: {parent}\n  Access-Control-Allow-Credentials: true" if embedded else ""
    return f"""/*
  Cross-Origin-Opener-Policy: same-origin
  Cross-Origin-Embedder-Policy: require-corp
  Cross-Origin-Resource-Policy: cross-origin{shared}
  Permissions-Policy: {policy}
  X-Content-Type-Options: nosniff
  Referrer-Policy: origin
  Content-Security-Policy: default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; worker-src 'self' blob:; connect-src {connect_src}; img-src 'self' blob:; frame-src {frame_src}; frame-ancestors {ancestors}; object-src 'none'; base-uri 'self'
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


def package_dirs(package):
    """The package at `package` (relative to the repository) and every package it depends on:
    a font any of them bakes ships in the build."""
    seen = []
    stack = [(ROOT / package).resolve()]
    while stack:
        directory = stack.pop()
        if directory in seen:
            continue
        seen.append(directory)
        manifest = directory / "wrela.toml"
        if manifest.is_file():
            dependencies = tomllib.loads(manifest.read_text()).get("dependencies", {})
            stack.extend((directory / d["path"]).resolve() for d in dependencies.values())
    return seen


def copy_licenses(package, output):
    shutil.copy2(ROOT / "LICENSE", output / "LICENSE")
    for directory in package_dirs(package):
        for license_file in (directory / "fonts").glob("OFL-*.txt"):
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
    """What the console fetches of a game ahead of time, once it is idle: each build's runtime,
    WASM and shaders, the session first. Not the files a game streams as it plays, nor its trap
    locations, which only a crash reads, nor its pages: a frame's page is a navigation, which
    the browser caches apart from fetches."""
    root = origin(game["domain"]) + "/releases/" + release + "/"
    builds = sorted(game["builds"], key=lambda name: name != game["session"])
    urls = []
    for name in builds:
        directory = output / game["worker"] / name
        for path in sorted(release_files(directory)):
            relative = path.relative_to(directory)
            if relative.parts[0] == "files" or path.suffix in {".lines", ".html"}:
                continue
            urls.append(root + name + "/" + relative.as_posix())
    return "\n".join(urls) + "\n"


def card(console, release, catalog):
    """The console page's description, and the card a link to it shows where it is shared: its
    first game's cover."""
    home = origin(console["domain"])
    text = html.escape(console["description"])
    cover = f"{home}/releases/{release}/{catalog[0].split(chr(9))[3]}"
    return (f'\n<meta name="description" content="{text}">'
            f'\n<meta property="og:type" content="website">'
            f'\n<meta property="og:site_name" content="wrela">'
            f'\n<meta property="og:title" content="wrela">'
            f'\n<meta property="og:description" content="{text}">'
            f'\n<meta property="og:url" content="{home}/">'
            f'\n<meta property="og:image" content="{cover}">'
            f'\n<meta name="twitter:card" content="summary_large_image">')


def export_docs(docs, directory, commit):
    """The reference docs of std and the docs' packages (`wrela doc --export`), put in `directory`
    beside the docs' program. A file of the export can't take the place of one of the program's."""
    with tempfile.TemporaryDirectory() as temporary:
        corpus = Path(temporary) / "corpus"
        subprocess.run([str(ROOT / "tools/wrela"), "doc", "--export", str(corpus), "--commit", commit,
                        *[str(ROOT / p) for p in docs["packages"]]], cwd=ROOT, check=True)
        for item in sorted(corpus.iterdir()):
            if (directory / item.name).exists():
                raise ValueError(f"the docs' export and program both have {item.name}")
            if item.is_dir():
                shutil.copytree(item, directory / item.name)
            else:
                shutil.copy2(item, directory / item.name)


def docs_headers(home):
    """The docs Worker's headers: the program's, and its Markdown and text readable by anyone, as
    what they are."""
    raw = "\n  Access-Control-Allow-Origin: *"
    return headers(home) + (f"/*.md\n  Content-Type: text/markdown; charset=utf-8{raw}\n"
                            f"/*.txt\n  Content-Type: text/plain; charset=utf-8{raw}\n"
                            f"/*.json{raw}\n")


def docs_redirects(directory, release):
    """Stable addresses for the docs' raw files (`/llms.txt`, `/std/io.md`): each leads to this
    release's copy, so an agent's link outlives the release."""
    lines = []
    for item in sorted(directory.iterdir()):
        if item.is_dir() and any(item.rglob("*.md")):
            lines.append(f"/{item.name}/* /releases/{release}/{item.name}/:splat 302")
        elif item.suffix in {".txt", ".json"} and item.name not in {"manifest.json", "files.json"}:
            lines.append(f"/{item.name} /releases/{release}/{item.name} 302")
    return "\n".join(lines) + "\n"


# The docs' page in each scheme (docsite/theme.wrela `DARK.page`, `LIGHT.page`).
PAGE_DARK = "#09090B"
PAGE_LIGHT = "#FFFFFF"


def docs_page(docs, directory):
    """The docs page's description, its Markdown for agents, and its own text (`main#wrela-text`):
    the docs' pages, each a link to its Markdown, for whoever doesn't run the program, such as an
    agent that reads the page as HTML or a browser without WebGPU. An address's fragment never
    reaches the server, so the text says how an address names a page. The browser host shows it
    when the program can't run and removes it once the program draws."""
    text = html.escape(docs["description"])
    home = origin(docs["domain"])
    head = (f'\n<meta name="description" content="{text}">'
            f'\n<meta property="og:type" content="website">'
            f'\n<meta property="og:site_name" content="wrela">'
            f'\n<meta property="og:title" content="Wrela docs">'
            f'\n<meta property="og:description" content="{text}">'
            f'\n<meta property="og:url" content="{home}/">'
            f'\n<link rel="alternate" type="text/markdown" href="/llms.txt" title="The docs as Markdown">'
            f'\n<script src="pages.js" defer></script>'
            # The docs' page colours (docsite/theme.wrela), behind the canvas before the program
            # draws and in a phone's browser bar, so the page doesn't open black.
            f'\n<meta name="theme-color" media="(prefers-color-scheme: dark)" content="{PAGE_DARK}">'
            f'\n<meta name="theme-color" media="(prefers-color-scheme: light)" content="{PAGE_LIGHT}">'
            # A reader may still pinch the docs larger, as on any page; a finger's swipe is the
            # program's.
            f'\n<style>html canvas#screen {{ touch-action: pinch-zoom; }}'
            f' html, body {{ background: {PAGE_DARK}; }}'
            f' @media (prefers-color-scheme: light) {{ html, body {{ background: {PAGE_LIGHT}; }} }}</style>')
    index = json.loads((directory / "index.json").read_text())
    groups = {}
    for page in index.get("pages", []):
        group = "spec" if page["kind"] == "spec" else page["path"].split("/")[0]
        groups.setdefault(group, []).append(page)
    # The language, then std, then the packages by name, as `llms.txt` has them.
    groups = dict(sorted(groups.items(), key=lambda g: (g[0] != "spec", g[0] != "std", g[0])))
    files = [("llms.txt", "every page with its summary, and how to read the docs"),
             ("index.json", "every item with its signature, its page and its source line"),
             *[(f"{g}/llms-full.txt", f"{'the language' if g == 'spec' else g}'s pages in one file")
               for g in groups if (directory / g / "llms-full.txt").is_file()],
             ("llms-full.txt", "every page in one file")]
    # A person first: the pages, each as plain HTML. Then an agent: the Markdown, and how an
    # address names a page, as its fragment never reaches the server.
    body = ['<main id="wrela-text">', "<h1>Wrela docs</h1>", f"<p>{text}</p>",
            "<p>These pages are a Wrela program that draws with WebGPU. Here they are as plain "
            "pages, for a browser that can't run it.</p>"]
    for group, members in groups.items():
        body.append(f"<h2>{'The language' if group == 'spec' else html.escape(group)}</h2>")
        body.append("<ul>")
        for page in members:
            link = f'<a href="{home}/{html.escape(page["path"][:-3])}.html">{html.escape(page["title"])}</a>'
            summary = page.get("summary", "")
            body.append(f"<li>{link}: {code_of(summary)}</li>" if summary else f"<li>{link}</li>")
        body.append("</ul>")
    body += ["<h2>For agents</h2>", "<p>Each page is Markdown too:</p>", "<ul>"]
    body += [f'<li><a href="{home}/{name}">{name}</a>: {what}.</li>' for name, what in files
             if (directory / name).is_file()]
    body += ["</ul>",
             f"<p>An address of these docs names its page after <code>#</code>, and a heading on it "
             f"after <code>:</code>. <code>{home}/#std/arena:arena-insert</code> is the heading "
             f"<code>arena-insert</code> of <a href=\"{home}/std/arena.md\">{home}/std/arena.md</a>.</p>"]
    body.append("</main>")
    return head, "\n" + "\n".join(body)


def code_of(markdown):
    """A line of Markdown as HTML: escaped, with its `code` as code."""
    parts = html.escape(markdown, quote=False).split("`")
    return "".join(f"<code>{part}</code>" if i % 2 else part for i, part in enumerate(parts))


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
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    docs = config.get("docs")
    if docs is not None:
        shelf = output / docs["worker"]
        build_package(docs["package"], shelf)
        copy_licenses(docs["package"], shelf)
        export_docs(docs, shelf, commit)
        pages.write_pages(shelf)
        packages.append(shelf)
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
    projects = [console, *config["games"], *([docs] if docs is not None else [])]
    for project in projects:
        directory = output / project["worker"]
        namespace = "console" if project is console else "docs" if project is docs else project["id"]
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
    if docs is not None:
        shelf = output / docs["worker"]
        (shelf / "_headers").write_text(docs_headers(origin(docs["domain"])))
        (shelf / "_redirects").write_text(docs_redirects(shelf, release))
    for project in projects:
        directory = output / project["worker"]
        if project is console:
            entry = (directory / "index.html").read_text()
            entry = entry.replace("<head>", f'<head>\n<base href="/releases/{release}/">' + card(console, release, catalog))
        elif project is docs:
            head, body = docs_page(docs, directory)
            entry = (directory / "index.html").read_text()
            entry = entry.replace("<head>", f'<head>\n<base href="/releases/{release}/">' + head)
            entry = entry.replace("<title>wrela</title>", "<title>Wrela docs</title>")
            entry = entry.replace("<body>", "<body>" + body)
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
    for project in projects:
        files = release_files(output / project["worker"])
        reports.append({"worker": project["worker"], "domain": project["domain"],
                        "console": project is console, "docs": project is docs, "files": len(files),
                        "bytes": sum(p.stat().st_size for p in files)})
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
