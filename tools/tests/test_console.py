"""Release failure boundaries, without building a scene or contacting Cloudflare."""
import hashlib
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import console


def packaged(root, docs=False, extra_files=None):
    """A release of a console and one game (and the docs, if `docs`), built from stand-in builds,
    and its stand-in cover."""
    cover = root / "cover.png"
    cover.write_bytes(b"rendered cover fixture")
    config = {
        "console": {
            "package": "console", "worker": "console-host", "domain": "console.example",
            "description": "Games & <more>",
        },
        "games": [{
            "id": "game", "title": "Game", "worker": "game-host", "domain": "game.example",
            "session": "session", "builds": {"session": "session"}, "cover": "cover.png",
        }],
    }
    if docs:
        config["docs"] = {"package": "docsite", "worker": "docs-host", "domain": "docs.example",
                          "description": "The reference.", "packages": ["engine"]}

    def export(docs, directory, commit):
        # What `wrela doc --export` writes, in small.
        (directory / "index.json").write_text(json.dumps({"commit": commit, "pages": [
            {"path": "spec/language/6-memory.md", "title": "6. Memory", "summary": "Values & `moves`.", "kind": "spec"},
            {"path": "std/io.md", "title": "std::io", "summary": "", "kind": "module"},
        ]}))
        (directory / "llms.txt").write_text("# wrela\n")
        (directory / "std").mkdir()
        (directory / "std/io.md").write_text("# std::io\n")
        (directory / "std/llms-full.txt").write_text("<!-- std/io.md -->\n")

    def build(package, output):
        output.mkdir(parents=True)
        (output / "index.html").write_text("<html><head><title>wrela</title></head><body></body></html>")
        (output / "game.wasm").write_bytes(b"wasm")
        # What a game streams as it plays, and its trap locations: never prefetched.
        (output / "files").mkdir()
        (output / "files/tile.bin").write_bytes(b"tile")
        (output / "game.lines").write_bytes(b"lines")
        if package == "console":
            for name, data in (extra_files or {}).items():
                (output / name).write_bytes(data)

    image = MagicMock()
    image.__enter__.return_value = image
    image.convert.return_value = image
    image.size = (1, 1)
    image.tobytes.return_value = bytes(4)
    pillow = SimpleNamespace(Image=SimpleNamespace(
        open=lambda path: image, Resampling=SimpleNamespace(LANCZOS=0),
    ))
    output = root / "release"
    with (
        patch.object(console, "ROOT", root),
        patch.object(console, "build_package", build),
        patch.object(console, "copy_licenses"),
        patch.object(console, "export_docs", export),
        patch.object(console.subprocess, "check_output", return_value="fixture\n"),
        patch.dict(sys.modules, {"PIL": pillow}),
    ):
        report = console.package(config, output)
    return output, report, image


class StaticRelease(unittest.TestCase):
    def test_release_identity_distinguishes_file_paths_from_their_contents(self):
        with tempfile.TemporaryDirectory() as temporary:
            reports = []
            for number, files in enumerate(({"a": b"bc"}, {"ab": b"c"}, {"a": b"bc"})):
                root = Path(temporary) / str(number)
                root.mkdir()
                _, report, _ = packaged(root, extra_files=files)
                reports.append(report)
            self.assertNotEqual(reports[0]["release"], reports[1]["release"])
            self.assertEqual(reports[0]["release"], reports[2]["release"])

    def test_build_names_cannot_collide_with_release_directories(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            for package in ["console", "session"]:
                (root / package).mkdir()
                (root / package / "wrela.toml").write_text('name = "fixture"\n')
            (root / "cover.png").write_bytes(b"rendered cover fixture")
            config = root / "release.toml"
            config.write_text('''[console]
package = "console"
worker = "console-host"
description = "A console."
domain = "console.example"
[[games]]
id = "game"
title = "Game"
worker = "game-host"
domain = "game.example"
session = "releases"
builds = { releases = "session" }
cover = "cover.png"
''')
            with patch.object(console, "ROOT", root):
                with self.assertRaisesRegex(ValueError, "reserved"):
                    console.read_config(config)
                config.write_text(config.read_text().replace("releases", "session"))
                self.assertEqual(console.read_config(config)["games"][0]["session"], "session")
                config.write_text(config.read_text().replace('description = "A console."\n', ""))
                with self.assertRaisesRegex(ValueError, "description"):
                    console.read_config(config)
                config.write_text(config.read_text().replace('worker = "console-host"\n', 'worker = "console-host"\ndescription = "A console."\n', 1))
                config.write_text(config.read_text().replace("game.example", "www.example"))
                with self.assertRaisesRegex(ValueError, "own hosts"):
                    console.read_config(config)
                config.write_text(config.read_text().replace("www.example", "console.example"))
                with self.assertRaisesRegex(ValueError, "shares"):
                    console.read_config(config)
                config.write_text(config.read_text().replace('domain = "console.example"\n[[games]]', 'domain = "c.example"\n[[games]]'))
                config.write_text(config.read_text().replace("cover.png", "missing.png"))
                with self.assertRaisesRegex(ValueError, "cover"):
                    console.read_config(config)

    def test_released_builds_are_optimized(self):
        with patch.object(console.subprocess, "run") as run:
            console.build_package("games/leapling", Path("/tmp/out"))
        command = run.call_args.args[0]
        self.assertEqual(command[1:3], ["build", str(console.ROOT / "games/leapling")])
        self.assertIn("--optimize", command)

    def test_every_console_bootstrap_has_the_same_storage_namespace(self):
        with tempfile.TemporaryDirectory() as temporary:
            output, report, image = packaged(Path(temporary))
            release = report["release"]
            console_paths = [
                output / "console-host/index.html",
                output / f"console-host/releases/{release}/index.html",
            ]
            for document in console_paths:
                self.assertIn('<meta name="wrela-storage" content="console">', document.read_text())
            game_document = output / f"game-host/releases/{release}/session/index.html"
            self.assertIn('<meta name="wrela-storage" content="game">', game_document.read_text())
            # Each release lists its files for a later deploy to keep, and each Worker has its config.
            listed = json.loads((output / f"game-host/releases/{release}/files.json").read_text())
            self.assertEqual(listed["session/game.wasm"], hashlib.sha256(b"wasm").hexdigest())
            self.assertNotIn("files.json", listed)
            wrangler = json.loads((output / "game-host.wrangler.json").read_text())
            self.assertEqual(wrangler["assets"], {"directory": "./game-host"})
            self.assertEqual(wrangler["routes"], [{"pattern": "game.example", "custom_domain": True}])
            self.assertFalse(wrangler["workers_dev"])
            self.assertIn("/*.wgsl\n  Content-Type: text/plain", (output / "game-host/_headers").read_text())
            self.assertEqual([w["console"] for w in report["workers"]], [True, False])
            # The console prefetches the game's runtime and WASM from its origin, not its pages.
            prefetch = (output / f"console-host/releases/{release}/prefetch-game.txt").read_text()
            base = f"https://game.example/releases/{release}/session/"
            self.assertEqual(prefetch.split(), [base + "game.wasm"])
            # The cover ships compressed, named by its bytes, for the host to decode.
            catalog = (output / f"console-host/releases/{release}/catalog.tsv").read_text().split("\t")
            self.assertRegex(catalog[3], r"^cover-[0-9a-f]{16}\.webp$")
            self.assertEqual(image.save.call_args.args[1:], ("WEBP",))

    def test_the_console_may_fetch_its_games_files_and_shows_a_card_where_shared(self):
        with tempfile.TemporaryDirectory() as temporary:
            output, report, _ = packaged(Path(temporary))
            release = report["release"]
            policy = lambda worker: next(line for line in (output / worker / "_headers").read_text().splitlines()
                                         if "Content-Security-Policy" in line)
            # The console connects to each game's origin, to fetch its files ahead; a game only to its own.
            self.assertIn("connect-src 'self' https://game.example;", policy("console-host"))
            self.assertIn("connect-src 'self';", policy("game-host"))
            # A game's files are readable by the console, with credentials, and by no other origin.
            game_headers = (output / "game-host/_headers").read_text()
            self.assertIn("Access-Control-Allow-Origin: https://console.example\n", game_headers)
            self.assertIn("Access-Control-Allow-Credentials: true\n", game_headers)
            self.assertNotIn("Access-Control-Allow-Origin", (output / "console-host/_headers").read_text())
            # The console's page describes itself and shows its first game's cover, by an absolute address.
            page = (output / "console-host/index.html").read_text()
            self.assertIn('<meta name="description" content="Games &amp; &lt;more&gt;">', page)
            self.assertIn('<meta property="og:url" content="https://console.example/">', page)
            catalog = (output / f"console-host/releases/{release}/catalog.tsv").read_text().split("\t")
            self.assertIn(f'<meta property="og:image" content="https://console.example/releases/{release}/{catalog[3]}">', page)

    def test_the_docs_ship_their_program_beside_their_export_with_stable_addresses(self):
        with tempfile.TemporaryDirectory() as temporary:
            output, report, _ = packaged(Path(temporary), docs=True)
            release = report["release"]
            shelf = output / "docs-host"
            # The program and the export together, in the release: the program fetches the
            # export's files beside it.
            for name in ["index.html", "game.wasm", "index.json", "llms.txt", "std/io.md", "files.json"]:
                self.assertTrue((shelf / "releases" / release / name).is_file(), name)
            self.assertIn('"commit": "fixture"', (shelf / "releases" / release / "index.json").read_text())
            # An agent's addresses last from release to release.
            redirects = (shelf / "_redirects").read_text().splitlines()
            self.assertIn(f"/llms.txt /releases/{release}/llms.txt 302", redirects)
            self.assertIn(f"/index.json /releases/{release}/index.json 302", redirects)
            self.assertIn(f"/std/* /releases/{release}/std/:splat 302", redirects)
            self.assertFalse(any("manifest.json" in line or "files.json" in line for line in redirects))
            # Markdown is served as what it is, readable by anyone; the page itself is framed by no one.
            docs_headers = (shelf / "_headers").read_text()
            self.assertIn("/*.md\n  Content-Type: text/markdown; charset=utf-8\n  Access-Control-Allow-Origin: *", docs_headers)
            self.assertIn("frame-ancestors 'none'", docs_headers)
            self.assertIn("connect-src 'self';", docs_headers)
            page = (shelf / "index.html").read_text()
            self.assertIn(f'<base href="/releases/{release}/">', page)
            self.assertIn('<meta name="description" content="The reference.">', page)
            self.assertIn('<link rel="alternate" type="text/markdown" href="/llms.txt"', page)
            # The page's own text lists each page by its Markdown's stable address, says how an
            # address names a page, and the tab is named before the program runs.
            self.assertIn("<title>Wrela docs</title>", page)
            text = page[page.index('<main id="wrela-text">'):page.index("</main>")]
            self.assertIn("<h2>The language</h2>", text)
            # A person's pages first, then what an agent reads.
            self.assertLess(text.index("<h2>The language</h2>"), text.index("<h2>For agents</h2>"))
            self.assertIn('<li><a href="https://docs.example/spec/language/6-memory.html">6. Memory</a>: '
                          'Values &amp; <code>moves</code>.</li>', text)
            self.assertIn('<li><a href="https://docs.example/std/io.html">std::io</a></li>', text)
            # Each page is HTML too, beside its Markdown, with the pages' style and the script that
            # sends a browser without WebGPU to the page its address names.
            shipped = shelf / "releases" / release
            self.assertIn("<title>std::io | Wrela docs</title>", (shipped / "std/io.html").read_text())
            for name in ["pages.css", "pages.js", "Manrope-Medium.ttf"]:
                self.assertTrue((shipped / name).is_file(), name)
            self.assertIn('<script src="pages.js" defer></script>', page)
            # It comes before the program's own style, so it says more to win: a pinch still zooms.
            self.assertIn("<style>html canvas#screen { touch-action: pinch-zoom; } html, body { background: #09090B; }", page)
            self.assertIn('<meta name="theme-color" media="(prefers-color-scheme: light)" content="#FFFFFF">', page)
            self.assertIn('<a href="https://docs.example/std/llms-full.txt">std/llms-full.txt</a>', text)
            self.assertNotIn("spec/llms-full.txt", text)
            self.assertIn("<code>https://docs.example/#std/arena:arena-insert</code>", text)
            self.assertIn('<meta name="wrela-storage" content="docs">', page)
            docs = next(w for w in report["workers"] if w["worker"] == "docs-host")
            self.assertEqual((docs["domain"], docs["console"], docs["docs"]), ("docs.example", False, True))
            wrangler = json.loads((output / "docs-host.wrangler.json").read_text())
            self.assertEqual(wrangler["routes"], [{"pattern": "docs.example", "custom_domain": True}])

    def test_the_docs_export_cannot_overwrite_the_program(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / "shelf"
            directory.mkdir()
            (directory / "index.html").write_text("program")

            def run(command, **kwargs):
                corpus = Path(command[command.index("--export") + 1])
                corpus.mkdir()
                (corpus / "index.html").write_text("export")
            with patch.object(console.subprocess, "run", run):
                with self.assertRaisesRegex(ValueError, "both have index.html"):
                    console.export_docs({"packages": []}, directory, "abc")
            self.assertEqual((directory / "index.html").read_text(), "program")

    def test_private_storage_and_symlinks_cannot_be_uploaded(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "game.wasm").write_bytes(b"wasm")
            self.assertEqual(len(console.release_files(root)), 1)
            (root / "storage").mkdir()
            save = root / "storage/save"
            save.write_bytes(b"private")
            with self.assertRaisesRegex(ValueError, "private"):
                console.release_files(root)
            save.unlink()
            (root / "storage").rmdir()
            (root / "outside").symlink_to(root / "game.wasm")
            with self.assertRaisesRegex(ValueError, "symbolic"):
                console.release_files(root)

    def test_domains_and_catalog_components_reject_header_and_path_injection(self):
        for value in ["../game", "a/b", "name\nHeader: injected", ""]:
            with self.assertRaises(ValueError):
                console.component(value)
        for domain in ["example.com\nOther: value", "https://example.com", "a..example.com"]:
            with self.assertRaises(ValueError):
                console.origin(domain)
        h = console.headers("https://games.wrela.dev", ["https://one.games.wrela.dev"])
        self.assertIn('cross-origin-isolated=(self "https://one.games.wrela.dev")', h)
        self.assertIn("frame-ancestors 'none'", h)
        child = console.headers("https://games.wrela.dev", embedded=True)
        self.assertIn("frame-ancestors https://games.wrela.dev", child)
        self.assertIn("Cross-Origin-Embedder-Policy: require-corp", child)


    def test_redistributed_releases_include_software_and_font_licenses(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "LICENSE").write_text("MIT software license")
            fonts = root / "console/fonts"
            fonts.mkdir(parents=True)
            (fonts / "OFL-example.txt").write_text("Font license")
            # A font a dependency bakes ships too, through a dependency of a dependency: one a
            # member of the workspace, found by its name, and one found by its path.
            (root / "wrela.toml").write_text('[workspace]\nmembers = [\n    "console",\n    "ui/*",\n]\n')
            (root / "console/wrela.toml").write_text('[package]\nname = "console"\n[dependencies]\nbrand = { workspace = true }\n')
            (root / "ui/brand/fonts").mkdir(parents=True)
            (root / "ui/brand/wrela.toml").write_text('[package]\nname = "brand"\n[dependencies]\nui = { path = ".." }\n')
            (root / "ui/brand/fonts/OFL-Brand.txt").write_text("Brand font license")
            (root / "ui/fonts").mkdir(parents=True)
            (root / "ui/fonts/OFL-Ui.txt").write_text("UI font license")
            output = root / "release"
            output.mkdir()
            with patch.object(console, "ROOT", root):
                console.copy_licenses("console", output)
            self.assertEqual((output / "LICENSE").read_text(), "MIT software license")
            self.assertEqual((output / "OFL-example.txt").read_text(), "Font license")
            self.assertEqual((output / "OFL-Brand.txt").read_text(), "Brand font license")
            self.assertEqual((output / "OFL-Ui.txt").read_text(), "UI font license")

    def test_workspace_members_are_the_nearest_workspaces(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            (root / "wrela.toml").write_text('[workspace]\nmembers = ["outer"]\n')
            (root / "outer").mkdir()
            (root / "outer/wrela.toml").write_text('[package]\nname = "outer"\n')
            inner = root / "inner"
            (inner / "great-tree").mkdir(parents=True)
            (inner / "wrela.toml").write_text('[package]\nname = "app"\n[workspace]\nmembers = [".", "great-tree"]\n')
            (inner / "great-tree/wrela.toml").write_text("")
            self.assertEqual(console.workspace_members(inner / "great-tree"),
                             {"app": inner, "great_tree": inner / "great-tree"})
            (inner / "wrela.toml").write_text('[package]\nname = "great_tree"\n[workspace]\nmembers = [".", "*"]\n')
            with self.assertRaises(ValueError):
                console.workspace_members(inner)
