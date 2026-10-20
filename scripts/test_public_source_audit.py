"""Fail closed when source, finding identity or public-path safety changes."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from public_source_audit import audit, classify, path_issue, sha256


class PublicSourceAuditTests(unittest.TestCase):
    def setUp(self):
        self.data = b"Synthetic fixture identity\n"
        self.finding = {"RuleID": "generic-api-key", "StartLine": 1, "EndLine": 1, "Commit": "a" * 40}
        self.review = {"rule": "generic-api-key", "path": "tests/fixture.json", "start_line": 1,
                       "end_line": 1, "source_sha256": sha256(self.data), "reason": "Synthetic identity, no credential"}

    def test_review_requires_exact_original_bytes_path_rule_and_lines(self):
        self.assertEqual(classify(self.finding, "tests/fixture.json", self.data, [self.review])["classification"], "reviewed_false_positive")
        cases = [(self.finding, "other.json", self.data), (self.finding, "tests/fixture.json", self.data + b"changed"),
                 ({**self.finding, "RuleID": "github-pat"}, "tests/fixture.json", self.data),
                 ({**self.finding, "StartLine": 2}, "tests/fixture.json", self.data),
                 ({**self.finding, "EndLine": 2}, "tests/fixture.json", self.data)]
        for finding, path, data in cases:
            with self.subTest(path=path, finding=finding):
                self.assertEqual(classify(finding, path, data, [self.review])["classification"], "requires_review")

    def test_ambiguous_or_unexplained_reviews_never_clear_a_finding(self):
        for reviews in [[], [self.review, self.review], [{**self.review, "reason": " "}]]:
            self.assertEqual(classify(self.finding, "tests/fixture.json", self.data, reviews)["classification"], "requires_review")

    def test_private_data_paths_and_indirections_are_held(self):
        for name in [".env", "nested/.env.production", ".local/recovery/request.json", ".aws/config", ".codex/auth.json",
                     "db/warehouse.dump", "fixtures/real.sqlite3", "keys/service.key", "target/debug/api", "dashboard/node_modules/react/package.json", "../outside", "/outside"]:
            with self.subTest(name=name):
                self.assertIsNotNone(path_issue(name))
        for mode in ["120000", "160000", "040000"]:
            self.assertIsNotNone(path_issue("ordinary-name", mode))

    def test_documented_examples_and_literal_fixtures_remain_auditable(self):
        for name in [".env.example", "docs/.env.example", "tests/fixtures/synthetic-journal.json", "docs/screenshots/desktop.png"]:
            self.assertIsNone(path_issue(name))
        self.assertIsNone(path_issue("scripts/check.sh", "100755"))


class PublicSourceHistoryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.repository = self.directory / "repository"
        self.repository.mkdir()
        self.env = dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        self.git("init")
        self.git("symbolic-ref", "HEAD", "refs/heads/main")
        self.git("config", "user.name", "Synthetic audit test")
        self.git("config", "user.email", "synthetic@example.invalid")
        (self.repository / "README.md").write_text("Synthetic test source\n")
        self.commit("Original synthetic revision")
        self.scanner = self.directory / "scanner"
        self.scanner.write_text("#!/usr/bin/env python3\nimport pathlib,sys\n"
                                "if sys.argv[1]=='version': print('synthetic-scanner');sys.exit(0)\n"
                                "pathlib.Path(sys.argv[sys.argv.index('--report-path')+1]).write_text('[]')\n")
        self.scanner.chmod(0o700)
        self.reviews = self.directory / "reviews.json"
        self.reviews.write_text(json.dumps({"reviews": []}))

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=str(self.repository), env=self.env,
                              check=True, capture_output=True).stdout

    def commit(self, message):
        self.git("add", "--all")
        self.git("commit", "-m", message)

    def test_deleted_private_path_still_holds_history_review(self):
        private = self.repository / ".local/pending.json"
        private.parent.mkdir()
        private.write_text('{"synthetic": true}\n')
        self.commit("Synthetic private-path fixture")
        private.unlink()
        self.commit("Remove current private path")
        result = audit(self.repository, self.scanner, self.directory / "output", self.reviews)
        self.assertEqual(result["result"], "held")
        self.assertEqual(result["commits"], 3)
        self.assertEqual(result["current_files"], 1)
        self.assertEqual(result["path_issues"][0]["path"], ".local/pending.json")

    def test_shallow_clone_and_existing_evidence_cannot_pass(self):
        clone = self.directory / "shallow"
        subprocess.run(["git", "clone", "--depth=1", self.repository.as_uri(), str(clone)],
                       env=self.env, check=True, capture_output=True)
        with self.assertRaisesRegex(ValueError, "Full history"):
            audit(clone, self.scanner, self.directory / "shallow-output", self.reviews)
        output = self.directory / "existing-output"
        output.mkdir()
        with self.assertRaises(FileExistsError):
            audit(self.repository, self.scanner, output, self.reviews)


if __name__ == "__main__":
    unittest.main()
