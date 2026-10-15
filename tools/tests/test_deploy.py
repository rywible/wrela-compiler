"""Deploys keep what live clients use, and stop rather than drop it; without contacting Cloudflare."""
from datetime import datetime, timedelta, timezone
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import urllib.error

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import console
import deploy

NOW = datetime(2026, 10, 15, 12, tzinfo=timezone.utc)
A, B, C, D = ("a" * 20, "b" * 20, "c" * 20, "d" * 20)
WORKERS = ["console-host", "game-host"]


def stamp(days_ago):
    return (NOW - timedelta(days=days_ago)).isoformat(timespec="seconds")


def entry(release, until):
    return {"release": release, "source": "x", "workers": WORKERS, "live_from": stamp(30),
            "live_until": until}


# A ended 20 days ago, B 3 days ago, and C is live.
HISTORY = [entry(A, stamp(20)), entry(B, stamp(3)), entry(C, None)]


class Site:
    """The live Workers: each release's files by domain, and the console's history."""

    def __init__(self):
        self.files = {}
        self.deploys = HISTORY

    def serve(self, domain, release, files):
        listing = {path: hashlib.sha256(data).hexdigest() for path, data in files.items()}
        self.files[f"https://{domain}/releases/{release}/files.json"] = json.dumps(listing).encode()
        for path, data in files.items():
            self.files[f"https://{domain}/releases/{release}/{path}"] = data

    def get(self, url):
        if url.endswith("/deploys.json"):
            return json.dumps({"deploys": self.deploys}).encode()
        if url not in self.files:
            raise urllib.error.HTTPError(url, 404, "Not Found", {}, None)
        return self.files[url]


def built(root):
    """A release D as tools/console.py leaves it."""
    output = root / "release"
    for worker in WORKERS:
        (output / worker / "releases" / D).mkdir(parents=True)
        (output / worker / "releases" / D / "index.html").write_text("new")
    workers = [{"worker": "console-host", "domain": "console.example", "console": True},
               {"worker": "game-host", "domain": "game.example", "console": False}]
    (output / "release.json").write_text(json.dumps({"source": "head", "release": D, "workers": workers}))
    return output


def live_site():
    site = Site()
    for release in [B, C]:
        site.serve("console.example", release, {"index.html": release.encode()})
        site.serve("game.example", release, {"session/game.wasm": release.encode(),
                                             "session/files/floor/t1_-1.bin": b"tile"})
    return site


class Deploy(unittest.TestCase):
    def test_a_release_stays_for_its_window_after_it_stops_being_live(self):
        self.assertEqual([d["release"] for d in deploy.kept(HISTORY, NOW, D)], [B, C])
        # Redeploying the live release keeps the others it would have kept.
        self.assertEqual([d["release"] for d in deploy.kept(HISTORY, NOW, C)], [B])
        after = deploy.next_history(HISTORY, D, "head", WORKERS, NOW)
        self.assertEqual([(d["release"], d["live_until"]) for d in after],
                         [(B, stamp(3)), (C, stamp(0)), (D, None)])
        self.assertEqual(after[-1]["live_from"], stamp(0))

    def test_kept_releases_come_back_from_the_live_site_checked(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = built(Path(temporary))
            after = deploy.deploy(output, dry_run=True, get=live_site().get, now=NOW)
            for release in [B, C]:
                game = output / "game-host/releases" / release
                self.assertEqual((game / "session/game.wasm").read_bytes(), release.encode())
                self.assertEqual((game / "session/files/floor/t1_-1.bin").read_bytes(), b"tile")
                self.assertTrue((game / "files.json").is_file())
                self.assertTrue((output / "console-host/releases" / release / "index.html").is_file())
            self.assertFalse((output / "game-host/releases" / A).exists())
            served = json.loads((output / "console-host/deploys.json").read_text())["deploys"]
            self.assertEqual(served, after)
            self.assertEqual(served[-1]["release"], D)

    def test_a_file_that_does_not_match_or_arrive_stops_the_deploy(self):
        for spoil in ["changed", "missing", "escape"]:
            with tempfile.TemporaryDirectory() as temporary:
                output = built(Path(temporary))
                site = live_site()
                url = f"https://game.example/releases/{C}/session/game.wasm"
                if spoil == "changed":
                    site.files[url] = b"tampered"
                elif spoil == "missing":
                    del site.files[url]
                else:
                    site.serve("game.example", C, {"../../escape": b"x"})
                with self.assertRaises((ValueError, urllib.error.HTTPError)):
                    deploy.deploy(output, dry_run=True, get=site.get, now=NOW)
                self.assertFalse((output / "console-host/deploys.json").exists(), spoil)

    def test_a_kept_release_cannot_change_under_its_immutable_urls(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = built(Path(temporary))
            game = output / "game-host/releases" / C / "session"
            game.mkdir(parents=True)
            (game / "game.wasm").write_bytes(b"different")
            with self.assertRaisesRegex(ValueError, "immutable"):
                deploy.deploy(output, dry_run=True, get=live_site().get, now=NOW)

    def test_a_worker_over_its_file_limit_fails_rather_than_drops_a_release(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = built(Path(temporary))
            with patch.object(console, "MAX_FILES", 3), self.assertRaisesRegex(ValueError, "file-count"):
                deploy.deploy(output, dry_run=True, get=live_site().get, now=NOW)

    def test_a_deploy_no_longer_at_main_s_head_changes_nothing(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = built(Path(temporary))
            ran = []
            with patch.object(deploy, "main_head", return_value="newer"), \
                    self.assertRaisesRegex(ValueError, "no longer the head"):
                deploy.deploy(output, require_main_head=True, get=live_site().get, now=NOW,
                              run=lambda *a, **k: ran.append(a))
            self.assertEqual(ran, [])

    def test_the_games_deploy_before_the_console_and_each_serves_the_release(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = built(Path(temporary))
            for worker in WORKERS:
                listing = output / worker / "releases" / D / "files.json"
                listing.write_text(json.dumps(console.file_list(listing.parent)))
            site = live_site()
            ran = []

            def wrangler(command, **_):
                # What `wrangler deploy` makes live: the Worker's release, and the console's history.
                worker = Path(command[-1]).name.removesuffix(".wrangler.json")
                domain = {"console-host": "console.example", "game-host": "game.example"}[worker]
                listing = output / worker / "releases" / D / "files.json"
                site.files[f"https://{domain}/releases/{D}/files.json"] = listing.read_bytes()
                if worker == "console-host":
                    site.deploys = json.loads((output / worker / "deploys.json").read_text())["deploys"]
                ran.append(worker)

            with patch.object(deploy, "main_head", return_value="head"):
                deploy.deploy(output, require_main_head=True, get=site.get, now=NOW, run=wrangler)
            self.assertEqual(ran, ["game-host", "console-host"])

    def test_a_worker_that_does_not_serve_the_release_fails_the_deploy(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = built(Path(temporary))
            for worker in WORKERS:
                listing = output / worker / "releases" / D / "files.json"
                listing.write_text("{}")
            with self.assertRaisesRegex(ValueError, "doesn't serve"):
                deploy.verify(output, get=live_site().get, wait=lambda s: None)


if __name__ == "__main__":
    unittest.main()
