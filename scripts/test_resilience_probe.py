"""Protocol regressions with simulated service state; not warehouse capacity evidence."""
from argparse import Namespace
from copy import deepcopy
import importlib.util
import io
import json
from pathlib import Path
from types import SimpleNamespace
import threading
import time
import unittest
from unittest.mock import patch
from contextlib import redirect_stdout

SPEC = importlib.util.spec_from_file_location("resilience_probe", Path(__file__).with_name("resilience_probe.py"))
PROBE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROBE)


class SimulatedProbe(PROBE.Probe):
    """Model request ownership and delayed reads independently of the scheduler."""
    def __init__(self, traffic="independent-operators", review="resources", fault=None):
        self.args = Namespace(seconds=0.5, traffic_model=traffic, review_mode=review, bins=3)
        self.operators = [(f"actor:{i}", f"token-{i}") for i in range(2)]
        self.credentials = {t: {"actor": a} for a, t in self.operators}
        self.bases = ["replica-a", "replica-b"]
        self.accepted = {}
        self.phase_evidence = {}
        self.lock = threading.Lock()
        self.version = 0
        self.stock = {(self.sku(i), self.lot(0), self.bin(j)): 100000 if i == j else 0
                      for i in range(2) for j in range(3)}
        self.versions = {}
        self.receipts = {}
        self.reads = {t: 0 for _, t in self.operators}
        self.commits = {t: 0 for _, t in self.operators}
        self.replays = {t: 0 for _, t in self.operators}
        self.calls = []
        self.fault = fault
        self.delayed_review = threading.Event()
        self.allow_review = threading.Event()
        self.fast_cycles_before_slow_review = 0

    def clock(self):
        # Advance the simulated interval only after every client has completed
        # two cycles. Test results must not depend on development-host speed.
        with self.lock:
            return 1.0 if all(n >= 2 for n in self.replays.values()) else 0.0

    def snapshot_review(self, base=None, token=None):
        time.sleep(0.0005)  # Simulate I/O yielding, without imposing a client barrier.
        token = token or self.operators[0][1]
        if self.fault == "slow_initial_review" and token == "token-1" and self.reads[token] == 0:
            self.delayed_review.set()
            if not self.allow_review.wait(1):
                raise AssertionError("The fast operator was blocked by another operator's review")
        with self.lock:
            self.reads[token] += 1
            self.calls.append(("read", token, base))
            snapshot = {"version": self.version, "resource_versions": deepcopy(self.versions),
                        "stocks": [{"sku": s, "lot": l, "location": b, "on_hand": q}
                                   for (s, l, b), q in self.stock.items()]}
            if token == "token-0" and self.commits[token]:
                if self.fault == "stale_refresh":
                    snapshot["version"] = 0
                elif self.fault == "invisible_move":
                    for stock in snapshot["stocks"]:
                        if stock["sku"] == self.sku(0):
                            stock["on_hand"] = 100000 if stock["location"] == self.bin(0) else 0
                elif self.fault == "refresh_unavailable":
                    raise OSError("Save committed but refresh unavailable")
            return snapshot, 0.01

    def http(self, base, method, path, token, body):
        time.sleep(0.0005)
        if token == "token-0" and self.fault == "slow_initial_review":
            if not self.delayed_review.wait(1):
                raise AssertionError("Second operator never began its review")
        with self.lock:
            self.calls.append(("post", token, base, deepcopy(body)))
            request_id = body["request_id"]
            if request_id in self.receipts:
                original = self.receipts[request_id]
                if original["request"] != body or original["actor"] != self.credentials[token]["actor"]:
                    return 409, {"error": "request-key conflict"}
                response = {"replayed": True, "result": deepcopy(original["result"])}
                if self.fault == "changed_replay" and token == "token-0":
                    response["result"]["version"] += 1
                self.replays[token] += 1
                return 200, response
            c = body["command"]
            key = lambda *p: json.dumps(p, separators=(",", ":"))
            resources = [key("stock", c["sku"], c["lot"], c["from"]),
                         key("stock", c["sku"], c["lot"], c["to"]), key("product", c["sku"]),
                         key("location", c["from"]), key("location", c["to"])]
            if "resource_versions" in body:
                expected = {k: self.versions.get(k, 0) for k in resources}
                matches = expected == body["resource_versions"]
            else:
                matches = body["expected_version"] == self.version
            if not matches:
                return 409, {"command_outcome": {"reason": "changed_resources" if "resource_versions" in body else "stale_version"}}
            self.stock[(c["sku"], c["lot"], c["from"])] -= 1
            self.stock[(c["sku"], c["lot"], c["to"])] += 1
            self.version += 1
            for k in resources[:2]:
                self.versions[k] = self.versions.get(k, 0) + 1
            result = {"version": self.version, "entity_id": c["sku"]}
            self.receipts[request_id] = {"request": deepcopy(body), "result": result,
                                         "actor": self.credentials[token]["actor"]}
            self.commits[token] += 1
            if self.fault == "slow_initial_review" and token == "token-0" and self.reads["token-1"] == 0:
                self.fast_cycles_before_slow_review += 1
                if self.fast_cycles_before_slow_review >= 3:
                    self.allow_review.set()
            return 200, {"replayed": False, "result": deepcopy(result)}


class ResilienceTrafficTests(unittest.TestCase):
    def run_probe(self, probe):
        with redirect_stdout(io.StringIO()), patch.object(PROBE, "time", SimpleNamespace(monotonic=probe.clock)):
            probe.sustained()
        return probe.phase_evidence["sustained_http"]

    def test_independent_clients_do_not_wait_at_an_operator_round_barrier(self):
        probe = SimulatedProbe(fault="slow_initial_review")
        report = self.run_probe(probe)
        self.assertGreaterEqual(probe.fast_cycles_before_slow_review, 3)
        self.assertIsNone(report["rounds"])
        self.assertEqual(report["shared_round_snapshot_reads"], 0)
        self.assertTrue(report["independent_post_commit_stock_visibility_verified"])
        self.assertEqual(report["action_latency"]["samples"], report["committed_moves"])
        self.assertEqual(report["frozen_replay_latency"]["samples"], report["committed_moves"])
        self.assertEqual(report["operational_snapshot_latency"]["samples"], report["committed_moves"] + 2)
        for index, op in enumerate(report["operators"]):
            self.assertGreater(op["commits"], 0)
            self.assertEqual(op["commits"], op["exact_replays"])
            self.assertEqual(op["commits"], op["post_commit_refreshes"])
            self.assertEqual(op["snapshot_reads"], op["commits"] + 1)
            self.assertEqual(op["initial_snapshot_reads"], 1)
            posts = [call for call in probe.calls if call[:2] == ("post", f"token-{index}")]
            for first, replay in zip(posts[::2], posts[1::2]):
                self.assertNotEqual(first[2], replay[2])
                self.assertEqual(first[3], replay[3], "The exact original envelope must survive replay")
                commit_index = probe.calls.index(first)
                replay_index = probe.calls.index(replay)
                self.assertTrue(any(call[:2] == ("read", f"token-{index}")
                                    for call in probe.calls[commit_index+1:replay_index]),
                                "Visible save latency must include the client's refresh before deliberate replay")

    def test_shared_snapshot_protocol_keeps_its_original_timing_and_round_counts(self):
        report = self.run_probe(SimulatedProbe(traffic="shared-snapshot"))
        self.assertEqual(report["committed_moves"], report["rounds"] * 2)
        self.assertEqual(report["shared_round_snapshot_reads"], report["rounds"])
        self.assertEqual(report["operational_snapshot_latency"]["samples"], report["rounds"])
        self.assertFalse(report["independent_post_commit_stock_visibility_verified"])
        self.assertIn("shared round snapshot", report["action_latency_scope"])
        self.assertTrue(all(op["post_commit_refreshes"] == 0 for op in report["operators"]))

    def test_shared_warehouse_fences_still_force_reviewed_rejections(self):
        report = self.run_probe(SimulatedProbe(traffic="shared-snapshot", review="warehouse"))
        self.assertGreaterEqual(report["definite_stale_conflicts"], report["rounds"])
        self.assertEqual(report["http_write_attempt_latency"]["samples"],
                         report["committed_moves"] + report["definite_stale_conflicts"])

    def test_failed_refresh_or_replay_cannot_generate_another_physical_request(self):
        for fault, message in [("stale_refresh", "predates"), ("invisible_move", "physical movement"),
                               ("refresh_unavailable", "refresh unavailable"), ("changed_replay", "replay changed")]:
            with self.subTest(fault=fault):
                probe = SimulatedProbe(fault=fault)
                with self.assertRaisesRegex((AssertionError, OSError), message):
                    self.run_probe(probe)
                self.assertNotIn("sustained_http", probe.phase_evidence)
                self.assertEqual(probe.commits["token-0"], 1)
                requests = [c[3] for c in probe.calls if c[:2] == ("post", "token-0")]
                self.assertEqual(len({r["request_id"] for r in requests}), 1)
                self.assertTrue(all(r == requests[0] for r in requests))

    def test_independent_legacy_clients_refresh_each_definitive_rejection(self):
        report = self.run_probe(SimulatedProbe(review="warehouse"))
        self.assertEqual(report["operational_snapshot_latency"]["samples"],
                         2 + report["committed_moves"] + report["definite_stale_conflicts"])
        self.assertEqual(report["http_write_attempt_latency"]["samples"],
                         report["committed_moves"] + report["definite_stale_conflicts"])




class SnapshotCoverageTests(unittest.TestCase):
    def fixture(self):
        probe = PROBE.Probe.__new__(PROBE.Probe)
        probe.args = Namespace(products=2, open_orders=2)
        probe.seeded_orders = ["allocated-source", "pending-source"]
        snapshot = {"warehouse": {"id": "wh-main"},
                    "products": [{"sku": sku} for sku in ["PROBE", probe.sku(0), probe.sku(1)]],
                    "orders": [{"id": "allocated-source", "status": "allocated"},
                               {"id": "pending-source", "status": "pending"},
                               {"id": "retained-shipment", "status": "shipped"}]}
        return probe, snapshot

    def test_complete_catalog_and_active_identities_allow_retained_history(self):
        probe, snapshot = self.fixture()
        probe.check_snapshot_coverage(snapshot)

    def test_equal_counts_cannot_hide_substituted_catalog_or_pending_work(self):
        for fault in ["product", "pending_order", "status", "warehouse", "duplicate_order"]:
            with self.subTest(fault=fault):
                probe, snapshot = self.fixture()
                if fault == "product":
                    snapshot["products"][2]["sku"] = "UNRELATED-SUBSTITUTE"
                elif fault == "pending_order":
                    snapshot["orders"][1]["id"] = "UNRELATED-ORDER"
                elif fault == "status":
                    snapshot["orders"][1]["status"] = "cancelled"
                elif fault == "warehouse":
                    snapshot["warehouse"]["id"] = "wh-other"
                else:
                    snapshot["orders"][2] = deepcopy(snapshot["orders"][1])
                with self.assertRaises(AssertionError):
                    probe.check_snapshot_coverage(snapshot)


if __name__ == "__main__":
    unittest.main()
