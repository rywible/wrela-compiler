"""Release failure boundaries, without building a scene or contacting Cloudflare."""
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import console


class StaticRelease(unittest.TestCase):
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
project = "console-host"
domain = "console.example"
[[games]]
id = "game"
title = "Game"
project = "game-host"
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
                config.write_text(config.read_text().replace("cover.png", "missing.png"))
                with self.assertRaisesRegex(ValueError, "cover"):
                    console.read_config(config)

    def test_every_console_bootstrap_has_the_same_storage_namespace(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cover = root / "cover.png"
            cover.write_bytes(b"rendered cover fixture")
            config = {
                "console": {
                    "package": "console", "project": "console-host", "domain": "console.example",
                },
                "games": [{
                    "id": "game", "title": "Game", "project": "game-host", "domain": "game.example",
                    "session": "session", "builds": {"session": "session"}, "cover": "cover.png",
                }],
            }

            def build(package, output):
                output.mkdir(parents=True)
                (output / "index.html").write_text("<html><head></head><body></body></html>")
                (output / "game.wasm").write_bytes(b"wasm")

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
                patch.object(console.subprocess, "check_output", return_value="fixture\n"),
                patch.dict(sys.modules, {"PIL": pillow}),
            ):
                report = console.package(config, output)
            release = report["release"]
            console_paths = [
                output / "console-host/index.html",
                output / f"console-host/releases/{release}/index.html",
            ]
            for document in console_paths:
                self.assertIn('<meta name="wrela-storage" content="console">', document.read_text())
            game_document = output / f"game-host/releases/{release}/session/index.html"
            self.assertIn('<meta name="wrela-storage" content="game">', game_document.read_text())

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


    def test_retained_release_assets_cannot_change_under_an_immutable_url(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "previous/releases" / ("a" * 20)
            source.mkdir(parents=True)
            (source / "game.wasm").write_bytes(b"old version")
            target = root / "next"
            console.retain_releases(root / "previous", target)
            self.assertEqual((target / "releases" / ("a" * 20) / "game.wasm").read_bytes(), b"old version")
            (source / "game.wasm").write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "immutable"):
                console.retain_releases(root / "previous", target)


    def test_redistributed_releases_include_software_and_font_licenses(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "LICENSE").write_text("MIT software license")
            fonts = root / "console/fonts"
            fonts.mkdir(parents=True)
            (fonts / "OFL-example.txt").write_text("Font license")
            output = root / "release"
            output.mkdir()
            with patch.object(console, "ROOT", root):
                console.copy_licenses("console", output)
            self.assertEqual((output / "LICENSE").read_text(), "MIT software license")
            self.assertEqual((output / "OFL-example.txt").read_text(), "Font license")
