"""Current facility evidence: authenticated claims, immutable captures and equations.

This offline verifier does not observe a warehouse or establish a signer's authority.
The caller pins the reviewed release and obtains witness keys independently.
"""
from collections import defaultdict
from datetime import datetime, timezone
from decimal import Decimal
import hashlib
import json
import math
import os
import stat
from pathlib import Path
import re

PROFILE = "current-v2"
BASE_SCENARIOS = {
    "receiving_exact_lines", "putaway", "allocation_same_sku_lines",
    "picking_exact_lines", "packing_handoff", "wrong_label_rejected",
    "short_pick_return_release_recount", "stale_operator_rescan",
    "lost_response_exact_replay", "tenant_and_role_denial",
    "oms_conditional_handoff", "tms_planned_load", "backup_restore_reconcile",
}
CURRENT_SCENARIOS = BASE_SCENARIOS | {
    "facility_selection_pending_save", "replenishment_actual_move",
    "receipt_return_remainder_cancellation", "partial_packages_backorders_returns",
    "partial_order_remainder_recovery", "facility_transfer_custody",
    "lot_expiry_quarantine_recovery", "cross_facility_lot_trace",
    "transfer_loss_reversal_arrival", "reviewed_pack_units",
}
STAGES = ["prerequisites", "software_checks", "observability_rules", "release_builds",
          "standard_workload", "archive_restore", "live_browsers", "production_container",
          "image_resilience", "stable_source_inputs"]
TRACE_SECTIONS = {"facilities", "positions", "allocations", "receipts", "shipments",
                  "transfers", "movements", "policies", "decisions"}
# Required successful canonical actions, in addition to typed source/custody checks.
COMMANDS = {
    "receiving_exact_lines": [{"create_receipt", "receive"}],
    "putaway": [{"move_stock"}],
    "allocation_same_sku_lines": [{"create_order", "allocate"}, {"create_order", "allocate_available"}],
    "picking_exact_lines": [{"pick"}],
    "packing_handoff": [{"ship"}, {"handoff_shipment"}],
    "short_pick_return_release_recount": [{"report_exception", "unpick", "reset_allocation", "count_stock", "resolve_exception"},
                                           {"report_exception", "unpick", "release_unpicked", "count_stock", "resolve_exception"}],
    "stale_operator_rescan": [{"receive"}, {"pick"}],
    "lost_response_exact_replay": [{"receive"}, {"pick"}, {"move_stock"}],
    "replenishment_actual_move": [{"move_stock"}],
    "receipt_return_remainder_cancellation": [{"create_return", "receive", "cancel_receipt"}],
    "partial_packages_backorders_returns": [{"allocate_available", "pack_shipment", "handoff_shipment", "create_return"}],
    "partial_order_remainder_recovery": [{"unpack_shipment", "unpick", "release_unpicked", "cancel_remainder"}],
    "facility_transfer_custody": [{"dispatch_transfer", "receive_transfer"}],
    "lot_expiry_quarantine_recovery": [{"set_product_expiry_policy", "record_lot_expiry", "quarantine_lot", "release_lot_hold", "unpack_shipment", "unpick"}],
    "transfer_loss_reversal_arrival": [{"record_transfer_loss", "reverse_transfer_loss", "receive_transfer"}],
    "reviewed_pack_units": [{"set_unit_pack", "receive", "move_stock", "count_stock"}],
}
KINDS = {"command_journal", "native_receipts", "trace_pages", "full_reconciliation",
         "peak_samples", "recovery", "deployment_observation", "physical_notes", "device_observations", "device_capture", "archive"}


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def digest_text(q, value):
    q.require(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value), "Invalid evidence content digest")
    return value



def instant(q, value):
    parsed = q.stamp(value).replace(microsecond=0)
    delta = parsed - datetime(1970, 1, 1, tzinfo=timezone.utc)
    fraction = re.search(r"\.(\d+)(?=(?:Z|[+-]\d\d:\d\d)$)", value)
    nanos = int((fraction.group(1) if fraction else "").ljust(9, "0"))
    return Decimal(delta.days * 86400 + delta.seconds) + Decimal(nanos) / Decimal(1000000000)

def image_text(q, value):
    q.require(isinstance(value, str) and re.fullmatch(r"sha256:[0-9a-f]{64}", value), "Invalid immutable image digest")
    return value


def sequence(q, value, maximum=10000, nonempty=True):
    q.require(isinstance(value, list) and len(value) <= maximum and (value or not nonempty), "Evidence array is missing or exceeds its limit")
    return value



def same_json(a, b):
    """Python's True == 1 must never establish a JSON receipt/source match."""
    if type(a) is not type(b): return False
    if isinstance(a, dict): return a.keys() == b.keys() and all(same_json(a[k], b[k]) for k in a)
    if isinstance(a, list): return len(a) == len(b) and all(same_json(x, y) for x, y in zip(a, b))
    return a == b


def canonical_semantics(q, request):
    q.require(isinstance(request, dict) and set(request) <= {"request_id", "expected_version", "resource_versions", "unit_inputs", "command"}, "Captured request contains unknown envelope fields")
    out = dict(request)
    if out.get("resource_versions") is None: out.pop("resource_versions", None)
    if out.get("unit_inputs", []) == []:
        q.require(isinstance(out.get("unit_inputs", []), list), "Unit inputs must be a captured array")
        out.pop("unit_inputs", None)
    command = dict(out["command"])
    for kind, key in [("create_order", "source_order_id"), ("report_exception", "order_id")]:
        if command.get("type") == kind and key not in command: command[key] = None
    out["command"] = command
    return out

def scoped(q, value, expected):
    q.require(all(value.get(k) == expected[k] for k in ("facility_id", "wms_tenant", "warehouse_id")),
              "Evidence belongs to a different facility, tenant or warehouse")



def artifact_snapshot(q, root, relative, limit=32 * 1024 * 1024, retain=True):
    q.require(isinstance(relative, str) and relative and not Path(relative).is_absolute()
              and all(part not in {".", ".."} for part in relative.split("/")), "Artifact path must stay beneath its root")
    # Directory descriptors and O_NOFOLLOW reject both symlink escapes and a
    # rename/symlink substitution between validation and opening the bytes.
    descriptors = []
    try:
        directory = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        descriptors.append(directory)
        parts = Path(relative).parts
        for part in parts[:-1]:
            directory = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            descriptors.append(directory)
        fd = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
        with os.fdopen(fd, "rb") as stream:
            before = os.fstat(stream.fileno())
            q.require(stat.S_ISREG(before.st_mode) and before.st_size <= limit, "Captured artifact must be a bounded regular file")
            chunks, size, sha = [], 0, hashlib.sha256()
            while True:
                chunk = stream.read(min(1024 * 1024, limit - size + 1))
                if not chunk: break
                size += len(chunk)
                q.require(size <= limit, "Captured artifact exceeds its byte limit")
                sha.update(chunk)
                if retain: chunks.append(chunk)
            after = os.fstat(stream.fileno())
            q.require((before.st_size, before.st_mtime_ns, before.st_ctime_ns) == (after.st_size, after.st_mtime_ns, after.st_ctime_ns)
                      and size == before.st_size, "Captured artifact changed while being read")
        return (b"".join(chunks) if retain else None), sha.hexdigest(), size
    finally:
        for fd in reversed(descriptors): os.close(fd)


def artifact_bytes(q, root, relative, limit=32 * 1024 * 1024):
    return artifact_snapshot(q, root, relative, limit)[0]

def release(q, args):
    root = Path(args.qualified_release).resolve()
    # A trusted caller pins the exact approval summary, rather than trusting a
    # newly authored manifest to announce that its own tests passed.
    raw = artifact_bytes(q, root, "summary.json", 2 * 1024 * 1024)
    q.require(digest(raw) == digest_text(q, args.expected_release_summary_sha256), "Reviewed software release summary digest differs")
    summary = q.decode(raw)
    q.require(summary.get("result") == "passed" and summary.get("production_qualified") is False,
              "A passing local software qualification bundle is required")
    q.require([s["name"] for s in summary["stages"]] == STAGES and all(s["status"] == "passed" and type(s["exit_code"]) is int and s["exit_code"] == 0 for s in summary["stages"]),
              "Software qualification has a missing or failed stage")
    contents = {}
    for item in sequence(q, summary["artifacts"], 100):
        name = item["path"]
        q.require(isinstance(name, str) and Path(name).name == name and name not in contents, "Software artifact names must be unique local files")
        data = artifact_bytes(q, root, name)
        q.require(digest(data) == digest_text(q, item["sha256"]), "Reviewed software artifact content differs")
        contents[name] = data
    def obj(name):
        q.require(name in contents, "Software qualification artifact is missing")
        return q.decode(contents[name])
    before, after = obj("source-inputs-before.json"), obj("source-inputs-after.json")
    q.require(before == after and before["sha256"] == digest_text(q, args.expected_source_inputs_sha256), "Software inputs changed or differ from the caller's reviewed source")
    q.require(digest(json.dumps(before["inputs"], sort_keys=True, separators=(",", ":")).encode()) == before["sha256"], "Software source manifest digest is inconsistent")
    source_names = [x["path"] for x in before["inputs"]]
    # qualify-release.sh sorts Path objects: a directory's children precede a
    # sibling such as restore_probe.rs. Verify that exact producer order rather
    # than string order, which incorrectly rejects accepted serial bundles.
    q.require(len(set(source_names)) == len(source_names) and source_names == sorted(source_names, key=Path), "Software source manifest repeats or reorders inputs")
    for item in before["inputs"]:
        digest_text(q, item["sha256"])
        q.require(isinstance(item["path"], str) and not Path(item["path"]).is_absolute() and ".." not in Path(item["path"]).parts, "Invalid software source path")
    provenance, container, resilience = obj("provenance.json"), obj("production_container.json"), obj("image_resilience.json")
    q.require(provenance.get("ok") is True and container.get("ok") is True and resilience.get("ok") is True, "Software source/image proofs did not pass")
    image = image_text(q, args.expected_image_digest)
    q.require(provenance["image_id"] == container["image_id"] == resilience["image_id"] == image, "Software qualification used a different immutable image")
    q.require(provenance["stable_software_inputs_sha256"] == before["sha256"], "Provenance software source differs")
    image_source = digest_text(q, provenance["image_source_inputs_sha256"])
    q.require(image_source == container["source_inputs_sha256"] == resilience["image_source_inputs_sha256"], "Image build-input identities differ")
    restore = obj("archive_restore.json")
    q.require(restore.get("ok") is True and provenance.get("restore_container_resilience_migrations_identical") is True,
              "Software archive or migration identity is unverified")
    migrations = {row["name"]: row["sha256"] for row in container["migrations"]}
    q.require(provenance["migrations"] == migrations and container["migrations"] == resilience["migrations"] == restore["verification"]["migrations"], "Qualified migration histories differ")
    # Build-input and all-software hashes intentionally have distinct scopes.
    return {"source_inputs_sha256": before["sha256"], "image_source_inputs_sha256": image_source,
            "image_digest": image, "release_summary_sha256": digest(raw), "qualified_at": summary["recorded_at"],
            "schema_tables_sha256": digest(json.dumps(sorted(restore["verification"]["table_evidence"]), separators=(",", ":")).encode()),
            "migrations_sha256": digest(json.dumps(provenance["migrations"], sort_keys=True, separators=(",", ":")).encode())}


class Artifacts:
    """Hash and consume one immutable byte snapshot; never hash then reopen."""
    def __init__(self, q, root, entries):
        self.q, self.data, self.entries, self.total = q, {}, {}, 0
        retained = 0
        root = Path(root).resolve()
        for item in sequence(q, entries, 256):
            key = q.text(item["id"], "artifact identity")
            q.require(key not in self.data and item.get("kind") in KINDS, "Duplicate or untyped evidence artifact")
            p = item["path"]
            q.require(isinstance(p, str) and p and not Path(p).is_absolute() and ".." not in Path(p).parts,
                      "Artifact path must stay beneath the evidence root")
            # Backups and hardware captures can exceed the structured-data
            # budget. Hash their one open descriptor without retaining bytes.
            retain = item["kind"] not in {"archive", "device_capture"}
            raw, sha, size = artifact_snapshot(q, root, p, 1024**4 if not retain else 32 * 1024**2, retain)
            self.total += size
            retained += size if retain else 0
            q.require(retained <= 128 * 1024**2, "Captured structured artifacts exceed the validation byte budget")
            q.require(sha == digest_text(q, item["sha256"]), "Captured artifact digest differs from the signed evidence")
            self.data[key], self.entries[key] = raw, item

    def objects(self, kind):
        return [(key, self.q.decode(raw)) for key, raw in self.data.items() if self.entries[key]["kind"] == kind]

    def reference(self, case, kind):
        ids = sequence(self.q, case["artifact_ids"], 256)
        self.q.require(len(set(ids)) == len(ids) and all(i in self.data for i in ids), "Scenario artifact references are missing or duplicated")
        selected = [(key, self.q.decode(self.data[key])) for key in ids if self.entries[key]["kind"] == kind]
        self.q.require(selected, "Scenario lacks its required typed captured artifact")
        return selected


class Journal:
    def __init__(self, q, artifacts, evidence):
        self.q, self.records, self.originals, self.by_capture = q, [], {}, {}
        events, versions, last_version = set(), set(), {}
        for artifact_id, document in artifacts.objects("command_journal"):
            q.require(document.get("schema_version") == 1, "Unsupported command capture schema")
            for row in sequence(q, document["records"], 10000):
                key = q.text(row["capture_id"], "command capture identity")
                q.require(key not in self.by_capture, "Command capture identity is repeated")
                q.text(row["actor"], "captured actor")
                q.identifier(row["tenant"]); q.identifier(row["warehouse_id"])
                q.require(row["role"] in {"admin", "operator", "viewer", "oms"}, "Captured authorization role is invalid")
                request = row["request"]; rid = request["request_id"]
                q.require(isinstance(rid, str) and re.fullmatch(r"[A-Za-z0-9_-]{8,100}", rid), "Original command request identity is invalid")
                q.integer(request["expected_version"], "reviewed command version")
                q.text(request["command"]["type"], "canonical command type")
                q.require(row["phase"] in {"baseline", "performed"}, "Command capture must distinguish prior context from the actual facility run")
                status = row["http_status"]
                q.require(type(status) is int and status in {200, 400, 401, 403, 404, 409, 422}, "Uncertain or invalid command response cannot establish a scenario result")
                if status == 200:
                    q.require(row["tenant"] == evidence["wms_tenant"], "Successful capture belongs to a foreign tenant")
                    event, receipt, body = row["event"], row["receipt"], row["body"]
                    q.require(type(body["replayed"]) is bool and event["tenant"] == receipt["tenant"] == row["tenant"]
                              and event["warehouse_id"] == receipt["warehouse_id"] == row["warehouse_id"]
                              and event["actor"] == receipt["actor"] == row["actor"]
                              and event["request_id"] == receipt["request_id"] == rid
                              and same_json(event["result"], receipt["result"]) and same_json(event["result"], body["result"])
                              and same_json(event["command"], canonical_semantics(q, request)["command"])
                              and same_json(event.get("unit_inputs", []), request.get("unit_inputs", [])),
                              "Captured command, original event, actor or durable receipt differs")
                    q.integer(event["version"], "original event version", 1)
                    q.text(event["id"], "original event identity")
                    q.require(event["version"] == body["result"]["version"] and event["type"] == body["result"]["type"], "Original result version/type differs")
                    digest_text(q, receipt["digest"])
                    canonical = row["canonical_request_json"]
                    q.require(isinstance(canonical, str) and digest(canonical.encode("utf-8")) == receipt["digest"]
                              and same_json(q.decode(canonical.encode("utf-8")), canonical_semantics(q, request)),
                              "Original canonical request bytes do not match the durable receipt digest")
                    q.require(instant(q, event["occurred_at"]) <= instant(q, evidence["ended_at"]) + 300, "Original command event is later than the facility run")
                    identity = (row["tenant"], row["warehouse_id"], rid)
                    original = self.originals.get(identity)
                    if original:
                        q.require(same_json(original["request"], request) and original["actor"] == row["actor"] and same_json(original["event"], event)
                                  and same_json(original["receipt"], receipt) and body["replayed"] is True,
                                  "Exact replay changed the original request/effect or repeated a fresh commit")
                    else:
                        q.require(body["replayed"] is False, "An original committed capture is required before its replay")
                        occurred = instant(q, event["occurred_at"])
                        q.require(occurred <= instant(q, evidence["started_at"]) if row["phase"] == "baseline" else instant(q, evidence["started_at"]) <= occurred <= instant(q, evidence["ended_at"]),
                                  "Original command timestamp contradicts its baseline/performed phase")
                        q.require(event["version"] > request["expected_version"], "Original command predates its reviewed version")
                        version_key = (row["tenant"], row["warehouse_id"], event["version"])
                        q.require(event["id"] not in events and version_key not in versions and event["version"] > last_version.get(version_key[:2], 0),
                                  "Original event identity/version is repeated or out of causal capture order")
                        events.add(event["id"]); versions.add(version_key); last_version[version_key[:2]] = event["version"]
                        command = request["command"]
                        admin_only = {"set_product_expiry_policy", "set_unit_pack", "record_transfer_loss", "reverse_transfer_loss", "correct_lot_expiry", "release_lot_hold", "create_warehouse", "update_warehouse", "create_product", "create_location", "count_stock"}
                        q.require(row["role"] == "admin" or row["role"] == "operator" and command["type"] not in admin_only
                                  or row["role"] == "oms" and (command["type"] in {"cancel_order", "cancel_remainder"} or command["type"] == "create_order" and command.get("source_order_id")),
                                  "Captured successful command contradicts its authenticated role")
                        self.originals[identity] = row
                else:
                    q.require(row.get("event") is None and row.get("receipt") is None, "A denied command capture claims a committed effect")
                row = dict(row, artifact_id=artifact_id)
                self.by_capture[key] = row; self.records.append(row)
        q.require(self.records, "Original command/effect captures are missing")

    def for_case(self, case):
        keys = sequence(self.q, case["capture_ids"], 10000)
        self.q.require(len(set(keys)) == len(keys) and all(k in self.by_capture for k in keys), "Scenario command captures are missing or duplicated")
        rows = [self.by_capture[k] for k in keys]
        self.q.require(all(r["artifact_id"] in case["artifact_ids"] for r in rows), "Scenario does not reference its original capture artifact")
        return rows


def peak(q, artifacts, evidence, targets, journal):
    declaration = evidence["peak"]
    q.require(declaration["duration_seconds"] == targets["duration_seconds"] and declaration["concurrent_operators"] == targets["concurrent_operators"], "Measured peak scope differs from the predeclared plan")
    duration = q.number(targets["duration_seconds"], "planned peak duration", 300)
    operators = q.integer(targets["concurrent_operators"], "planned concurrent operators", 1)
    rate = q.number(targets["required_actions_per_second"], "planned actions per second", Decimal("0.000001"))
    q.require(duration <= 86400 and operators <= 256, "Peak interval/operator population exceeds the verification profile")
    maximum = q.number(targets["max_p95_ms"], "planned p95", 1)
    q.require(instant(q, evidence["started_at"]) <= instant(q, declaration["started_at"]) and instant(q, declaration["started_at"]) + Decimal(duration) <= instant(q, evidence["ended_at"]), "Peak timing anchor falls outside the actual facility run")
    rows, keys, actors = [], set(), set()
    for key, raw in artifacts.data.items():
        if artifacts.entries[key]["kind"] != "peak_samples": continue
        # Bounded NDJSON keeps the full timing series without a huge JSON tree.
        for line in raw.splitlines():
            q.require(line and len(line) <= 8192, "Peak sample line is empty or oversized")
            row = q.decode(line); capture = journal.by_capture[row["capture_id"]]
            q.require(row["capture_id"] not in keys and capture["http_status"] == 200 and capture["body"]["replayed"] is False and capture["phase"] == "performed",
                      "Peak samples repeat a request or count a replay/failed action")
            q.require(capture["warehouse_id"] == evidence["warehouse_id"] and capture["tenant"] == evidence["wms_tenant"], "Peak sample belongs to a different warehouse or tenant")
            start = q.number(row["start_elapsed_ms"], "sample start")
            latency = q.number(row["duration_ms"], "sample save/refresh duration", Decimal("0.000001"))
            q.require(start + latency <= duration * 1000, "Peak sample extends outside the declared continuous interval")
            commit = instant(q, capture["event"]["occurred_at"])
            anchor = instant(q, declaration["started_at"])
            q.require(anchor + Decimal(start) / 1000 - 5 <= commit <= anchor + Decimal(start + latency) / 1000 + 5, "Peak samples borrow original commits from outside their timing interval")
            keys.add(row["capture_id"]); actors.add(capture["actor"]); rows.append((start, start + latency, latency, capture["actor"]))
    q.require(rows and len(actors) == operators, "Actual peak operator population differs from the plan")
    q.require(Decimal(len(rows)) / Decimal(duration) >= rate, "Recomputed confirmed throughput misses the predeclared target")
    # Each actor must contribute throughout the interval; separate sequential
    # five-second demonstrations cannot establish sustained declared concurrency.
    for actor in actors:
        intervals = [(a, b) for a, b, _, who in rows if who == actor]
        q.require(min(a for a, _ in intervals) <= 10000 and max(b for _, b in intervals) >= (duration - 10) * 1000,
                  "A peak operator did not participate across the complete interval")
        segments = sorted((int(a // 10000), min(math.ceil(duration / 10) - 1, int(b // 10000))) for a, b in intervals)
        covered = -1
        for first, last in segments:
            q.require(first <= covered + 1, "Peak operator work has an uncovered ten-second interval")
            covered = max(covered, last)
        q.require(covered == math.ceil(duration / 10) - 1, "Peak operator work ends before the declared interval")
    edges = sorted([(start, 1, actor) for start, _, _, actor in rows] + [(end, -1, actor) for _, end, _, actor in rows])
    active, maximum_active = defaultdict(int), 0
    for _, delta, actor in edges:
        active[actor] += delta
        maximum_active = max(maximum_active, sum(count > 0 for count in active.values()))
    q.require(maximum_active >= operators, "Timing captures never show the declared distinct operators concurrently active")
    latencies = sorted(x[2] for x in rows); p95 = latencies[math.ceil(len(rows) * Decimal("0.95")) - 1]
    observed_rate = Decimal(len(rows)) / Decimal(duration)
    q.require(p95 <= maximum and q.number(declaration["p95_ms"], "reported p95") == p95
              and abs(q.number(declaration["observed_actions_per_second"], "reported rate") - observed_rate) <= Decimal("0.000001"),
              "Reported peak summary differs from samples or misses its declared latency target")
    return {"samples": len(rows), "operators": len(actors), "duration_seconds": duration, "p95_ms": p95}


def scan_and_inventory(q, evidence, journal):
    scans = sequence(q, evidence["scans"], 10000)
    matched = {"receive": [], "pick": []}; rejected = 0
    creators = {}
    for r in journal.originals.values():
        if r["request"]["command"]["type"] in {"create_receipt", "create_return", "create_order"}:
            creators[(r["warehouse_id"], r["body"]["result"]["entity_id"])] = r["request"]["command"]
    for row in scans:
        q.require(row["operation"] in matched and type(row["accepted"]) is bool, "Invalid physical scan operation/outcome")
        for field in ("line_id", "owner_id", "bin_label", "sku_label", "lot_label"):
            q.text(row[field], "literal scan identity")
        q.integer(row["quantity_each"], "scanned each quantity", 1)
        expected = row["expected"]
        for field in ("bin_label", "sku_label", "lot_label"): q.text(expected[field], "reviewed label identity")
        equal = all(expected[field] == row[field] for field in ("bin_label", "sku_label", "lot_label"))
        q.require(row["accepted"] == equal, "Physical scan outcome contradicts the literal expected labels")
        reference = journal.by_capture[row["review_capture_id"]]
        command = reference["request"]["command"]
        q.require(command["type"] == row["operation"] and reference["warehouse_id"] == evidence["warehouse_id"], "Scan review belongs to different work/facility")
        original = creators[(evidence["warehouse_id"], row["owner_id"])]
        line = next((l for l in original["lines"] if l["id"] == row["line_id"]), None)
        q.require(line is not None and command["line_id"] == row["line_id"] and command["location"] == expected["bin_label"], "Literal scan review does not belong to its original line/bin")
        if original["type"] == "create_return":
            order = creators[(evidence["warehouse_id"], original["order_id"])]
            original_line = next((l for l in order["lines"] if l["id"] == line["order_line_id"]), None)
            q.require(original_line is not None, "Return scan lacks its original shipped order line")
            expected_sku = original_line["sku"]
        else: expected_sku = line["sku"]
        q.require(expected["sku_label"] == expected_sku and expected["lot_label"] == (line["lot"] if row["operation"] == "receive" else command["lot"]), "Literal expected SKU/lot borrows another original line")
        if row["accepted"]:
            record = journal.by_capture[row["capture_id"]]; c = record["request"]["command"]
            q.require(record["http_status"] == 200 and record["phase"] == "performed" and record["body"]["replayed"] is False and record["warehouse_id"] == evidence["warehouse_id"] and c["type"] == row["operation"]
                      and c["line_id"] == row["line_id"] and c["quantity"] == row["quantity_each"] and c["location"] == row["bin_label"],
                      "Literal accepted scan does not match its committed command")
            owner = c["receipt_id"] if c["type"] == "receive" else c["order_id"]
            q.require(owner == row["owner_id"], "Scan belongs to a different receipt/order")
            if c["type"] == "pick": q.require(c["lot"] == row["lot_label"], "Pick scan names a different original lot")
            matched[row["operation"]].append(row)
        else:
            q.require(row.get("capture_id") is None and row.get("physical_effects") == 0 and type(row.get("physical_effects")) is int,
                      "A rejected label scan claims a command or physical effect")
            rejected += 1
    for rows in matched.values():
        q.require(any(a["owner_id"] == b["owner_id"] and a["sku_label"] == b["sku_label"] and a["line_id"] != b["line_id"] for a in rows for b in rows),
                  "Same-SKU independent lines in one original receipt/order were not physically scanned")
    q.require(rejected, "No literal wrong-label rejection was captured")
    seen = set()
    for row in sequence(q, evidence["inventory"], 10000):
        identity = tuple(q.text(row[k], "physical custody identity") for k in ("sku", "lot", "location"))
        q.require(identity not in seen, "Duplicate physical inventory custody identity")
        seen.add(identity)
        values = {k: q.integer(row[k], k) for k in ("start_physical", "received", "moved_in", "moved_out", "shipped", "transfer_in", "transfer_out", "count_gain", "count_loss", "end_physical", "end_system")}
        expected = values["start_physical"] + values["received"] + values["moved_in"] - values["moved_out"] - values["shipped"] + values["transfer_in"] - values["transfer_out"] + values["count_gain"] - values["count_loss"]
        q.require(expected == values["end_physical"] == values["end_system"], "Physical custody equation or final system balance differs")
    return len(scans), len(seen)


def unit_sources(q, journal, evidence=None):
    definitions, observed, disabled = {}, 0, 0
    performed_observed, performed_disabled, performed_change = 0, 0, False
    originals = sorted(journal.originals.values(), key=lambda r: (r["warehouse_id"], r["event"]["version"]))
    for row in originals:
        c, e = row["request"]["command"], row["event"]
        if c["type"] == "set_unit_pack":
            p = e["unit_pack_change"]
            q.require(p["id"] == e["result"]["entity_id"] and p["source_event_id"] == e["id"] and p["version"] == e["version"]
                      and p["warehouse_id"] == row["warehouse_id"] and p["actor"] == row["actor"]
                      and all(p[k] == c[k] for k in ("sku", "code", "label", "each_per_pack", "enabled", "reason")), "Original pack definition source differs")
            q.require(type(p["enabled"]) is bool and 1 <= q.integer(p["each_per_pack"], "pack factor", 1) <= 1000000
                      and re.fullmatch(r"[A-Z0-9_-]{1,32}", p["code"]) and p["code"] != "EA", "Invalid reviewed packaging definition")
            key = (row["warehouse_id"], p["sku"], p["code"])
            history = definitions.setdefault(key, [])
            if evidence is not None and row["phase"] == "performed" and row["warehouse_id"] == evidence["warehouse_id"]:
                performed_disabled += not p["enabled"]
                performed_change |= bool(history and history[-1]["each_per_pack"] != p["each_per_pack"])
            history.append(p)
            disabled += not p["enabled"]
        seen = set()
        for i in row["request"].get("unit_inputs", []):
            target = tuple(i["target"])
            q.require(target not in seen and len(target) <= 3, "Duplicate or invalid original conversion target")
            seen.add(target)
            history = definitions[(row["warehouse_id"], i["sku"], i["pack_code"])]
            p = history[-1]
            q.require(p["enabled"] and p["id"] == i["definition_id"] and p["each_per_pack"] == i["each_per_pack"] and p["version"] < e["version"],
                      "Conversion did not use the original definition current at its command")
            quantity = q.integer(i["pack_quantity"], "observed whole pack count") * p["each_per_pack"]
            q.require(quantity <= 1000000, "Original conversion exceeds the exact each range")
            if c["type"] == "count_stock": q.require(not target and c["sku"] == i["sku"] and c["counted"] == quantity, "Count conversion target/each differs")
            elif c["type"] == "move_stock": q.require(not target and c["sku"] == i["sku"] and c["quantity"] == quantity, "Move conversion target/each differs")
            elif c["type"] in {"create_receipt", "create_order", "create_return", "dispatch_transfer", "pack_shipment"}:
                candidates = [l for l in c["lines"] if tuple([l["line_id"], l["location"], l["lot"]] if c["type"] == "pack_shipment" else [l["id"]]) == target]
                q.require(len(candidates) == 1 and candidates[0]["quantity"] == quantity, "Original line/tuple conversion differs")
                if "sku" in candidates[0]: q.require(candidates[0]["sku"] == i["sku"], "Original conversion borrows another SKU")
            else:
                q.require(c["type"] in {"receive", "pick", "unpick", "receive_transfer", "record_transfer_loss", "reverse_transfer_loss"}
                          and not target and c["quantity"] == quantity, "Unsupported original quantity conversion target")
            observed += 1
            if evidence is not None and row["phase"] == "performed" and row["warehouse_id"] == evidence["warehouse_id"]: performed_observed += 1
    q.require(observed >= 3 and disabled and any(len({p["each_per_pack"] for p in history}) > 1 for history in definitions.values()),
              "Pack observations lack an actual size change, disabled code or original conversion history")
    if evidence is not None:
        q.require(performed_observed >= 3 and performed_disabled and performed_change, "Reviewed pack trial borrows baseline or counterpart observations")
        current, changed_replay = {}, False
        for row in journal.records:
            if row["http_status"] != 200 or row["warehouse_id"] != evidence["warehouse_id"]: continue
            c = row["request"]["command"]
            if not row["body"]["replayed"] and c["type"] == "set_unit_pack": current[(c["sku"], c["code"])] = row["event"]["unit_pack_change"]
            if row["body"]["replayed"] and row["phase"] == "performed":
                for item in row["request"].get("unit_inputs", []):
                    latest = current.get((item["sku"], item["pack_code"]))
                    changed_replay |= bool(latest and latest["id"] != item["definition_id"] and latest["each_per_pack"] != item["each_per_pack"])
        q.require(changed_replay, "Reviewed packs lack an exact original conversion replay after a size change")
    return observed


def workflow_sources(q, journal):
    losses, packages, dispatches, arrivals = {}, {}, {}, defaultdict(int)
    counts = defaultdict(int)
    # Record order follows the captured command sequence; per-warehouse event
    # versions are independently unique, and cross-facility dispatch must have
    # been observed before its destination arrival/loss.
    for row in journal.records:
        if row["http_status"] != 200 or row["body"]["replayed"]: continue
        c, e = row["request"]["command"], row["event"]; kind = c["type"]; counts[kind] += 1
        if kind in {"set_unit_pack", "set_product_expiry_policy", "record_lot_expiry", "correct_lot_expiry", "quarantine_lot", "release_lot_hold", "record_transfer_loss", "reverse_transfer_loss"}:
            q.require(e["inventory_changes"] == [], "Metadata or loss/reversal command changed physical/reserved stock")
        if kind == "dispatch_transfer":
            t = e["transfer"]
            q.require(t["id"] == e["result"]["entity_id"] and t["source_warehouse_id"] == row["warehouse_id"]
                      and t["destination_warehouse_id"] == c["destination_warehouse_id"] and t["source_event_id"] == e["id"]
                      and t["carrier"] == c["carrier"] and t["handoff_reference"] == c["handoff_reference"], "Original facility handoff ownership differs")
            actual = {l["id"]: l for l in t["lines"]}
            q.require(len(actual) == len(c["lines"]) and t["id"] not in dispatches, "Transfer duplicates or changes original lines")
            for line in c["lines"]:
                q.require(all(actual[line["id"]][k] == line[k] for k in ("sku", "lot", "location", "quantity")), "Original transfer line differs")
            dispatches[t["id"]] = t
        elif kind in {"record_transfer_loss", "reverse_transfer_loss"}:
            r = e["transfer_loss_record"]
            q.require(r["id"] == e["result"]["entity_id"] and r["event_id"] == e["id"] and r["warehouse_id"] == row["warehouse_id"]
                      and r["quantity"] == c["quantity"] and r["actor"] == row["actor"] and r["reason"] == c["reason"]
                      and r["version"] == e["version"], "Original loss/reversal source differs")
            q.integer(r["quantity"], "original loss/reversal quantity", 1)
            if kind == "record_transfer_loss":
                q.require(r["kind"] == "loss" and r["original_loss_id"] is None and r["transfer_id"] == c["transfer_id"] and r["line_id"] == c["line_id"]
                          and r["incident_reference"] == c["incident_reference"], "Loss borrows another line or incident")
                t = dispatches[r["transfer_id"]]
                q.require(row["warehouse_id"] == t["destination_warehouse_id"] and row["role"] == "admin", "Loss authority does not belong to destination administration")
                losses[r["id"]] = dict(r, reversed=0)
            else:
                original = losses[c["loss_id"]]
                q.require(r["kind"] == "reversal" and r["original_loss_id"] == c["loss_id"] and r["transfer_id"] == original["transfer_id"]
                          and r["line_id"] == original["line_id"] and r["warehouse_id"] == original["warehouse_id"] and row["role"] == "admin",
                          "Reversal borrows another original loss, line or authority")
                original["reversed"] += r["quantity"]
                q.require(original["reversed"] <= original["quantity"], "Reversal exceeds its original unreversed loss")
        elif kind == "receive_transfer":
            t = dispatches[c["transfer_id"]]
            q.require(row["warehouse_id"] == t["destination_warehouse_id"], "Arrival credits the wrong facility")
            arrivals[(t["id"], c["line_id"])] += q.integer(c["quantity"], "actual arrival", 1)
        if kind in {"record_transfer_loss", "reverse_transfer_loss", "receive_transfer"}:
            tid = r["transfer_id"] if kind != "receive_transfer" else c["transfer_id"]
            for line in dispatches[tid]["lines"]:
                lost = sum(l["quantity"] - l["reversed"] for l in losses.values() if l["transfer_id"] == tid and l["line_id"] == line["id"])
                q.require(arrivals[(tid, line["id"])] + lost <= line["quantity"], "Transfer arrivals/net loss exceed original dispatched custody")
        if kind in {"pack_shipment", "unpack_shipment", "handoff_shipment"}:
            p = e["shipment"]
            q.require(p["id"] == e["result"]["entity_id"], "Shipment event names a different package")
            if kind == "pack_shipment":
                q.require(p["source_event_id"] == e["id"] and p["source_version"] == e["version"] and p["closure"] is None
                          and p["id"] not in packages and p["order_id"] == c["order_id"] and p["reference"] == c["reference"], "Package original packing source differs")
                packed = {(l["line_id"], l["location"], l["lot"]): l for l in p["lines"]}
                q.require(len(packed) == len(c["lines"]), "Package tuple source is repeated")
                for l in c["lines"]: q.require(packed[(l["line_id"], l["location"], l["lot"])]["quantity"] == l["quantity"], "Package borrows another allocation quantity")
                packages[p["id"]] = p
            else:
                old = packages[c["shipment_id"]]
                q.require(all(p[k] == old[k] for k in ("id", "order_id", "reference", "actor", "packed_at", "source_event_id", "source_version", "lines"))
                          and old["closure"] is None and p["closure"]["event_id"] == e["id"] and p["closure"]["actor"] == row["actor"], "Closure rewrites original package facts or repeats its effect")
                closure = p["closure"]
                q.require(closure["kind"] == ("handed_off" if kind == "handoff_shipment" else "unpacked"), "Original package closure kind differs")
                if kind == "handoff_shipment": q.require(closure["carrier"] == c["carrier"] and closure["handoff_reference"] == c["handoff_reference"], "Original carrier handoff differs")
                packages[p["id"]] = p
        if kind == "cancel_receipt":
            receipt = e["receipt"]
            q.require(receipt["id"] == c["receipt_id"] and receipt["status"] == "cancelled" and receipt["cancellation"]["actor"] == row["actor"], "Receipt cancellation original source differs")
            for l in receipt["lines"]:
                x = receipt["cancellation"]["lines"][l["id"]]
                q.require(x["quantity"] == l["quantity"] and x["received"] == l["received"] and x["cancelled"] == l["quantity"] - l["received"], "Receipt cancellation invents a physical return or rewrites original demand")
        if kind == "cancel_remainder":
            before, after = e["previous_order"], e["order"]
            q.require(before["id"] == after["id"] == c["order_id"] and before["lines"] == after["lines"] and after["status"] == "closed_partial", "Order closure rewrites original ordered demand")
            q.require(sum(a["shipped"] for a in before["allocations"]) == sum(a["shipped"] for a in after["allocations"]), "Remainder closure changes actual shipped custody")
            for line in after["lines"]:
                x = after["remainder_cancellation"]["lines"][line["id"]]
                shipped = sum(a["shipped"] for a in after["allocations"] if a["line_id"] == line["id"])
                q.require(x["ordered"] == line["quantity"] and x["shipped"] == shipped and x["cancelled"] == line["quantity"] - shipped, "Cancelled remainder differs from original unshipped demand")
    return counts


def trace_sources(q, artifacts, evidence=None):
    captures = artifacts.objects("trace_pages")
    q.require(captures, "Cross-facility trace page captures are missing")
    for _, document in captures:
        q.require(same_json(document["before_custody"], document["after_custody"]), "Trace reads changed captured physical/reserved custody")
        custody, physical, reserved = set(), 0, 0
        for row in sequence(q, document["before_custody"], 10000):
            identity = tuple(q.text(row[k], "trace custody identity") for k in ("warehouse_id", "sku", "lot", "location"))
            q.require(identity not in custody, "Trace custody identity is repeated")
            custody.add(identity)
            on_hand = q.integer(row["on_hand"], "trace physical each")
            held = q.integer(row["reserved"], "trace reserved each")
            q.require(held <= on_hand, "Trace reservations exceed custody")
            physical += on_hand; reserved += held
        pages = sequence(q, document["pages"], 1000)
        first = pages[0]["response"]; review = digest_text(q, first["review"])
        q.require(q.integer(first["summary"]["physical"], "trace total physical") == physical and q.integer(first["summary"]["reserved"], "trace total reserved") == reserved
                  and all(row["sku"] == first["sku"] and row["lot"] == first["lot"] for row in document["before_custody"]), "Trace custody totals or original labels differ")
        if evidence is not None:
            q.require(instant(q, evidence["started_at"]) <= instant(q, document["checked_at"]) <= instant(q, evidence["ended_at"]), "Trace observation is outside the performed run")
        totals, cursors, keys, finished = defaultdict(int), {}, set(), set()
        for capture in pages:
            query, value = capture["query"], capture["response"]
            if evidence is not None:
                q.require(instant(q, evidence["started_at"]) <= instant(q, value["checked_at"]) <= instant(q, evidence["ended_at"]), "Trace source page was observed outside the performed run")
            section = value["section"]
            q.require(section in TRACE_SECTIONS and section not in finished and value["review"] == review
                      and query["sku"] == value["sku"] == first["sku"] and query["lot"] == value["lot"] == first["lot"]
                      and query.get("section", "positions") == section and value["summary"] == first["summary"]
                      and value["page_source_ok"] is True and value["summary"]["physical_reconciliation_ok"] is True,
                      "Trace pages mix labels, reviews, section scope or failed sources")
            q.require(query.get("after") == cursors.get(section) and (query.get("review") == review if section in cursors else query.get("review") in {None, review}),
                      "Trace page did not retain its original review/cursor")
            for item in sequence(q, value["items"], 100, nonempty=False):
                encoded = (section, q.text(item["warehouse_id"], "trace source facility"), json.dumps(item["key"], sort_keys=True))
                q.require(encoded not in keys and item["source_ok"] is True, "Trace page repeats a source or retains a failed source")
                keys.add(encoded); totals[section] += 1
            cursor = value["next_cursor"]
            q.require(cursor is None or isinstance(cursor, str) and cursor and cursor != cursors.get(section), "Trace cursor did not advance")
            cursors[section] = cursor
            if cursor is None: finished.add(section)
        q.require(finished == TRACE_SECTIONS and all(totals[s] == q.integer(first["summary"]["sections"][s], "trace section records", 1) for s in TRACE_SECTIONS),
                  "Trace section evidence is incomplete or silently truncated")
    return len(captures)


def native_sources(q, artifacts, evidence):
    states, completed = defaultdict(set), {}
    for _, document in artifacts.objects("native_receipts"):
        for row in sequence(q, document["records"], 10000):
            value = row["response"]
            q.require(row["tenant"] == evidence["wms_tenant"] and row["warehouse_id"] == evidence["warehouse_id"]
                      and value["found"] is True and value["request_id"] == row["request_id"]
                      and value["kind"] in {"oms_fulfillment", "tms_transport"}
                      and value["status"] in {"completed", "rejected", "uncertain"} and type(value["replayed"]) is bool,
                      "Native receipt scope, kind or result is invalid")
            q.integer(value["attempts"], "native attempts", 1)
            states[value["kind"]].add(value["status"])
            identity = (value["kind"], value["request_id"])
            if value["status"] == "completed":
                q.require(value["response"] is not None, "Completed native request lacks its original response")
                if identity in completed:
                    old = completed[identity]
                    q.require(value["replayed"] and same_json(value["response"], old["response"]) and same_json(value["evidence"], old["evidence"])
                              and value["attempts"] == old["attempts"], "Completed native retry changes its original effect or frozen intent")
                    states[value["kind"]].add("exact_completed_replay")
                else:
                    q.require(not value["replayed"], "Native replay lacks its captured original completed result")
                    completed[identity] = value
    q.require({"completed", "rejected", "uncertain", "exact_completed_replay"} <= states["oms_fulfillment"]
              and {"completed", "exact_completed_replay"} <= states["tms_transport"], "Native original/rejected/uncertain/replay receipts are incomplete")
    return {kind: sorted(values) for kind, values in states.items()}


def reconcile(q, artifacts, evidence, plan, journal=None):
    identities = [tuple(q.text(row[k], "planned custody identity") for k in ("sku", "lot", "location")) for row in sequence(q, plan["inventory_scope"], 10000)]
    q.require(len(set(identities)) == len(identities), "Predeclared custody scope contains duplicate identities")
    physical = {tuple(row[k] for k in ("sku", "lot", "location")): row for row in evidence["inventory"]}
    q.require(set(physical) == set(identities), "Physical observations omit or extend the predeclared custody scope")
    documents = artifacts.objects("full_reconciliation")
    q.require(len(documents) == 1, "Exactly one captured final full reconciliation is required")
    _, captured = documents[0]; value = captured["response"]
    q.require(captured["warehouse_id"] == evidence["warehouse_id"] and captured["tenant"] == evidence["wms_tenant"] and value["ok"] is True,
              "Final reconciliation belongs to another scope or did not pass")
    q.integer(value["version"], "final reconciliation version", 1)
    if journal is not None:
        q.require(value["version"] >= max(r["event"]["version"] for r in journal.originals.values() if r["warehouse_id"] == evidence["warehouse_id"]), "Final reconciliation predates captured warehouse work")
    rows = {}
    for row in sequence(q, value["rows"], 100000, nonempty=False):
        key = tuple(row[k] for k in ("sku", "lot", "location"))
        q.require(key not in rows and row["ok"] is True and q.integer(row["on_hand"], "final physical each") == q.integer(row["ledger"], "append-only each")
                  and q.integer(row["reserved"], "final reserved each") == q.integer(row["allocation_reserved"], "original allocation each"), "Final source ledger/reservation proof differs")
        rows[key] = row
    for identity in identities:
        q.require(physical[identity]["end_system"] == rows.get(identity, {"on_hand": 0})["on_hand"], "Signed physical count disagrees with captured final reconciliation")
    for section in ("receipts", "shipments", "packages", "transfers"):
        q.require(isinstance(value[section], list) and all(row["ok"] is True for row in value[section]), "A final receipt/shipment/package/transfer source proof failed")
    q.require(value["lot_controls"]["ok"] is True and value["units"]["ok"] is True, "Final lot or unit original-source proof failed")
    return len(identities), value["version"]



def custody_deltas(q, evidence, journal):
    """Rebuild each declared custody column from original committed stock changes."""
    declared = {tuple(r[k] for k in ("sku", "lot", "location")): r for r in evidence["inventory"]}
    balances = {key: q.integer(row["start_physical"], "opening controlled physical each") for key, row in declared.items()}
    columns = {key: defaultdict(int) for key in declared}
    for row in journal.records:
        if row["http_status"] != 200 or row["body"]["replayed"] or row["phase"] != "performed" or row["warehouse_id"] != evidence["warehouse_id"]: continue
        command = row["request"]["command"]
        touched = set()
        for change in row["event"]["inventory_changes"]:
            stock = change["stock"]; key = tuple(stock[k] for k in ("sku", "lot", "location"))
            q.require(key in balances and key not in touched, "Committed command changes undeclared/repeated controlled custody")
            touched.add(key)
            after = q.integer(stock["on_hand"], "committed physical each")
            q.require(0 <= q.integer(stock["reserved"], "committed reservation") <= after, "Captured reserved stock exceeds actual custody")
            delta = after - balances[key]; balances[key] = after
            kind = command["type"]
            if not delta: continue
            if kind == "count_stock": label = "count_gain" if delta > 0 else "count_loss"
            elif kind == "receive":
                q.require(delta > 0 and stock["location"] == command["location"] and delta == command["quantity"], "Receiving stock delta differs from the original action")
                label = "received"
            elif kind in {"move_stock", "pick", "unpick"}: label = "moved_in" if delta > 0 else "moved_out"
            elif kind in {"ship", "handoff_shipment"}:
                q.require(delta < 0 and stock["location"] == "STAGE-01", "Handoff credits stock or removes it outside staging")
                label = "shipped"
            elif kind == "dispatch_transfer":
                q.require(delta < 0, "Transfer dispatch credits source custody")
                label = "transfer_out"
            elif kind == "receive_transfer":
                q.require(delta > 0 and delta == command["quantity"] and stock["location"] == command["location"], "Transfer arrival differs from actual receiving")
                label = "transfer_in"
            else: q.require(False, "Command type changed physical custody without an explicit physical effect")
            columns[key][label] += abs(delta)
    for key, row in declared.items():
        q.require(balances[key] == row["end_system"] and all(row[label] == columns[key][label] for label in ("received", "moved_in", "moved_out", "shipped", "transfer_in", "transfer_out", "count_gain", "count_loss")),
                  "Witness custody columns omit or contradict captured original physical effects")
    return sum(sum(values.values()) for values in columns.values())

def recovery(q, artifacts, targets, binding):
    items = artifacts.objects("recovery"); q.require(len(items) == 1, "One original archive/restore observation is required")
    _, item = items[0]
    q.require(item["release"] == binding and item["post_backup_effects_reviewed"] is True,
              "Restore scope or original post-backup effect review differs")
    backup = instant(q, item["backup_at"]); incident = instant(q, item["incident_at"]); ready = instant(q, item["ready_at"])
    durable, restored = instant(q, item["last_durable_effect_at"]), instant(q, item["last_restored_effect_at"])
    q.require(backup <= incident <= ready and restored <= durable <= incident and item["source_table_hashes"] == item["restored_table_hashes"],
              "Restore timestamps, effect cutoff or original table fingerprints disagree")
    for key, value in item["source_table_hashes"].items(): q.text(key, "restored table identity"); digest_text(q, value)
    q.require(digest(json.dumps(sorted(item["source_table_hashes"]), separators=(",", ":")).encode()) == binding["schema_tables_sha256"] and item["reconciliation"]["ok"] is True
              and item["reconciliation"]["lot_controls"]["ok"] is True and item["reconciliation"]["units"]["ok"] is True,
              "Restored current-schema tables and source proofs are incomplete")
    rto = instant(q, item["ready_at"]) - instant(q, item["incident_at"]); rpo = instant(q, item["last_durable_effect_at"]) - instant(q, item["last_restored_effect_at"])
    q.require(rto <= q.number(targets["max_rto_seconds"], "predeclared RTO", 1)
              and rpo <= q.number(targets["max_rpo_seconds"], "predeclared RPO"), "Recomputed recovery misses a predeclared target")
    q.require(item["archive_artifact_id"] in artifacts.entries and artifacts.entries[item["archive_artifact_id"]]["kind"] == "archive", "Original restore archive bytes are missing")
    return {"actual_rto_seconds": rto, "actual_rpo_seconds": rpo, "tables": len(item["source_table_hashes"])}



def device_sources(q, artifacts, evidence, journal, plan):
    observations = []
    for _, document in artifacts.objects("device_observations"):
        observations.extend(sequence(q, document["observations"], 10000))
    pending_kinds, stale, repl = set(), 0, 0
    for item in observations:
        kind = item["kind"]
        if kind == "pending_facility_switch":
            q.require(item["pending_kind"] in {"warehouse", "native"} and item["pending_save_state"] == "uncertain"
                      and item["switch_enabled"] is False and item["warehouse_metadata"]["id"] == item["displayed_warehouse_id"] == evidence["warehouse_id"]
                      and item["displayed_code"] == item["warehouse_metadata"]["code"] and item["displayed_city"] == item["warehouse_metadata"]["city"]
                      and item["attempted_warehouse_id"] in plan["counterpart_warehouse_ids"], "Pending save or displayed facility switch observation differs")
            q.text(item["pending_request_id"], "retained pending request")
            if item["pending_kind"] == "warehouse":
                q.require(any(r["request"]["request_id"] == item["pending_request_id"] for r in journal.records), "Pending warehouse save lacks its original capture")
            capture = item["device_capture_artifact_id"]
            q.require(capture in artifacts.entries and artifacts.entries[capture]["kind"] == "device_capture", "Original hardware/display capture is missing")
            pending_kinds.add(item["pending_kind"])
        elif kind == "stale_rescan":
            old, new = journal.by_capture[item["denied_capture_id"]], journal.by_capture[item["fresh_capture_id"]]
            q.require(old["http_status"] == 409 and new["http_status"] == 200 and old["actor"] == new["actor"]
                      and old["warehouse_id"] == new["warehouse_id"] == evidence["warehouse_id"]
                      and old["request"]["request_id"] != new["request"]["request_id"], "Stale save did not require a distinct reviewed fresh command")
            outcome = old["body"]["command_outcome"]
            q.require(outcome["status"] == "not_committed" and outcome["tenant"] == old["tenant"] and outcome["actor"] == old["actor"]
                      and outcome["warehouse_id"] == old["warehouse_id"] and outcome["request_id"] == old["request"]["request_id"]
                      and outcome["expected_version"] == old["request"]["expected_version"]
                      and new["request"]["expected_version"] >= outcome["current_version"], "Definitive stale outcome does not bind the original review")
            if outcome["reason"] == "changed_resources":
                q.require(outcome["expected_resource_versions"] == old["request"]["resource_versions"]
                          and set(outcome["current_resource_versions"]) == set(outcome["expected_resource_versions"])
                          and outcome["current_resource_versions"] != outcome["expected_resource_versions"], "Resource-stale outcome has no original changed dependencies")
            else:
                q.require(outcome["reason"] == "stale_version" and outcome["current_version"] > outcome["expected_version"], "Stale-version outcome has no warehouse advance")
            q.require(any(s.get("capture_id") == item["fresh_capture_id"] and s["accepted"] is True for s in evidence["scans"]), "Fresh physical work lacks a new literal scan")
            stale += 1
        elif kind == "replenishment_move":
            row = journal.by_capture[item["move_capture_id"]]; proposed = item["plan"]
            matches = [t for t in proposed["tasks"] if t["command"] == row["request"]["command"]]
            q.require(row["http_status"] == 200 and row["request"]["command"]["type"] == "move_stock"
                      and proposed["warehouse_id"] == row["warehouse_id"] == evidence["warehouse_id"]
                      and proposed["version"] == row["request"]["expected_version"] and len(matches) == 1
                      and matches[0]["quantity"] == row["request"]["command"]["quantity"]
                      and matches[0].get("resource_versions") == row["request"].get("resource_versions"), "Actual replenishment move differs from its reviewed plan/dependencies")
            repl += 1
        else: q.require(False, "Unsupported device/workbench observation")
    q.require(pending_kinds == {"warehouse", "native"} and stale and repl, "Facility pending-save, stale rescan or actual replenishment observations are incomplete")
    lost = evidence["lost_response"]
    original, replay = journal.by_capture[lost["original_capture_id"]], journal.by_capture[lost["retry_capture_id"]]
    q.require(original["http_status"] == replay["http_status"] == 200 and original["body"]["replayed"] is False
              and replay["body"]["replayed"] is True and original["request"] == replay["request"]
              and original["actor"] == replay["actor"] and original["event"] == replay["event"]
              and original["warehouse_id"] == replay["warehouse_id"] == evidence["warehouse_id"]
              and original["request"]["command"]["type"] in {"receive", "pick", "move_stock"}
              and original["event"]["inventory_changes"] and q.integer(lost["physical_effects"], "witnessed original physical effects") == 1,
              "Lost-response evidence does not retain one exact original physical effect")
    return {"pending_save_kinds": sorted(pending_kinds), "stale_rescans": stale, "planned_moves": repl}

def run(args, q):
    report = q.Report("signed_current_facility_evidence")
    report.data.update(schema_version=2, profile=PROFILE)
    report.data["not_performed"] = ["Independent observation of physical work, signer identity or deployed orchestrator state",
                                    "Live external native mutation or carrier-claim verification",
                                    "Unobserved custody outside the predeclared controlled acceptance scope"]
    def verify():
        q.require(all(getattr(args, name, None) for name in ("qualified_release", "expected_release_summary_sha256", "expected_source_inputs_sha256", "expected_image_digest", "plan", "plan_signature", "plan_trust_key", "controller_signature", "controller_trust_key")),
                  "Current profile requires independently pinned release, predeclared signed plan and controller signature/key")
        binding = release(q, args)
        plan, plan_key, plan_digest = q.signed_file(args.plan, args.plan_signature, args.plan_trust_key, versions=(2,))
        evidence, operator_key, evidence_digest = q.signed_file(args.evidence, args.signature, args.trust_key, versions=(2,))
        second, controller_key, second_digest = q.signed_file(args.evidence, args.controller_signature, args.controller_trust_key, versions=(2,))
        q.require(second_digest == evidence_digest and second == evidence and operator_key != controller_key, "Independent operator/controller signatures must authenticate the same exact evidence bytes")
        expected = {"facility_id": args.expected_facility, "wms_tenant": args.expected_tenant, "warehouse_id": args.expected_warehouse}
        scoped(q, evidence, expected); scoped(q, plan, expected)
        q.require(evidence.get("performed") is True and evidence.get("synthetic") is False and plan.get("profile") == evidence.get("profile") == PROFILE,
                  "Current facility evidence has not been performed or has the wrong profile")
        q.require(plan["release"] == evidence["release"] == binding and evidence["plan_sha256"] == plan_digest,
                  "Facility run changes the predeclared release or plan")
        q.recent(evidence["observed_at"], hours=168)
        start, end, observed = instant(q, evidence["started_at"]), instant(q, evidence["ended_at"]), instant(q, evidence["observed_at"])
        q.require(instant(q, binding["qualified_at"]) <= instant(q, plan["approved_at"]) < start < end <= observed and end - start >= 300, "Test plan must precede the complete actual run")
        witnesses = sequence(q, evidence["witnesses"], 2)
        q.require(len(witnesses) == 2 and {w["role"] for w in witnesses} == {"operator", "inventory_controller"}
                  and len({q.text(w["name"], "witness identity") for w in witnesses}) == 2, "Independent named operator/controller witnesses are required")
        for w in witnesses: q.require(w["key_sha256"] == (operator_key if w["role"] == "operator" else controller_key), "Witness role does not bind its independently trusted signing key")
        scanner = evidence["scanner"]
        q.require(scanner["physical_hardware"] is True and scanner["printed_labels"] is True, "Actual hardware and printed-label observations are missing")
        for key in ("make_model", "device_id", "firmware", "input_mode", "symbology", "label_stock", "printer_model"): q.text(scanner[key], "physical scanner fact")
        cases = sequence(q, evidence["scenarios"], len(CURRENT_SCENARIOS))
        q.require(len(cases) == len(CURRENT_SCENARIOS) and {c["name"] for c in cases} == CURRENT_SCENARIOS
                  and len(plan["scenarios"]) == len(CURRENT_SCENARIOS) and set(plan["scenarios"]) == CURRENT_SCENARIOS,
                  "Current facility workflow matrix is incomplete or duplicated")
        artifacts = Artifacts(q, args.artifact_root, evidence["artifacts"])
        for key, raw in artifacts.data.items():
            if artifacts.entries[key]["kind"] not in {"archive", "physical_notes", "device_capture", "peak_samples"}:
                q.require(q.decode(raw).get("synthetic") is not True, "Synthetic software artifacts cannot establish physical facility observations")
        journal = Journal(q, artifacts, evidence)
        allowed = {evidence["warehouse_id"], *sequence(q, plan.get("counterpart_warehouse_ids", []), 20, nonempty=False)}
        q.require(all(r["http_status"] != 200 or r["warehouse_id"] in allowed for r in journal.records), "Successful work extends outside the declared facility/counterpart scope")
        for case in cases:
            q.require(case["passed"] is True, "A required current facility workflow did not pass")
            q.text(case["observation"], "literal witnessed result")
            ids = sequence(q, case["artifact_ids"], 256)
            q.require(all(i in artifacts.data for i in ids), "Scenario references an uncaptured artifact")
            if case["name"] in COMMANDS:
                rows = journal.for_case(case)
                kinds = {r["request"]["command"]["type"] for r in rows if r["http_status"] == 200 and not r["body"]["replayed"] and r["phase"] == "performed"}
                q.require(any(required <= kinds for required in COMMANDS[case["name"]]), "Scenario lacks the actual original canonical work it claims")
                if case["name"] == "partial_order_remainder_recovery":
                    stories = defaultdict(dict)
                    for row in rows:
                        if row["http_status"] != 200 or row["body"]["replayed"] or row["phase"] != "performed" or row["warehouse_id"] != evidence["warehouse_id"]: continue
                        c = row["request"]["command"]
                        if c["type"] in {"unpack_shipment", "unpick", "release_unpicked", "cancel_remainder"}:
                            owner = row["event"]["shipment"]["order_id"] if c["type"] == "unpack_shipment" else c["order_id"]
                            stories[owner][c["type"]] = row["event"]["version"]
                    q.require(any(all(k in story for k in ("unpack_shipment", "unpick", "release_unpicked", "cancel_remainder"))
                                  and story["unpack_shipment"] < story["unpick"] < story["release_unpicked"] < story["cancel_remainder"]
                                  for story in stories.values()), "Remainder recovery borrows actions from different orders or reverses their causal sequence")
            if case["name"] in {"oms_conditional_handoff", "tms_planned_load"}: artifacts.reference(case, "native_receipts")
            if case["name"] == "cross_facility_lot_trace": artifacts.reference(case, "trace_pages")
            if case["name"] == "backup_restore_reconcile": artifacts.reference(case, "recovery")
            if case["name"] in {"facility_selection_pending_save", "wrong_label_rejected", "stale_operator_rescan"}: artifacts.reference(case, "device_observations")
        # No successful command may mutate stock outside the signed pilot scope.
        scope = {tuple(row[k] for k in ("sku", "lot", "location")) for row in plan["inventory_scope"]}
        for row in journal.originals.values():
            if row["warehouse_id"] == evidence["warehouse_id"] and row["phase"] == "performed":
                q.require(all(tuple(change["stock"][k] for k in ("sku", "lot", "location")) in scope for change in row["event"]["inventory_changes"]), "Acceptance work mutated custody outside its predeclared scope")
        scans, inventory = scan_and_inventory(q, evidence, journal)
        custody_deltas(q, evidence, journal)
        device_result = device_sources(q, artifacts, evidence, journal, plan)
        workflow_sources(q, journal); conversions = unit_sources(q, journal, evidence)
        traces = trace_sources(q, artifacts, evidence); native = native_sources(q, artifacts, evidence)
        _, version = reconcile(q, artifacts, evidence, plan, journal)
        peak_result = peak(q, artifacts, evidence, plan["targets"]["peak"], journal)
        recovery_result = recovery(q, artifacts, plan["targets"]["recovery"], binding)
        q.require(any(row["http_status"] == 200 and row["body"]["replayed"] for row in journal.records), "An actual exact original-command replay is missing")
        denied = [row for row in journal.records if row["http_status"] in {401, 403, 404}]
        # Domain lookups intentionally hide a foreign opaque order as absent;
        # that existing response is 422. An arbitrary 422 cannot prove isolation.
        own_orders = {row["body"]["result"]["entity_id"] for row in journal.originals.values()
                      if row["warehouse_id"] == evidence["warehouse_id"] and row["request"]["command"]["type"] == "create_order"}
        foreign = [row for row in journal.records if row["tenant"] != evidence["wms_tenant"]
                   and row["warehouse_id"] == evidence["warehouse_id"] and row["request"]["command"]["type"] == "pick"
                   and row["request"]["command"]["order_id"] in own_orders
                   and (row["http_status"] in {403, 404} or row["http_status"] == 422 and row["body"].get("error") == "Fulfillment order not found")]
        q.require(any(row["role"] == "viewer" and row["tenant"] == evidence["wms_tenant"] and row["warehouse_id"] == evidence["warehouse_id"] for row in denied)
                  and foreign, "Actual viewer/foreign denial captures are missing")
        deployment = artifacts.objects("deployment_observation")
        q.require(len(deployment) == 1 and deployment[0][1]["release"] == binding, "Deployed source/image witness capture differs from the reviewed release")
        scoped(q, deployment[0][1], expected)
        return {"release": binding, "plan_sha256": plan_digest, "signed_evidence_sha256": evidence_digest,
                "operator_key_sha256": operator_key, "controller_key_sha256": controller_key, "plan_key_sha256": plan_key,
                "witness_attested_scenarios": sorted(CURRENT_SCENARIOS), "original_commands": len(journal.originals), "captures": len(journal.records),
                "scans": scans, "inventory_rows": inventory, "final_version": version, "original_pack_observations": conversions,
                "complete_trace_captures": traces, "device_observations": device_result, "native_receipt_states": native, "peak": peak_result, "recovery": recovery_result,
                "artifacts_verified": len(artifacts.data), "artifact_bytes_verified": artifacts.total,
                "limitations": ["Signatures authenticate supplied trusted keys; witnesses own physical truth and observation completeness",
                                "Native checks validate captured receipt/replay consistency, not the external receiver or actual carrier event",
                                "Canonical request bytes are checked against durable receipt digests; Rust serialization is supplied by the release-matched offline helper"]}
    report.check("current_signed_facility_sources", verify)
    return report.finish()
