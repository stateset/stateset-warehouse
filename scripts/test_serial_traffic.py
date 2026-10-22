"""Independent evidence regressions; simulated services are not capacity evidence."""

from copy import deepcopy
import json
import unittest
import urllib.parse
from serial_traffic_proof import serial_proof
import test_resilience_probe as legacy


class SerialClient(legacy.SimulatedProbe):
    def __init__(self, bad=None):
        super().__init__()
        self.args.serial_units = 2
        self.bad = bad
        self.stock = {k: 2 if n else 0 for k, n in self.stock.items()}

    def http(self, base, method, path, token, body=None):
        if method == "GET" and path.startswith("/v1/serials?"):
            q = urllib.parse.parse_qs(urllib.parse.urlsplit(path).query)
            i = int(token.split("-")[-1])
            label = self.serial_id(i, 0, 0)
            assert q == {"sku": [self.sku(i)], "serial": [label], "limit": ["1"]}
            self.calls.append(("serial", token, base))
            place = {
                "status": "on_hand",
                "warehouse_id": "wh-main",
                "location": self.bin(i + self.commits[token] % 2),
            }
            item = {
                "sku": self.sku(i),
                "serial": label,
                "lot": self.lot(0),
                "source_ok": True,
                "place": place,
            }
            page = {
                "page_source_ok": True,
                "total_records": 1,
                "next_cursor": None,
                "items": [item],
            }
            if self.bad == "source" and token == "token-0":
                page["page_source_ok"] = False
            if self.bad == "literal" and token == "token-0":
                item["serial"] = label.lower()
            if self.bad == "custody" and token == "token-0":
                item["place"]["location"] = self.bin(i)
            return 200, page
        return super().http(base, method, path, token, body)


class SerialSchedulingTests(unittest.TestCase):
    run_probe = legacy.ResilienceTrafficTests.run_probe

    def test_short_rotating_run_retains_observed_coverage_and_fails_acceptance(self):
        class TwoMoveClient(SerialClient):
            def reviewed_move(self, index, move_number, *args):
                if move_number >= 2:
                    return None
                return super().reviewed_move(index, move_number, *args)

        p = TwoMoveClient()
        p.args.serial_pattern = "rotating"
        p.args.lots = 1
        p.args.min_serial_coverage = 1
        with self.assertRaisesRegex(AssertionError, "required serial population"):
            self.run_probe(p)
        report = p.phase_evidence["sustained_http"]
        self.assertFalse(report["acceptance"]["ok"])
        self.assertEqual(report["serial_labels_available_per_operator"], 2)
        self.assertTrue(any(n < 2 for n in report["serial_distinct_labels_per_operator"]))

    # Reuse the independent scheduler clock, not the parent protocol cases.
    def test_serial_clients_replay_the_same_label_and_validate_each_visible_save(self):
        p = SerialClient()
        r = self.run_probe(p)
        self.assertEqual(r["serial_current_source_reads"], r["committed_moves"])
        self.assertEqual(
            r["serial_current_source_latency"]["samples"], r["committed_moves"]
        )
        for i in range(2):
            posts = [x for x in p.calls if x[:2] == ("post", f"token-{i}")]
            for first, replay in zip(posts[::2], posts[1::2]):
                self.assertEqual(first[3], replay[3])
                self.assertEqual(
                    first[3]["serial_inputs"],
                    [
                        {
                            "target": [],
                            "sku": p.sku(i),
                            "serials": [p.serial_id(i, 0, 0)],
                        }
                    ],
                )
                self.assertTrue(
                    any(
                        x[:2] == ("serial", f"token-{i}")
                        for x in p.calls[
                            p.calls.index(first) + 1 : p.calls.index(replay)
                        ]
                    )
                )

    def test_bad_source_substituted_literal_and_invisible_custody_stop_before_another_request(
        self,
    ):
        for bad in ["source", "literal", "custody"]:
            with self.subTest(bad=bad):
                p = SerialClient(bad)
                with self.assertRaisesRegex(AssertionError, "serial"):
                    self.run_probe(p)
                self.assertNotIn("sustained_http", p.phase_evidence)
                self.assertEqual(p.commits["token-0"], 1)
                self.assertEqual(p.replays["token-0"], 0)


def history(recovery=False, moves=None, lots=1, units=2, pattern="fixed"):
    moves = [1, 0] if moves is None else moves
    tables = {
        name: []
        for name in [
            "wms_events",
            "wms_serial_units",
            "wms_serial_steps",
            "wms_serial_observations",
            "wms_serial_batches",
            "wms_serial_heads",
        ]
    }
    accepted = {}
    current = {}

    def event(sku, version, labels, location, command=None, lot="LOT-000"):
        eid = f"{sku}-{version}"
        steps = []
        for n, label in enumerate(labels, 1):
            old = current.get((sku, label))
            rev = 1 if old is None else old["revision"] + 1
            step = {
                "tenant": "resilience-test",
                "sku": sku,
                "serial": label,
                "lot": lot,
                "revision": rev,
                "previous_revision": None if rev == 1 else rev - 1,
                "sku_version": version,
                "warehouse_id": "wh-main",
                "warehouse_version": len(tables["wms_events"]) + 1,
                "event_id": eid,
                "ordinal": n,
                "target": [],
                "before_place": None if old is None else old["place"],
                "after_place": {
                    "status": "on_hand",
                    "warehouse_id": "wh-main",
                    "location": location,
                },
                "actor": "actor",
                "recorded_at": "synthetic",
            }
            tables["wms_serial_steps"].append(step)
            steps.append({k: v for k, v in step.items() if k != "tenant"})
            current[(sku, label)] = {
                "sku": sku,
                "serial": label,
                "lot": lot,
                "revision": rev,
                "place": deepcopy(step["after_place"]),
                "last_event_id": eid,
            }
        inputs = [{"target": [], "sku": sku, "serials": labels}]
        payload = {
            "request_id": eid,
            "actor": "actor",
            "serial_inputs": deepcopy(inputs),
            "serial_steps": steps,
            "serial_heads": {sku: version},
        }
        if command is not None:
            payload["command"] = deepcopy(command)
        tables["wms_events"].append(
            {
                "id": eid,
                "warehouse_id": "wh-main",
                "version": len(tables["wms_events"]) + 1,
                "payload": payload,
            }
        )
        tables["wms_serial_observations"].append(
            {
                "event_id": eid,
                "ordinal": 1,
                "warehouse_id": "wh-main",
                "sku_version": version,
                **deepcopy(inputs[0]),
            }
        )
        tables["wms_serial_batches"].append(
            {
                "sku": sku,
                "version": version,
                "warehouse_id": "wh-main",
                "warehouse_version": len(tables["wms_events"]),
                "event_id": eid,
            }
        )
        accepted[eid] = {
            "request": {"request_id": eid, "serial_inputs": deepcopy(inputs),
                        **({"command": deepcopy(command)} if command is not None else {})},
            "actor": "actor",
        }

    versions = [0, 0]
    for i in range(2):
        for lot in range(lots):
            versions[i] += 1
            event(f"LOAD-{i:05}", versions[i],
                  [f"Load-{i:05}-Lot-{lot:03}-Unit-{n:05}" for n in range(units)],
                  f"BIN-{i:05}", lot=f"LOT-{lot:03}")

    def recover():
        for returning in range(2):
            versions[1] += 1
            event("LOAD-00001", versions[1], [f"Load-00001-Lot-000-Unit-{units-1:05}"],
                  f"BIN-{returning:05}",
                  {"type": "move_stock", "sku": "LOAD-00001", "lot": "LOT-000",
                   "from": f"BIN-{1-returning:05}", "to": f"BIN-{returning:05}", "quantity": 1})

    if recovery and pattern == "rotating":
        recover()
    for i, count in enumerate(moves):
        for move in range(count):
            # Enumerate out-and-back pairs independently from Probe's selector.
            labels = [(lot, unit) for lot in range(lots) for unit in range(units)]
            lot, unit = labels[(move // 2) % len(labels)] if pattern == "rotating" else (0, 0)
            versions[i] += 1
            event(f"LOAD-{i:05}", versions[i], [f"Load-{i:05}-Lot-{lot:03}-Unit-{unit:05}"],
                  f"BIN-{(i + (move + 1) % 2) % 2:05}", lot=f"LOT-{lot:03}")
    if recovery and pattern == "fixed":
        recover()
    tables["wms_serial_units"] = list(current.values())
    tables["wms_serial_heads"] = [
        {"sku": f"LOAD-{i:05}", "version": version,
         "last_event_id": f"LOAD-{i:05}-{version}"}
        for i, version in enumerate(versions)
    ]
    return tables, accepted


class SerialEvidenceTests(unittest.TestCase):
    def test_rotating_population_covers_lots_wraparound_and_odd_final_moves(self):
        for moves in ([0, 0], [1, 3], [11, 12], [25, 31]):
            with self.subTest(moves=moves):
                tables, accepted = history(moves=moves, lots=2, units=3, pattern="rotating")
                proof = serial_proof(tables, accepted, 2, 2, 3, moves, 2, pattern="rotating")
                self.assertTrue(proof["ok"])
                self.assertEqual(proof["distinct_timed_labels"],
                                 sum(min((n + 1) // 2, 6) for n in moves))

    def test_rotating_recovery_label_can_move_again_after_both_crashes(self):
        tables, accepted = history(recovery=True, moves=[17, 19], lots=2, units=3, pattern="rotating")
        proof = serial_proof(tables, accepted, 2, 2, 3, [17, 19], 2,
                             ["LOAD-00001-3", "LOAD-00001-4"], pattern="rotating")
        self.assertEqual(proof["recovered_moves_outside_timed_interval"], 2)
        self.assertEqual(proof["distinct_timed_labels"], 12)

    def test_fixed_label_history_cannot_pass_as_rotating_coverage(self):
        tables, accepted = history(moves=[8, 8])
        with self.assertRaises(AssertionError):
            serial_proof(tables, accepted, 2, 1, 2, [8, 8], 2, pattern="rotating")

    def test_rotating_requests_keep_pairs_exact_across_lots_and_wraparound(self):
        probe = SerialClient()
        probe.args.serial_pattern = "rotating"
        probe.args.serial_units = 2
        probe.args.lots = 2
        review = {"version": 0, "resource_versions": {}}
        expected = [(0, 0), (0, 1), (1, 0), (1, 1), (0, 0)]
        for pair, (lot, unit) in enumerate(expected):
            outbound = probe.move_request(0, pair * 2, review)
            inbound = probe.move_request(0, pair * 2 + 1, review)
            self.assertEqual(outbound["serial_inputs"], inbound["serial_inputs"])
            self.assertEqual(outbound["serial_inputs"][0]["serials"], [probe.serial_id(0, lot, unit)])
            self.assertEqual(outbound["command"]["lot"], probe.lot(lot))
            self.assertEqual(outbound["command"]["from"], inbound["command"]["to"])
            self.assertEqual(outbound["command"]["to"], inbound["command"]["from"])

    def test_recovered_original_serial_moves_are_separate_from_timed_traffic(self):
        tables, accepted = history(recovery=True)
        ids = ["LOAD-00001-2", "LOAD-00001-3"]
        proof = serial_proof(tables, accepted, 2, 1, 2, [1, 0], 2, ids)
        self.assertEqual(proof["timed_moves"], 1)
        self.assertEqual(proof["recovered_moves_outside_timed_interval"], 2)
        self.assertEqual(proof["recovery_request_ids"], ids)
        self.assertEqual(proof["steps"], 7)

    def test_recovery_requires_exact_original_pair_and_custody_not_only_final_totals(self):
        for damage in ["omitted", "duplicate", "reversed", "new_label", "original_move", "intermediate_place"]:
            with self.subTest(damage=damage):
                tables, accepted = history(recovery=True)
                ids = ["LOAD-00001-2", "LOAD-00001-3"]
                if damage == "omitted":
                    ids = []
                elif damage == "duplicate":
                    ids[1] = ids[0]
                elif damage == "reversed":
                    ids.reverse()
                elif damage == "new_label":
                    accepted[ids[0]]["request"]["serial_inputs"][0]["serials"] = ["Substitute"]
                elif damage == "original_move":
                    accepted[ids[0]]["request"]["command"]["quantity"] = 2
                else:
                    tables["wms_serial_steps"][-2]["after_place"]["location"] = "BIN-OTHER"
                    tables["wms_serial_steps"][-1]["before_place"]["location"] = "BIN-OTHER"
                    for e in tables["wms_events"][-2:]:
                        step = next(s for s in tables["wms_serial_steps"] if s["event_id"] == e["id"])
                        e["payload"]["serial_steps"] = [{k: v for k, v in step.items() if k != "tenant"}]
                with self.assertRaises(AssertionError):
                    serial_proof(tables, accepted, 2, 1, 2, [1, 0], 2, ids)

    def test_complete_literal_chain_and_original_observations_are_verified(self):
        tables, accepted = history()
        r = serial_proof(tables, accepted, 2, 1, 2, [1, 0], 2)
        self.assertTrue(r["ok"])
        self.assertEqual(r["literal_units"], 4)
        self.assertEqual(r["steps"], 5)

    def test_conserved_totals_cannot_hide_changed_literal_lot_custody_or_original_history(
        self,
    ):
        for damage in [
            "case",
            "lot",
            "custody",
            "predecessor",
            "missing_step",
            "original_group",
            "head",
            "batch",
        ]:
            with self.subTest(damage=damage):
                t, a = history()
                if damage == "case":
                    t["wms_serial_units"][0]["serial"] = t["wms_serial_units"][0][
                        "serial"
                    ].lower()
                if damage == "lot":
                    t["wms_serial_units"][0]["lot"] = "OTHER"
                if damage == "custody":
                    t["wms_serial_units"][0]["place"]["location"] = "BIN-00000"
                if damage == "predecessor":
                    t["wms_serial_steps"][-1]["before_place"]["location"] = "BIN-00001"
                if damage == "missing_step":
                    t["wms_serial_steps"].pop()
                if damage == "original_group":
                    t["wms_serial_observations"][0]["serials"][0] = "forged"
                    t["wms_events"][0]["payload"]["serial_inputs"][0]["serials"][
                        0
                    ] = "forged"
                if damage == "head":
                    t["wms_serial_heads"][0]["version"] = 1
                if damage == "batch":
                    t["wms_serial_batches"].pop()
                with self.assertRaises(AssertionError):
                    serial_proof(t, a, 2, 1, 2, [1, 0], 2)


if __name__ == "__main__":
    unittest.main()
