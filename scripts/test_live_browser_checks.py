"""The standalone browser runner must reject unsafe inputs before starting work."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().with_name('live-browser-checks.sh')


class LiveBrowserAdmissionTests(unittest.TestCase):
    def test_database_identity_overrides_and_non_test_targets_never_start_a_build(self):
        for url in ['postgres://private:secret@host/production',
                    'postgres://private:secret@host/warehouse_test?dbname=production',
                    'postgres://host/warehouse_test?SERVICE=production',
                    'postgres://host/test%2Fproduction', 'https://host/test']:
            with self.subTest(url=url), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                binary = root / 'cargo'
                binary.write_text('#!/bin/sh\ntouch "$WMS_BROWSER_BUILD_SENTINEL"\nexit 99\n')
                binary.chmod(0o700)
                sentinel = root / 'build'
                env = dict(os.environ, PATH=directory + os.pathsep + os.environ['PATH'],
                           WMS_TEST_DATABASE_URL=url, WMS_BROWSER_BUILD_SENTINEL=str(sentinel),
                           WMS_BROWSER_ARTIFACT_DIR=str(root / 'evidence'))
                result = subprocess.run(['bash', str(SCRIPT)], env=env,
                                        capture_output=True, text=True, timeout=10)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(sentinel.exists())
                self.assertFalse((root / 'evidence').exists())
                self.assertNotIn('private:secret', result.stdout + result.stderr)

    def test_existing_evidence_is_preserved_before_any_build_or_database_work(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            evidence = root / 'original.json'
            evidence.write_text('{"ok":false}')
            env = dict(os.environ, WMS_TEST_DATABASE_URL='postgres:///owned_test',
                       WMS_BROWSER_ARTIFACT_DIR=str(root))
            result = subprocess.run(['bash', str(SCRIPT)], env=env,
                                    capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(evidence.read_text(), '{"ok":false}')
            self.assertEqual(list(root.iterdir()), [evidence])


if __name__ == '__main__':
    unittest.main()
