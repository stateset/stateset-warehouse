"""Notices retain upstream texts and reject dependency/license drift."""
import json
from pathlib import Path
import tempfile
import unittest
from dashboard_notices import render


class BrowserNoticesTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.package = self.root / "dashboard/node_modules/example"
        self.package.mkdir(parents=True)
        self.manifest = {"name": "example", "version": "1.0.0", "license": "MIT"}
        self.lock = {"packages": {"": {"version": "0.2.0"}, "node_modules/example": {"version": "1.0.0", "license": "MIT", "integrity": "sha512-synthetic"}, "node_modules/dev-only": {"version": "2.0.0", "dev": True}}}
        self.notice = "Copyright Original Author\n\nOriginal permission text.\n"
        (self.package / "LICENSE").write_text(self.notice)
        self.save()

    def save(self):
        (self.package / "package.json").write_text(json.dumps(self.manifest))
        (self.root / "dashboard/package-lock.json").write_text(json.dumps(self.lock))

    def test_retains_original_license_and_notice_text_and_excludes_build_tools(self):
        (self.package / "NOTICE.txt").write_text("Original attribution.\n")
        text, count = render(self.root)
        self.assertEqual(count, 1)
        self.assertIn(self.notice.rstrip(), text)
        self.assertIn("Original attribution.", text)
        self.assertNotIn("dev-only", text)
        self.assertEqual(render(self.root), (text, count))

    def test_changed_installed_version_or_license_is_held(self):
        for change in [{"version": "1.0.1"}, {"license": "OTHER"}]:
            self.manifest = {"name": "example", "version": "1.0.0", "license": "MIT", **change}
            self.save()
            with self.assertRaises(ValueError):
                render(self.root)

    def test_missing_or_empty_original_notice_is_held(self):
        (self.package / "LICENSE").unlink()
        with self.assertRaises(ValueError):
            render(self.root)
        (self.package / "LICENSE").write_text("\n")
        with self.assertRaises(ValueError):
            render(self.root)

    def test_optional_runtime_packages_are_never_silently_omitted(self):
        self.lock["packages"]["node_modules/not-installed"] = {"version": "1.0.0", "license": "MIT", "integrity": "sha512-synthetic", "optional": True}
        self.save()
        with self.assertRaises(FileNotFoundError):
            render(self.root)

    def test_notice_cannot_copy_a_file_from_outside_the_package(self):
        original = self.package / "LICENSE"
        original.unlink()
        outside = self.root / "private.txt"
        outside.write_text("Private material must never become a public notice")
        original.symlink_to(outside)
        with self.assertRaises(ValueError):
            render(self.root)


if __name__ == "__main__":
    unittest.main()
