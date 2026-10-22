"""Fault-path evidence regressions; these are not facility acceptance tests."""

from argparse import Namespace
from copy import deepcopy
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

from crash_journal import CrashJournal, verify_journal, verify_table_inventory
from resilience_probe import Probe


class CrashTableInventoryTests(unittest.TestCase):
    def setUp(self):
        self.restored = {f"wms_fixture_{n}": {"rows": n + 1, "sha256": "a" * 64}
                         for n in range(49)}
        self.restored["wms_serial_pending_checks"] = {
            "rows": 0, "sha256": hashlib.sha256(b"[]").hexdigest()}
        self.crashed = deepcopy(self.restored)
        self.crashed["wms_fixture_0"] = {"rows": 27, "sha256": "b" * 64}

    def test_complete_inventory_uses_actual_names_instead_of_a_fixed_count(self):
        verify_table_inventory(self.crashed, self.restored)
        self.crashed["wms_new_history"] = {"rows": 2, "sha256": "c" * 64}
        self.restored["wms_new_history"] = {"rows": 1, "sha256": "d" * 64}
        verify_table_inventory(self.crashed, self.restored)

    def test_missing_extra_and_same_count_substituted_tables_are_rejected(self):
        for variant in ("missing", "extra", "substituted"):
            with self.subTest(variant=variant):
                damaged = deepcopy(self.crashed)
                if variant != "extra":
                    damaged.pop("wms_fixture_0")
                if variant != "missing":
                    damaged["wms_unexpected"] = {"rows": 0, "sha256": "a" * 64}
                with self.assertRaisesRegex(AssertionError, "inventories differ"):
                    verify_table_inventory(damaged, self.restored)

    def test_pending_checks_require_zero_rows_and_the_exact_empty_digest(self):
        for side in ("crash", "restore"):
            for item in ({"rows": 1, "sha256": "a" * 64},
                         {"rows": 0, "sha256": "a" * 64}):
                with self.subTest(side=side, item=item):
                    crashed, restored = deepcopy(self.crashed), deepcopy(self.restored)
                    (crashed if side == "crash" else restored)["wms_serial_pending_checks"] = item
                    with self.assertRaisesRegex(AssertionError, "must be empty"):
                        verify_table_inventory(crashed, restored)

    def test_malformed_or_omitted_inventory_cannot_count_as_complete(self):
        for item in ({"rows": -1, "sha256": "a" * 64},
                     {"rows": False, "sha256": "a" * 64},
                     {"rows": 0, "sha256": "invalid"}, {"rows": 0}, None):
            with self.subTest(item=item):
                damaged = deepcopy(self.crashed)
                damaged["wms_fixture_0"] = item
                with self.assertRaisesRegex(AssertionError, "Invalid table fingerprint"):
                    verify_table_inventory(damaged, self.restored)
        for damaged in (None, {}, []):
            with self.assertRaisesRegex(AssertionError, "inventories differ"):
                verify_table_inventory(damaged, self.restored)


class CrashJournalTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.request = {"request_id": "original-request", "expected_version": 7,
                        "resource_versions": {'["stock","SKU"]': 3},
                        "command": {"type": "move_stock", "quantity": 1},
                        "serial_inputs": [{"target": [], "sku": "SKU", "serials": ["Literal-'é"]}]}

    def journal(self):
        return CrashJournal(self.root, "api_sigkill", "owned-run", "operator:one", self.request)

    def events(self):
        return [json.loads(p.read_text()) for p in sorted((self.root / "crash-journal/api_sigkill").glob("*.json"))]

    def test_original_wire_body_and_actor_survive_caller_mutation(self):
        original = deepcopy(self.request)
        journal = self.journal()
        self.request["serial_inputs"][0]["serials"][0] = "substitute"
        facts = self.events()[0]["facts"]
        self.assertEqual(facts["original_request"], original)
        self.assertEqual(facts["actor"], "operator:one")
        self.assertEqual(facts["http_body"], json.dumps(original, separators=(",", ":")))
        self.assertEqual(hashlib.sha256(facts["http_body"].encode()).hexdigest(), facts["http_body_sha256"])
        self.assertEqual(len(journal.manifest()["private_checkpoints"]), 1)

    def test_private_permissions_even_with_permissive_umask(self):
        previous = os.umask(0)
        try:
            journal = self.journal()
        finally:
            os.umask(previous)
        self.assertEqual(journal.directory.stat().st_mode & 0o777, 0o700)
        self.assertEqual(journal.directory.parent.stat().st_mode & 0o777, 0o700)
        for path in journal.directory.iterdir():
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_failed_checkpoint_keeps_previous_events_without_publishing_partial_json(self):
        journal = self.journal()
        before = journal.manifest()
        with patch("crash_journal.os.fsync", side_effect=OSError("disk failure")):
            with self.assertRaisesRegex(OSError, "disk failure"):
                journal.record("fault_requested", signal="KILL")
        self.assertEqual(journal.manifest(), before)
        self.assertEqual([p.name for p in journal.directory.iterdir()], ["000-prepared.json"])

    def test_existing_attempt_and_checkpoint_cannot_be_overwritten(self):
        journal = self.journal()
        original = (journal.directory / "000-prepared.json").read_bytes()
        with self.assertRaises(FileExistsError):
            self.journal()
        (journal.directory / "001-completed.json").write_text("existing evidence")
        with self.assertRaises(FileExistsError):
            journal.record("completed")
        self.assertEqual((journal.directory / "001-completed.json").read_text(), "existing evidence")
        self.assertEqual((journal.directory / "000-prepared.json").read_bytes(), original)

    def test_modified_private_event_prevents_successful_manifest(self):
        journal = self.journal()
        (journal.directory / "000-prepared.json").write_text("{}")
        with self.assertRaisesRegex(AssertionError, "checkpoint changed"):
            journal.manifest()

    def test_cannot_follow_journal_directory_symlink(self):
        (self.root / "crash-journal").symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "Unsafe crash journal"):
            self.journal()

    def test_release_verifier_rejects_missing_reordered_and_changed_checkpoints(self):
        journal = self.journal()
        stages = ["baseline_captured", "original_send_requested", "writer_blocked",
                  "fault_requested", "interrupted_response_observed", "fault_verified",
                  "original_absent", "original_recovery_requested", "original_committed",
                  "exact_replay_verified", "completed"]
        for stage in stages:
            journal.record(stage)
        manifest = journal.manifest()
        self.assertEqual(len(verify_journal(self.root, manifest, "api_sigkill")), 12)
        for indexes in ([0, *range(2, 12)], [1, 0, *range(2, 12)], list(range(11))):
            damaged = deepcopy(manifest)
            damaged["private_checkpoints"] = [damaged["private_checkpoints"][i] for i in indexes]
            with self.assertRaises((AssertionError, ValueError)):
                verify_journal(self.root, damaged, "api_sigkill")
        changed = self.root / manifest["private_checkpoints"][4]["path"]
        changed.write_text("{}")
        with self.assertRaisesRegex(AssertionError, "digest mismatch"):
            verify_journal(self.root, manifest, "api_sigkill")

    def test_original_is_saved_before_barrier_installation_failure_and_http_send(self):
        probe = self.probe()
        def failed_locker(request, token):
            self.assertEqual(self.events()[0]["facts"]["original_request"], self.request)
            raise RuntimeError("barrier installation failed")
        probe.locker = failed_locker
        with self.assertRaisesRegex(RuntimeError, "barrier installation failed"):
            probe.crash_api()
        probe.http.assert_not_called()
        self.assertEqual([e["stage"] for e in self.events()], ["prepared", "baseline_captured"])

    def probe(self):
        probe = Probe.__new__(Probe)
        probe.root = self.root
        probe.args = Namespace(serial_units=2, run="owned-run", api=["api-a", "api-b"])
        probe.operators = [("operator:one", "private-test-token")]
        probe.credentials = {"private-test-token": {"actor": "operator:one"}}
        probe.bases = ["http://replica-a", "http://replica-b"]
        probe.phase_evidence = {}
        probe.recovery_request = Mock(return_value=self.request)
        probe.fingerprints = Mock(side_effect=[{"table": "before"}, {"table": "changed"}])
        probe.http = Mock(return_value=(503, {}))
        probe.sql = Mock()
        probe.owned = Mock(return_value={"State": {"ExitCode": 137}})
        probe.docker = Mock()
        probe.wait_blocked = Mock()
        return probe

    def test_failure_after_trigger_cleanup_preserves_original_without_claiming_recovery(self):
        probe = self.probe()
        probe.locker = Mock(return_value=("owned-locker", Mock()))
        probe.remove_serial_barrier = Mock()
        # The survivor readiness check succeeds; the interrupted command returns 503.
        probe.http.side_effect = lambda base, method, path, *args: (200, {}) if path == "/ready" else (503, {})
        with self.assertRaisesRegex(AssertionError, "did not roll back"):
            probe.crash_api()
        probe.remove_serial_barrier.assert_called_once()
        events = self.events()
        self.assertEqual(events[0]["facts"]["original_request"], self.request)
        stages = [e["stage"] for e in events]
        self.assertIn("fault_requested", stages)
        self.assertIn("fault_verified", stages)
        self.assertNotIn("original_recovery_requested", stages)
        self.assertNotIn("completed", stages)
        self.assertNotIn("private-test-token", "".join(p.read_text() for p in probe.crash_journal.directory.iterdir()))
        self.assertEqual(probe.phase_evidence, {})


if __name__ == "__main__":
    unittest.main()
