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


def history():
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

    def event(sku, version, labels, location):
        eid = f"{sku}-{version}"
        steps = []
        for n, label in enumerate(labels, 1):
            old = current.get((sku, label))
            rev = 1 if old is None else old["revision"] + 1
            step = {
                "tenant": "resilience-test",
                "sku": sku,
                "serial": label,
                "lot": "LOT-000",
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
                "lot": "LOT-000",
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
            "request": {"serial_inputs": deepcopy(inputs)},
            "actor": "actor",
        }

    for i in range(2):
        event(
            f"LOAD-{i:05}",
            1,
            [f"Load-{i:05}-Lot-000-Unit-{n:05}" for n in range(2)],
            f"BIN-{i:05}",
        )
    event("LOAD-00000", 2, ["Load-00000-Lot-000-Unit-00000"], "BIN-00001")
    tables["wms_serial_units"] = list(current.values())
    tables["wms_serial_heads"] = [
        {
            "sku": f"LOAD-{i:05}",
            "version": 2 if i == 0 else 1,
            "last_event_id": f"LOAD-{i:05}-{2 if i==0 else 1}",
        }
        for i in range(2)
    ]
    return tables, accepted


class SerialEvidenceTests(unittest.TestCase):
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
