"""Exercise ownership and retained-data cleanup through the actual shell helper."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
DOCKER = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
with pathlib.Path(os.environ["CALLS"]).open("a") as stream:
    stream.write(json.dumps(args) + "\n")
state = json.loads(os.environ["RESOURCES"])
name = args[-1]
if "inspect" in args:
    if name not in state: sys.exit(1)
    if ".State" in args[args.index("--format") + 1]:
        print(json.dumps({"Status": "exited", "Running": False}))
    else: print(state[name])
elif args[0] == "stop" and name == os.environ.get("FAIL_STOP"):
    sys.exit(1)
'''


class ResourceCleanupTests(unittest.TestCase):
    def run_cleanup(self, keep="1", resources=None, fail_stop=""):
        with tempfile.TemporaryDirectory(prefix="wms-resource-test-") as directory:
            root = Path(directory)
            docker = root / "docker"
            docker.write_text(DOCKER)
            docker.chmod(0o700)
            env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"],
                       CALLS=str(root / "calls.jsonl"), FAIL_STOP=fail_stop,
                       RESOURCES=json.dumps(resources if resources is not None else
                                            {name: "owned-run" for name in ("api", "pg", "volume", "network")}),
                       WMS_KEEP_QUALIFICATION_RESOURCES=keep)
            result = subprocess.run(
                ["bash", "-c", 'source scripts/qualification_resources.sh; '
                 'wms_qualification_finish_resources "$1" owner.label owned-run network volume api pg',
                 "test-cleanup", str(root)], cwd=ROOT, env=env, capture_output=True, text=True)
            calls = [json.loads(line) for line in (root / "calls.jsonl").read_text().splitlines()] if (root / "calls.jsonl").exists() else []
            rows = [json.loads(line) for line in (root / "resource-disposition.jsonl").read_text().splitlines()] if (root / "resource-disposition.jsonl").exists() else []
            states = {p.name: json.loads(p.read_text()) for p in root.glob("*-final-state.json")}
            return result, calls, rows, states

    def test_retention_stops_both_services_keeps_data_and_records_state(self):
        result, calls, rows, states = self.run_cleanup()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([c[-1] for c in calls if c[0] == "stop"], ["api", "pg"])
        self.assertFalse(any("rm" in c for c in calls))
        self.assertEqual([r["outcome"] for r in rows], ["retained"] * 4)
        self.assertTrue(all(r["retention_requested"] for r in rows))
        self.assertEqual(set(states), {"api-final-state.json", "pg-final-state.json"})
        self.assertTrue(all(not state["Running"] for state in states.values()))

    def test_default_removes_only_owned_resources_in_dependency_order(self):
        result, calls, rows, _ = self.run_cleanup(keep="0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([c for c in calls if "rm" in c], [
            ["rm", "--force", "--volumes", "api"], ["rm", "--force", "--volumes", "pg"],
            ["volume", "rm", "volume"], ["network", "rm", "network"]])
        self.assertEqual([r["outcome"] for r in rows], ["removed"] * 4)

    def test_wrong_owners_are_never_stopped_or_removed(self):
        for mode in ("0", "1"):
            with self.subTest(mode=mode):
                result, calls, rows, _ = self.run_cleanup(mode, {name: "another-run" for name in ("api", "pg", "volume", "network")})
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue(all("inspect" in c for c in calls))
                self.assertEqual([r["outcome"] for r in rows], ["ownership_mismatch"] * 4)

    def test_stop_failure_fails_cleanup_and_still_stops_postgres(self):
        result, calls, rows, _ = self.run_cleanup(fail_stop="api")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual([c[-1] for c in calls if c[0] == "stop"], ["api", "pg"])
        self.assertFalse(any("rm" in c for c in calls))
        self.assertEqual([r["outcome"] for r in rows], ["failed", "retained", "retained", "retained"])

    def test_partial_startup_leaves_missing_resources_alone(self):
        result, calls, rows, _ = self.run_cleanup(resources={"network": "owned-run"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(all("inspect" in c for c in calls))
        self.assertEqual([r["outcome"] for r in rows], ["not_observed"] * 3 + ["retained"])

    def test_invalid_retention_rejected_before_any_docker_call(self):
        result, calls, rows, _ = self.run_cleanup(keep="yes")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must be 0 or 1", result.stderr)
        self.assertEqual((calls, rows), ([], []))


if __name__ == "__main__":
    unittest.main()
