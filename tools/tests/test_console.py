"""Release failure boundaries, without building a scene or contacting Cloudflare."""
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import console


class StaticRelease(unittest.TestCase):
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
