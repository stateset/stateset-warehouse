"""Storage admission and credential-free observations for qualification runs."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import qualification_host as HOST


class QualificationHostTests(unittest.TestCase):
    def test_any_insufficient_target_holds_preflight_including_the_exact_boundary(self):
        gib = 1024 ** 3
        for free, ok in ((5 * gib - 1, False), (5 * gib, True)):
            with self.subTest(free=free), tempfile.TemporaryDirectory() as directory:
                def usage(path):
                    available = free if path == 'artifacts' else 10 * gib
                    return SimpleNamespace(total=20 * gib, used=20 * gib - available, free=available)
                with patch.object(HOST.shutil, 'disk_usage', side_effect=usage):
                    report = HOST.storage_preflight({'workspace': 'workspace', 'artifacts': 'artifacts'}, 5, Path(directory))
                self.assertEqual(report['ok'], ok)
                self.assertEqual(report['required_available_bytes'], 5 * gib)
                self.assertEqual(report['reasons'], [] if ok else ['artifacts: insufficient available storage'])

    def test_uninspectable_storage_cannot_pass_and_does_not_expose_exception_details(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(HOST.shutil, 'disk_usage', side_effect=PermissionError('private-path-sentinel')):
                report = HOST.storage_preflight({'workspace': '/unreadable'}, 5, Path(directory))
        self.assertFalse(report['ok'])
        self.assertEqual(report['filesystems']['workspace']['error'], 'PermissionError')
        self.assertNotIn('private-path-sentinel', json.dumps(report))

    def test_linux_observations_do_not_include_environment_or_require_linux_files(self):
        with tempfile.TemporaryDirectory() as directory:
            proc = Path(directory)
            (proc / 'pressure').mkdir()
            (proc / 'pressure/io').write_text('some avg10=92.50 avg60=81.00 avg300=45.00 total=123\n')
            (proc / 'meminfo').write_text('MemAvailable:   12345 kB\n')
            with patch.dict(os.environ, {'WMS_API_KEYS_JSON': 'private-environment-sentinel'}):
                report = HOST.host_snapshot({'artifacts': proc}, proc)
        self.assertEqual(report['available_memory_bytes'], 12345 * 1024)
        self.assertIn('avg10=92.50', report['linux_pressure']['io'])
        self.assertIsNone(report['linux_pressure']['cpu'])
        self.assertNotIn('private-environment-sentinel', json.dumps(report))

    def test_reports_are_private_exclusive_and_preserve_the_first_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'observation.json'
            HOST.write_report(path, {'ok': False})
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            with self.assertRaises(FileExistsError):
                HOST.write_report(path, {'ok': True})
            self.assertEqual(json.loads(path.read_text()), {'ok': False})

    def test_cli_retains_real_low_storage_report_and_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'host.json'
            result = subprocess.run([sys.executable, str(Path(HOST.__file__)), '--path', 'artifacts=' + directory,
                                     '--output', str(output), '--minimum-free-gib', '1000000000000'],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 1, result.stderr)
            report = json.loads(output.read_text())
            self.assertFalse(report['ok'])
            self.assertGreater(report['filesystems']['artifacts']['total_bytes'], 0)
            self.assertIn('insufficient', result.stdout)

    def test_cli_rejects_invalid_policy_or_duplicate_labels_before_writing(self):
        with tempfile.TemporaryDirectory() as directory:
            for arguments in (['--minimum-free-gib', '0'], ['--minimum-free-gib', '-1'],
                              ['--minimum-free-gib', 'NaN'], ['--path', 'workspace=' + directory]):
                with self.subTest(arguments=arguments):
                    output = Path(directory) / 'host.json'
                    result = subprocess.run([sys.executable, str(Path(HOST.__file__)), '--path', 'workspace=' + directory,
                                             '--output', str(output), *arguments], capture_output=True, text=True)
                    self.assertEqual(result.returncode, 2)
                    self.assertFalse(output.exists())

    def test_both_actual_entrypoints_reject_storage_before_build_or_docker_calls(self):
        repository = Path(__file__).resolve().parents[1]
        for script, variable in [('qualify-release.sh', 'WMS_RELEASE_ARTIFACT_DIR'),
                                 ('resilience-rehearsal.sh', 'WMS_RESILIENCE_ARTIFACT_DIR')]:
            with self.subTest(script=script), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                binaries = root / 'bin'
                binaries.mkdir()
                for name in ('docker', 'cargo'):
                    executable = binaries / name
                    executable.write_text('#!/bin/sh\necho unexpected >> "$QUALIFICATION_CALLS"\nexit 99\n')
                    executable.chmod(0o700)
                evidence = root / 'evidence'
                calls = root / 'calls'
                env = dict(os.environ, PATH=str(binaries) + os.pathsep + os.environ['PATH'],
                           QUALIFICATION_CALLS=str(calls), WMS_QUALIFICATION_MIN_FREE_GIB='1000000000000',
                           WMS_RESILIENCE_IMAGE='sha256:' + '0' * 64, **{variable: str(evidence)})
                result = subprocess.run(['bash', str(repository / 'scripts' / script)], cwd=repository,
                                        env=env, capture_output=True, text=True, timeout=20)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertFalse(calls.exists(), 'Storage admission must precede Docker and Cargo')
                self.assertFalse(json.loads((evidence / 'host-preflight.json').read_text())['ok'])
                if script == 'qualify-release.sh':
                    summary = json.loads((evidence / 'summary.json').read_text())
                    self.assertEqual(summary['result'], 'failed')
                    self.assertEqual(summary['stages'][0]['status'], 'failed')
                    self.assertTrue(all(s['status'] == 'not_run' for s in summary['stages'][1:]))
                    self.assertTrue((evidence / 'publish/host-preflight.json').exists())


if __name__ == '__main__':
    unittest.main()
