"""Keep the portable dependency snapshot byte-identical to reviewed upstream."""
import hashlib
import json
from pathlib import Path
import unittest


class UpstreamSnapshotTests(unittest.TestCase):
    def test_pinned_crates_licenses_and_consumer_workspace_match_recorded_bytes(self):
        root = Path(__file__).resolve().parent.parent / "vendor/stateset-icommerce"
        manifest = json.loads((root / "UPSTREAM.json").read_text())
        self.assertEqual(manifest["revision"], "b8a5c6db885998defa9fd74e2b12f0d1add21e1c")
        self.assertEqual(len(manifest["files"]), 169)
        self.assertEqual({str(p.relative_to(root)) for p in root.rglob("*") if p.is_file()},
                         {entry["path"] for entry in manifest["files"]} | {"Cargo.toml", "UPSTREAM.json"})
        for entry in manifest["files"]:
            with self.subTest(path=entry["path"]):
                raw = (root / entry["path"]).read_bytes()
                self.assertEqual(hashlib.sha256(raw).hexdigest(), entry["sha256"])
                self.assertEqual(hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw).hexdigest(), entry["git_blob_sha1"])
        self.assertEqual(hashlib.sha256((root / "Cargo.toml").read_bytes()).hexdigest(), manifest["consumer_workspace_sha256"])


if __name__ == "__main__":
    unittest.main()
