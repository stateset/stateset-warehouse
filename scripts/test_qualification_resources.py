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


class QualificationNetworkTests(unittest.TestCase):
    def create_network(self, subnet="", allocation=""):
        with tempfile.TemporaryDirectory(prefix="wms-network-test-") as directory:
            root = Path(directory)
            docker = root / "docker"
            docker.write_text(r'''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
with pathlib.Path(os.environ["CALLS"]).open("a") as stream:
    stream.write(json.dumps(args) + "\n")
if args[:2] == ["network", "create"]:
    print("synthetic-network-id")
elif args[:2] == ["network", "inspect"]:
    print(json.dumps([{"Subnet": os.environ.get("ALLOCATION_OVERRIDE") or os.environ.get("WMS_QUALIFICATION_SUBNET") or "172.21.0.0/16"}]))
else:
    sys.exit(1)
''')
            docker.chmod(0o700)
            env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"],
                       CALLS=str(root / "calls.jsonl"), WMS_QUALIFICATION_SUBNET=subnet,
                       ALLOCATION_OVERRIDE=allocation)
            result = subprocess.run(
                ["bash", "-c", 'source scripts/qualification_resources.sh; '
                 'wms_qualification_create_network "$1" owner.label owned-run owned-network',
                 "test-network", str(root)], cwd=ROOT, env=env, capture_output=True, text=True)
            calls = [json.loads(line) for line in (root / "calls.jsonl").read_text().splitlines()] if (root / "calls.jsonl").exists() else []
            selection = json.loads((root / "network-selection.json").read_text()) if (root / "network-selection.json").exists() else None
            identity = (root / "network.id").read_text().strip() if (root / "network.id").exists() else None
            return result, calls, selection, identity

    def test_explicit_private_ranges_retain_ownership_and_actual_allocation(self):
        for subnet in ("10.253.240.0/24", "172.16.1.0/25", "192.168.100.0/28"):
            with self.subTest(subnet=subnet):
                result, calls, selection, identity = self.create_network(subnet)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(calls[0], ["network", "create", "--label", "owner.label=owned-run", "--subnet", subnet, "owned-network"])
                self.assertEqual(identity, "synthetic-network-id")
                self.assertEqual(selection["requested_subnet"], subnet)
                self.assertEqual(selection["allocated_ipam"], [{"Subnet": subnet}])
                self.assertTrue(all(call[:2] in (["network", "create"], ["network", "inspect"]) for call in calls))

    def test_default_keeps_daemon_allocation_and_records_observed_range(self):
        result, calls, selection, _ = self.create_network()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--subnet", calls[0])
        self.assertIsNone(selection["requested_subnet"])
        self.assertEqual(selection["allocated_ipam"], [{"Subnet": "172.21.0.0/16"}])

    def test_invalid_ranges_fail_before_any_docker_action(self):
        for subnet in ("8.8.8.0/24", "127.0.0.0/24", "169.254.1.0/24", "100.64.1.0/24", "224.0.0.0/24", "10.0.0.0/8", "10.0.0.0/29", "10.0.0.1/24", "fd00::/64", "10.0.0.0/24\nother", "--driver=host"):
            with self.subTest(subnet=subnet):
                result, calls, selection, identity = self.create_network(subnet)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(calls, [])
                self.assertIsNone(selection)
                self.assertIsNone(identity)

    def test_wrong_allocation_fails_and_retains_created_identity_without_removal(self):
        result, calls, selection, identity = self.create_network("10.253.240.0/24", "10.253.241.0/24")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("did not retain the requested", result.stderr)
        self.assertIsNone(selection)
        self.assertEqual(identity, "synthetic-network-id")
        self.assertEqual([call[:2] for call in calls], [["network", "create"], ["network", "inspect"]])

    def test_profile_ranges_must_be_complete_and_nonoverlapping(self):
        for values, accepted in [
            (["", "", ""], True),
            (["10.253.240.0/24", "10.253.241.0/24", "10.253.242.0/24"], True),
            (["10.253.240.0/24", "", ""], False),
            (["10.253.240.0/24", "10.253.240.0/24", "10.253.242.0/24"], False),
            (["10.253.240.0/24", "10.253.240.0/25", "10.253.242.0/24"], False),
        ]:
            with self.subTest(values=values):
                result = subprocess.run(["bash", "-c", 'source scripts/qualification_resources.sh; wms_qualification_validate_network_subnets "$@"', "test-subnets", *values], cwd=ROOT, capture_output=True, text=True)
                self.assertEqual(result.returncode == 0, accepted, result.stderr)


if __name__ == "__main__":
    unittest.main()
