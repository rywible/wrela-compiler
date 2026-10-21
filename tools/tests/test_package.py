"""Tests for tools/package.py: a release's tarball and its checksums, and pinned downloads."""

import hashlib
import io
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import package


class Pack(unittest.TestCase):
    def test_the_tarball_holds_the_files_with_their_modes_and_its_checksum(self):
        files = [("bin/wrela", 0o755, b"#!/bin/sh\n"), ("VERSION", 0o644, b"1.2.3\n"),
                 ("lib/libbinaryen.dylib", 0o755, b"lib")]
        with tempfile.TemporaryDirectory() as d:
            out = Path(d)
            tarball = package.pack(out, "1.2.3", "aarch64-apple-darwin", files)
            self.assertEqual(tarball.name, "wrela-1.2.3-aarch64-apple-darwin.tar.gz")
            data = tarball.read_bytes()
            self.assertEqual((out / "SHA256SUMS").read_text(),
                             f"{hashlib.sha256(data).hexdigest()}  {tarball.name}\n")
            with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
                found = {m.name: (m.mode, m.isdir()) for m in tar.getmembers()}
                self.assertEqual(tar.extractfile("bin/wrela").read(), b"#!/bin/sh\n")
            self.assertEqual(found, {"bin": (0o755, True), "lib": (0o755, True),
                                     "bin/wrela": (0o755, False), "VERSION": (0o644, False),
                                     "lib/libbinaryen.dylib": (0o755, False)})
            # The same files give the same bytes.
            again = package.pack(out / "again", "1.2.3", "aarch64-apple-darwin",
                                 list(reversed(files)))
            self.assertEqual(again.read_bytes(), data)


class Fetch(unittest.TestCase):
    def test_a_download_is_used_only_with_its_pinned_checksum(self):
        with tempfile.TemporaryDirectory() as d:
            source = Path(d) / "source"
            source.write_bytes(b"binaryen")
            digest = hashlib.sha256(b"binaryen").hexdigest()
            keep = Path(d) / "kept/binaryen.tgz"
            with self.assertRaisesRegex(ValueError, "not the pinned"):
                package.fetch(source.as_uri(), "0" * 64, keep)
            self.assertFalse(keep.exists())
            self.assertEqual(package.fetch(source.as_uri(), digest, keep), b"binaryen")
            source.unlink()
            # Kept: read from there.
            self.assertEqual(package.fetch(source.as_uri(), digest, keep), b"binaryen")


if __name__ == "__main__":
    unittest.main()
