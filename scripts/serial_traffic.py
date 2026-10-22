#!/usr/bin/env python3
"""Development serial HTTP traffic on two explicitly owned native API processes.

Uses the existing independent-client scheduler. This is not an image, crash,
failover, scanner-device or facility release qualification.
"""

import argparse
import collections
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time
import threading
import urllib.parse

from resilience_probe import Probe, require
from serial_traffic_proof import serial_proof
import traffic_policy


class NativeSerialProbe(Probe):
    def __init__(self, args):
        self.args = args
        self.root = Path(args.artifacts)
        ownership = json.loads((self.root / "ownership.json").read_text())
        self.database = ownership["database"]
        u = urllib.parse.urlsplit(self.database)
        q = dict(urllib.parse.parse_qsl(u.query))
        require(
            u.scheme == "postgresql"
            and not u.netloc
            and re.fullmatch(r"/wms_serial_load_test_[a-f0-9]{12}", u.path)
            and re.fullmatch(
                r"/tmp/stateset-wms-test\.[A-Za-z0-9]+/socket", q.get("host", "")
            ),
            "Requires a uniquely owned synthetic database from the native launcher",
        )
        self.bases = ownership["bases"]
        require(
            len(set(self.bases)) == 2
            and all(re.fullmatch(r"http://127\.0\.0\.1:[0-9]+", x) for x in self.bases),
            "Requires two distinct owned loopback API endpoints",
        )
        self.binary = Path(ownership["binary"])
        require(
            hashlib.sha256(self.binary.read_bytes()).hexdigest()
            == ownership["binary_sha256"],
            "Native executable changed",
        )
        self.credentials = json.loads((self.root / "credentials.json").read_text())
        self.admin = next(
            k
            for k, v in self.credentials.items()
            if v["actor"] == "qualification:admin"
        )
        self.outsider = next(
            k for k, v in self.credentials.items() if v["tenant"] != self.tenant
        )
        self.operators = sorted(
            (v["actor"], k)
            for k, v in self.credentials.items()
            if v["role"] == "operator"
        )
        require(
            2 <= len(self.operators) <= args.products,
            "Operator population must fit distinct operational SKUs",
        )
        self.accepted = {}
        self.phase_evidence = {}
        self.seeded_orders = []
        self.started = time.time()
        self.sample_lock = threading.Lock()
        self.ready()
        require(
            self.table("wms_products") == [] and self.table("wms_commands") == [],
            "Owned load database must start empty",
        )

    def sql(self, sql):
        return subprocess.run(
            ["psql", self.database, "-XqAt", "-v", "ON_ERROR_STOP=1"],
            input=sql,
            check=True,
            capture_output=True,
            text=True,
            timeout=30,
        ).stdout.strip()

    def reviewed_move(
        self, index, move_number, reviewed, started, refresh_after_save, stop=None
    ):
        sample = super().reviewed_move(
            index, move_number, reviewed, started, refresh_after_save, stop
        )
        if sample is not None:
            row = {k: v for k, v in sample.items() if k != "reviewed"}
            row["actor"] = self.operators[index][0]
            row["action_start_seconds"] = (
                row.pop("action_started_monotonic") - self.traffic_started_monotonic
            )
            row["cycle_end_seconds"] = (
                row.pop("cycle_ended_monotonic") - self.traffic_started_monotonic
            )
            with self.sample_lock:
                with (self.root / "cycle-samples.jsonl").open("a") as output:
                    output.write(json.dumps(row, separators=(",", ":")) + "\n")
        return sample

    def independent_proof(self):
        names = [
            "wms_stock",
            "wms_movements",
            "wms_allocations",
            "wms_commands",
            "wms_events",
            "wms_activity",
            "wms_outbox",
            "wms_serial_units",
            "wms_serial_steps",
            "wms_serial_observations",
            "wms_serial_heads",
            "wms_serial_batches",
        ]
        tables = {name: self.table(name) for name in names}
        physical = collections.Counter()
        reserved = collections.Counter()
        for r in tables["wms_movements"]:
            if r["from_location"] is not None:
                physical[(r["sku"], r["lot"], r["from_location"])] -= r["quantity"]
            if r["to_location"] is not None:
                physical[(r["sku"], r["lot"], r["to_location"])] += r["quantity"]
        for r in tables["wms_allocations"]:
            reserved[(r["sku"], r["lot"], r["location"])] += (
                r["quantity"] - r["picked"] - r["released"]
            )
            reserved[(r["sku"], r["lot"], "STAGE-01")] += r["picked"] - r["shipped"]
        actual = {
            (r["sku"], r["lot"], r["location"]): (r["on_hand"], r["reserved"])
            for r in tables["wms_stock"]
        }
        require(
            all(
                actual.get(k, (0, 0)) == (physical[k], reserved[k])
                for k in set(actual) | set(physical) | set(reserved)
            ),
            "Independent physical and reservation equations disagree",
        )
        expected = self.expected_setup_commands() + sum(self.moves)
        require(
            all(
                len(tables[n]) == expected
                for n in ["wms_commands", "wms_events", "wms_activity", "wms_outbox"]
            )
            and len(self.accepted) == expected,
            "Atomic command/event/activity/outbox counts disagree",
        )
        require(
            len(tables["wms_movements"])
            == self.args.products * self.args.lots + sum(self.moves),
            "Physical move count changed",
        )
        require(
            sum(r["on_hand"] for r in tables["wms_stock"])
            == self.args.products * self.args.lots * self.args.serial_units
            and sum(r["reserved"] for r in tables["wms_stock"])
            == (self.args.open_orders + 1) // 2,
            "Physical or reserved total changed",
        )
        require(
            collections.Counter(
                (o["event_id"], o["destination"]) for o in tables["wms_outbox"]
            )
            == collections.Counter((e["id"], "oms") for e in tables["wms_events"]),
            "An original event lacks its one exact OMS intent",
        )
        receipts = {c["request_id"]: c for c in tables["wms_commands"]}
        for event in tables["wms_events"]:
            payload = event["payload"]
            saved = self.accepted[payload["request_id"]]
            receipt = receipts[payload["request_id"]]
            require(
                payload["actor"] == receipt["actor"] == saved["actor"]
                and payload["result"] == receipt["result"] == saved["result"]
                and payload["command"] == saved["request"]["command"],
                "Original actor, result or command changed",
            )
        require(
            sorted(e["version"] for e in tables["wms_events"])
            == list(range(1, expected + 1))
            and self.version() == expected,
            "Warehouse event versions are not contiguous",
        )
        require(
            all(
                o["status"] == "pending" and o["attempts"] == 0
                for o in tables["wms_outbox"]
            )
            and self.table("wms_delivery_attempts") == [],
            "Unexpected external delivery",
        )
        serials = serial_proof(
            tables,
            self.accepted,
            self.args.products,
            self.args.lots,
            self.args.serial_units,
            self.moves,
            self.args.bins,
            pattern=self.args.serial_pattern,
        )
        require(serials["distinct_timed_labels"] == sum(
            self.phase_evidence["sustained_http"]["serial_distinct_labels_per_operator"]),
            "Observed serial coverage differs from independent literal custody proof")
        before = self.fingerprints()
        for i, (actor, token) in enumerate(self.operators):
            own = next(s for s in self.accepted.values() if s["actor"] == actor)
            status, _ = self.http(
                self.bases[1 - i % 2],
                "POST",
                self.route + "/commands",
                self.operators[(i + 1) % len(self.operators)][1],
                own["request"],
            )
            require(status == 409, "Another actor replayed original serial custody")
        for base in self.bases:
            status, page = self.http(base, "GET", self.route, self.outsider)
            require(
                status == 200
                and page["version"] == 0
                and page["stocks"] == []
                and page["products"] == [],
                "Cross-tenant inventory leaked",
            )
            reviewed, _ = self.snapshot_review(base, self.admin)
            require(
                reviewed["version"] == expected,
                "Final operational snapshot lost a command",
            )
            status, proof = self.http(
                base, "GET", self.route + "/reconciliation", self.admin, timeout=30
            )
            require(
                status == 200
                and proof["ok"] is True
                and proof["serials"]["source_ok"] is True
                and proof["serials"]["physical_ok"] is True,
                "Final complete serial reconciliation failed",
            )
        require(
            before == self.fingerprints(),
            "Final reviews or forbidden replays changed evidence",
        )
        return {
            "ok": True,
            "commands": expected,
            "physical_and_reserved_equations_recomputed": True,
            "atomic_original_actors_results_commands_verified": True,
            "cross_actor_and_tenant_isolation": True,
            "complete_serial_proof": serials,
            "final_table_fingerprints": before,
        }

    def run(self):
        try:
            self.seed()
            self.sustained()
            proof = self.independent_proof()
            report = {
                "ok": True,
                "synthetic": True,
                "production_qualified": False,
                "qualification": "development native serial HTTP traffic",
                "binary_sha256": hashlib.sha256(self.binary.read_bytes()).hexdigest(),
                "binary_path": str(self.binary),
                "script_sha256": {
                    p: hashlib.sha256(Path(p).read_bytes()).hexdigest()
                    for p in [
                        "scripts/resilience_probe.py",
                        "scripts/serial_traffic.py",
                        "scripts/serial_traffic_proof.py",
                        "scripts/traffic_policy.py",
                        "scripts/serial-traffic-native.sh",
                    ]
                },
                "cycle_samples": {
                    "path": "cycle-samples.jsonl",
                    "sha256": hashlib.sha256(
                        (self.root / "cycle-samples.jsonl").read_bytes()
                    ).hexdigest(),
                },
                "postgres_version": self.sql("SHOW server_version;"),
                "phases": self.phase_evidence,
                "independent_proof": proof,
                "limits": [
                    "Explicit native executable identified by SHA-256; optimization profile requires separate build provenance. Shared development host, loopback HTTP, one PostgreSQL primary; not production image, crash/failover, deployed, scanner-device, native receiver or facility qualification.",
                    "Opening counts are synthetic complete observed sets; the recorded serial pattern and distinct-label counts describe the active population, not whole-facility capacity.",
                ],
            }
            (self.root / "serial-traffic.json").write_text(
                json.dumps(report, indent=2) + "\n"
            )
            print(
                json.dumps(
                    {
                        "ok": True,
                        "committed_moves": sum(self.moves),
                        "proof": proof["complete_serial_proof"],
                    }
                ),
                flush=True,
            )
        except BaseException as error:
            (self.root / "serial-traffic-failed.json").write_text(
                json.dumps(
                    {"ok": False, "error": str(error), "phases": self.phase_evidence},
                    indent=2,
                )
                + "\n"
            )
            raise
        finally:
            (self.root / "accepted-journal.json").write_text(
                json.dumps(self.accepted, indent=2) + "\n"
            )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", required=True)
    parser.add_argument("--seconds", type=int, default=300)
    parser.add_argument("--products", type=int, default=8)
    parser.add_argument("--lots", type=int, default=1)
    parser.add_argument("--bins", type=int, default=8)
    parser.add_argument("--open-orders", type=int, default=8)
    parser.add_argument("--serial-units", type=int, default=16)
    parser.add_argument("--serial-pattern", choices=("fixed", "rotating"), default="rotating")
    traffic_policy.arguments(parser)
    args = parser.parse_args()
    args.review_mode = "resources"
    args.traffic_model = "independent-operators"
    require(
        10 <= args.seconds <= 3600
        and 2 <= args.products <= 32
        and 1 <= args.lots <= 4
        and 2 <= args.bins <= 64
        and 2 <= args.serial_units <= 64
        and 0 <= args.open_orders <= 64,
        "Native diagnostic scope exceeds its bounded profile",
    )
    require(
        args.serial_units > (args.open_orders + args.products - 1) // args.products,
        "Opening custody must exceed active allocations",
    )
    NativeSerialProbe(args).run()


if __name__ == "__main__":
    main()
