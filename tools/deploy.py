#!/usr/bin/env python3
"""Deploy a release that tools/console.py built to Cloudflare Workers, keeping what live clients use.

    tools/deploy.py --release <dir> [--require-main-head] [--dry-run]

A client keeps the console it loaded, and the console pins each game's release: a deploy must keep
serving every release a client may still hold. The console serves `deploys.json`, the history of
deploys. A release is kept while it's live and for KEEP after it stops being live. The live site is
the archive: each kept release's files are fetched from the Worker that serves them, each checked
against the SHA-256 its `files.json` lists, and uploaded again with the new release. A file that
doesn't arrive, or doesn't match, stops the deploy, and so does a Worker over the file limits:
nothing is dropped to make room. With `--require-main-head`, a deploy whose commit is no longer
the head of `main` stops before it changes anything. The games deploy first and then the console,
so a console never offers a game release its Worker doesn't serve yet; the docs, which no other
Worker depends on, go last, so a failure there leaves the console and games up to date. Each Worker
must serve the new release before the deploy passes.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timedelta, timezone
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parent))
import console

ROOT = Path(__file__).resolve().parents[1]
# How long a release stays after it stops being live: a tab left open that long can still play.
KEEP = timedelta(days=14)
WRANGLER = ["npx", "--yes", "wrangler@4.151.0"]
RELEASE = re.compile(r"[a-f0-9]{20}")


def fetch(url):
    """The body at `url`. Errors on anything but 200."""
    request = urllib.request.Request(url, headers={"User-Agent": "wrela-deploy"})
    with urllib.request.urlopen(request, timeout=60) as response:
        return response.read()


def history(domain, get=fetch):
    """The deploys the console at `domain` lists, oldest first."""
    deploys = json.loads(get(f"https://{domain}/deploys.json"))["deploys"]
    for d in deploys:
        if not RELEASE.fullmatch(d["release"]):
            raise ValueError(f"invalid release in the history: {d['release']!r}")
    return deploys


def kept(deploys, now, release):
    """The releases a deploy of `release` at `now` keeps: those live, or live within KEEP."""
    keep = []
    for d in deploys:
        until = d["live_until"]
        if d["release"] != release and (until is None or now - parse(until) <= KEEP):
            keep.append(d)
    return keep


def next_history(deploys, release, source, workers, now):
    """The history after `release` goes live at `now`: the live one ends, the expired ones go."""
    stamp = now.isoformat(timespec="seconds")
    after = []
    for d in deploys:
        if d["release"] == release:
            continue
        d = dict(d)
        if d["live_until"] is None:
            d["live_until"] = stamp
        if now - parse(d["live_until"]) <= KEEP:
            after.append(d)
    after.append({"release": release, "source": source, "workers": workers, "live_from": stamp,
                  "live_until": None})
    return after


def parse(stamp):
    return datetime.fromisoformat(stamp)


def restore(release, domain, target, get=fetch):
    """Fetches the kept `release` from the Worker at `domain` into `target`, its `releases/<id>`."""
    base = f"https://{domain}/releases/{release}/"
    listing = get(base + "files.json")
    files = json.loads(listing)
    destination = target / "releases" / release

    def one(item):
        path, digest = item
        parts = PurePosixPath(path).parts
        if not parts or any(p in {"", ".", ".."} for p in parts) or PurePosixPath(path).is_absolute():
            raise ValueError(f"{release}: invalid path {path!r}")
        data = get(base + urllib.parse.quote(path))
        if hashlib.sha256(data).hexdigest() != digest:
            raise ValueError(f"{release}/{path} from {domain} doesn't match its SHA-256")
        return destination.joinpath(*parts), data

    with ThreadPoolExecutor(16) as pool:
        fetched = list(pool.map(one, sorted(files.items())))
    fetched.append((destination / "files.json", listing))
    for file, data in fetched:
        if file.exists():
            if file.read_bytes() != data:
                raise ValueError(f"an immutable release cannot change: {file}")
            continue
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_bytes(data)


def verify(output, get=fetch, wait=time.sleep, tries=12):
    """Waits until each Worker serves the new release, byte for byte, and the console lists it live."""
    report = json.loads((output / "release.json").read_text())
    release = report["release"]
    for w in report["workers"]:
        want = (output / w["worker"] / "releases" / release / "files.json").read_bytes()
        for attempt in range(tries):
            try:
                if get(f"https://{w['domain']}/releases/{release}/files.json") == want:
                    break
            except urllib.error.URLError:
                pass
            if attempt == tries - 1:
                raise ValueError(f"{w['domain']} doesn't serve release {release}")
            wait(5)
    home = next(w for w in report["workers"] if w["console"])
    live = [d["release"] for d in history(home["domain"], get) if d["live_until"] is None]
    if live != [release]:
        raise ValueError(f"{home['domain']} lists {live} live, not {release}")


def main_head():
    """The commit `main` points at on GitHub."""
    out = subprocess.check_output(["git", "ls-remote", "origin", "refs/heads/main"], cwd=ROOT, text=True)
    return out.split()[0]


def deploy(output, require_main_head=False, dry_run=False, get=fetch, now=None,
           run=subprocess.run):
    now = now or datetime.now(timezone.utc)
    report = json.loads((output / "release.json").read_text())
    release, workers = report["release"], report["workers"]
    home = next(w for w in workers if w["console"])
    deploys = history(home["domain"], get)
    for d in kept(deploys, now, release):
        for w in workers:
            # A Worker the kept release didn't have (a game added since) has nothing to keep.
            if w["worker"] in d["workers"]:
                restore(d["release"], w["domain"], output / w["worker"], get)
    for w in workers:
        files = console.release_files(output / w["worker"])
        print(f"{w['worker']} ({w['domain']}): {len(files)} files", flush=True)
    names = [w["worker"] for w in workers]
    after = next_history(deploys, release, report["source"], names, now)
    (output / home["worker"] / "deploys.json").write_text(json.dumps({"deploys": after}, indent=2) + "\n")
    if require_main_head and main_head() != report["source"]:
        raise ValueError(f"{report['source']} is no longer the head of main; a newer push deploys")
    if dry_run:
        return after
    # The games first, then the console that pins their releases, then the docs.
    for w in sorted(workers, key=lambda w: 2 if w.get("docs") else 1 if w["console"] else 0):
        run([*WRANGLER, "deploy", "--config", str(output / f"{w['worker']}.wrangler.json")],
            cwd=output, check=True)
    verify(output, get)
    return after


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--release", type=Path, required=True, help="tools/console.py's output")
    parser.add_argument("--require-main-head", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    try:
        deploy(args.release.resolve(), args.require_main_head, args.dry_run)
    except (ValueError, KeyError, OSError, urllib.error.URLError, subprocess.CalledProcessError) as error:
        print(f"deploy: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
