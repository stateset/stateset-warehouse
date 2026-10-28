"""Synthetic verifier regressions, never facility or deployed acceptance evidence."""
from argparse import Namespace
from copy import deepcopy
from datetime import datetime, timedelta, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from types import SimpleNamespace
import unittest

ROOT = Path(__file__).resolve().parent.parent

def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value

Q = module("qualify_native_facility_tests", ROOT / "scripts/qualify-native.py")
V = module("facility_evidence_tests", ROOT / "scripts/facility_evidence.py")
CONTEXT = SimpleNamespace(**{name: getattr(Q, name) for name in ("Report", "require", "text", "identifier", "integer", "number", "stamp", "recent", "signed_file", "decode")})
FIXTURE = json.loads((ROOT / "tests/fixtures/facility-v2-command-journal.json").read_text())
TRACE_FIXTURE = json.loads((ROOT / "tests/fixtures/facility-v2-trace.json").read_text())


class InventedSignedTrial:
    """Verifier test only. Hardware, timing and witness claims are invented.

    Backend request/receipt/ledger joins come from the retained synthetic local
    rehearsal. Only this disposable copy remaps event clocks to a fictitious
    recent trial. Nothing here is publishable facility acceptance evidence.
    """
    def __init__(self, root):
        self.root, self.documents = root, {}
        release = ROOT / "docs/qualification/release-20261018-pack-units"
        inputs = json.loads((release / "source-inputs-before.json").read_text())
        provenance = json.loads((release / "provenance.json").read_text())
        self.args = Namespace(profile="current-v2", qualified_release=release,
            expected_release_summary_sha256=V.digest((release / "summary.json").read_bytes()),
            expected_source_inputs_sha256=inputs["sha256"], expected_image_digest=provenance["image_id"],
            expected_facility="invented-test-facility", expected_tenant="restore-rehearsal", expected_warehouse="wh-main",
            evidence=root / "evidence.json", signature=root / "operator.sig", trust_key=root / "operator.pub",
            controller_signature=root / "controller.sig", controller_trust_key=root / "controller.pub",
            plan=root / "plan.json", plan_signature=root / "plan.sig", plan_trust_key=root / "plan.pub", artifact_root=root)
        binding = V.release(CONTEXT, self.args)
        self.keys = {}
        for name in ("operator", "controller", "plan"):
            private, public = root / (name + ".key"), root / (name + ".pub")
            self.command("openssl", "genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256", "-out", str(private))
            self.command("openssl", "pkey", "-in", str(private), "-pubout", "-out", str(public))
            self.keys[name] = V.digest(self.command("openssl", "pkey", "-pubin", "-in", str(public), "-outform", "DER"))
        now = datetime.now(timezone.utc)
        start = now - timedelta(minutes=15)
        self.start = start
        scope = dict(facility_id=self.args.expected_facility, wms_tenant=self.args.expected_tenant, warehouse_id="wh-main")
        self.evidence = dict(schema_version=2, profile=V.PROFILE, performed=True, synthetic=False, **scope,
            release=binding, started_at=start.isoformat(), ended_at=(start + timedelta(minutes=10)).isoformat(), observed_at=now.isoformat(),
            witnesses=[dict(role=role, name="Invented " + name, key_sha256=self.keys[name]) for role, name in (("operator", "operator"), ("inventory_controller", "controller"))],
            scanner=dict(physical_hardware=True, printed_labels=True, **{k: "Invented test claim" for k in ("make_model", "device_id", "firmware", "input_mode", "symbology", "label_stock", "printer_model")}),
            peak=dict(started_at=(start + timedelta(minutes=4)).isoformat(), duration_seconds=300, concurrent_operators=2, p95_ms=100, observed_actions_per_second=2),
            scenarios=[], inventory=[], scans=[], artifacts=[])
        records = deepcopy(FIXTURE["records"])
        peaks = []
        event_clocks = {}
        for r in records:
            r["phase"] = "performed"
            if r["http_status"] != 200: continue
            key = r["event"]["id"]
            if key not in event_clocks:
                is_peak = r["request"]["command"].get("reason") == "Synthetic timing-sample source command; no actual physical trial"
                if is_peak:
                    elapsed = (len(peaks) // 2) * 1000
                    event_clocks[key] = (start + timedelta(minutes=4, milliseconds=elapsed + 50)).isoformat()
                    peaks.append(dict(capture_id=r["capture_id"], start_elapsed_ms=elapsed, duration_ms=100))
                else: event_clocks[key] = (start + timedelta(seconds=len(event_clocks) / 100)).isoformat()
            r["event"]["occurred_at"] = event_clocks[key]
        assert len(peaks) == 600, "Test source must retain all 600 original timing-source requests"
        self.records = records
        self.add("command_journal", dict(schema_version=1, records=records))
        # An independent exported append-only movement ledger supplies the
        # expected gross custody columns, not the verifier's stock-delta code.
        columns = ("received", "moved_in", "moved_out", "shipped", "transfer_in", "transfer_out", "count_gain", "count_loss")
        inventory = {}
        for movement in FIXTURE["ledger_source"]:
            for side in ("from", "to"):
                location = movement[side]
                if location is None: continue
                identity = (movement["sku"], movement["lot"], location)
                row = inventory.setdefault(identity, dict(zip(("sku", "lot", "location"), identity), start_physical=0, **dict.fromkeys(columns, 0)))
                kind = movement["kind"]
                label = {"receipt": "received", "shipment": "shipped", "transfer_dispatch": "transfer_out", "transfer_receive": "transfer_in"}.get(kind)
                if kind == "count": label = "count_gain" if side == "to" else "count_loss"
                if kind in {"putaway", "pick", "unpick"}: label = "moved_in" if side == "to" else "moved_out"
                assert label is not None
                row[label] += movement["quantity"]
        for source in FIXTURE["reconciliation_source"]["rows"]:
            identity = tuple(source[k] for k in ("sku", "lot", "location"))
            row = inventory.setdefault(identity, dict(zip(("sku", "lot", "location"), identity), start_physical=0, **dict.fromkeys(columns, 0)))
            row["end_physical"] = row["end_system"] = source["on_hand"]
        self.evidence["inventory"] = list(inventory.values())
        self.add("full_reconciliation", dict(tenant="restore-rehearsal", warehouse_id="wh-main", response=deepcopy(FIXTURE["reconciliation_source"])))
        creators = {r["body"]["result"]["entity_id"]: r["request"]["command"] for r in records if r["http_status"] == 200 and not r["body"]["replayed"] and r["request"]["command"]["type"] in {"create_receipt", "create_order"}}
        for r in records:
            c = r["request"]["command"]
            if r["warehouse_id"] != "wh-main" or r["http_status"] != 200 or r["body"]["replayed"] or c["type"] not in {"receive", "pick"}: continue
            owner = c.get("receipt_id", c.get("order_id")); original = creators.get(owner)
            if not original or not original["reference"].startswith("FACILITY-"): continue
            line = next(l for l in original["lines"] if l["id"] == c["line_id"])
            labels = dict(bin_label=c["location"], sku_label=line["sku"], lot_label=line["lot"] if c["type"] == "receive" else c["lot"])
            self.evidence["scans"].append(dict(operation=c["type"], accepted=True, line_id=c["line_id"], owner_id=owner, quantity_each=c["quantity"], **labels, expected=labels.copy(), capture_id=r["capture_id"], review_capture_id=r["capture_id"]))
        rejected = deepcopy(self.evidence["scans"][0]); rejected.update(accepted=False, capture_id=None, physical_effects=0, sku_label="WRONG-SKU")
        self.evidence["scans"].append(rejected)
        replay = next(r for r in records if r["http_status"] == 200 and r["warehouse_id"] == "wh-main" and r["body"]["replayed"])
        original = next(r for r in records if r["http_status"] == 200 and not r["body"]["replayed"] and r["event"]["id"] == replay["event"]["id"])
        self.evidence["lost_response"] = dict(original_capture_id=original["capture_id"], retry_capture_id=replay["capture_id"], physical_effects=1)
        self.add("device_capture", b"Invented test display capture; no physical hardware observation.")
        observations = [dict(kind="pending_facility_switch", pending_kind=kind, pending_save_state="uncertain", switch_enabled=False,
            warehouse_metadata=dict(id="wh-main", code="MAIN", city="Invented test city"), displayed_warehouse_id="wh-main", displayed_code="MAIN", displayed_city="Invented test city",
            attempted_warehouse_id="wh-facility-protocol-peer", pending_request_id=original["request"]["request_id"], device_capture_artifact_id="device_capture") for kind in ("warehouse", "native")]
        stale = next(r for r in records if r["http_status"] == 409)
        fresh = next(r for r in records if r["http_status"] == 200 and not r["body"]["replayed"] and r["request"]["command"] == stale["request"]["command"])
        observations.append(dict(kind="stale_rescan", denied_capture_id=stale["capture_id"], fresh_capture_id=fresh["capture_id"]))
        move = next(r for r in records if r["warehouse_id"] == "wh-main" and r["request"]["command"]["type"] == "move_stock")
        observations.append(dict(kind="replenishment_move", move_capture_id=move["capture_id"], plan=dict(warehouse_id="wh-main", version=move["request"]["expected_version"], tasks=[dict(command=move["request"]["command"], quantity=move["request"]["command"]["quantity"], resource_versions=move["request"].get("resource_versions"))])))
        self.add("device_observations", dict(observations=observations))
        trace = deepcopy(TRACE_FIXTURE)
        before = [dict(warehouse_id=r["warehouse_id"], sku=trace["sku"], lot=trace["lot"], location=r["document"]["location"], on_hand=r["document"]["physical"], reserved=r["document"]["reserved"]) for r in trace["sections"]["positions"]]
        pages = []
        for section, rows in trace["sections"].items():
            items = deepcopy(rows)
            pages.append(dict(query=dict(sku=trace["sku"], lot=trace["lot"], section=section), response=dict(section=section, review=trace["review"], checked_at=start.isoformat(), sku=trace["sku"], lot=trace["lot"], summary=trace["summary"], page_source_ok=True, items=items, next_cursor=None)))
        self.add("trace_pages", dict(checked_at=start.isoformat(), before_custody=before, after_custody=deepcopy(before), pages=pages))
        native = []
        for kind in ("oms_fulfillment", "tms_transport"):
            for state in ("completed", "rejected", "uncertain"):
                response = dict(found=True, request_id="invented-" + kind + state, kind=kind, status=state, replayed=False, attempts=1, evidence=dict(invented=True), response=dict(invented=True) if state == "completed" else None)
                row = dict(tenant="restore-rehearsal", warehouse_id="wh-main", actor="Invented native actor", request_id=response["request_id"], response=response)
                native.append(row)
                if state == "completed":
                    retry = deepcopy(row); retry["response"]["replayed"] = True; native.append(retry)
        self.add("native_receipts", dict(records=native))
        self.add("peak_samples", b"\n".join(json.dumps(r).encode() for r in peaks) + b"\n")
        self.add("archive", b"Invented test archive; no physical recovery observation.")
        table_evidence = json.loads((release / "archive_restore.json").read_text())["verification"]["table_evidence"]
        hashes = {k: v["sha256"] for k, v in table_evidence.items()}
        self.add("recovery", dict(release=binding, post_backup_effects_reviewed=True, backup_at=start.isoformat(), incident_at=(start + timedelta(seconds=1)).isoformat(), ready_at=(start + timedelta(seconds=11)).isoformat(), last_durable_effect_at=start.isoformat(), last_restored_effect_at=start.isoformat(), source_table_hashes=hashes, restored_table_hashes=hashes.copy(), reconciliation=dict(ok=True, lot_controls=dict(ok=True), units=dict(ok=True)), archive_artifact_id="archive"))
        self.add("deployment_observation", dict(**scope, release=binding))
        self.plan = dict(schema_version=2, profile=V.PROFILE, **scope, release=binding, approved_at=(start - timedelta(seconds=1)).isoformat(), scenarios=sorted(V.CURRENT_SCENARIOS),
            counterpart_warehouse_ids=sorted({r["warehouse_id"] for r in records if r["http_status"] == 200} - {"wh-main"}),
            inventory_scope=[{k:r[k] for k in ("sku", "lot", "location")} for r in self.evidence["inventory"]],
            targets=dict(peak=dict(duration_seconds=300, concurrent_operators=2, required_actions_per_second=2, max_p95_ms=100), recovery=dict(max_rto_seconds=10, max_rpo_seconds=0)))
        for name in sorted(V.CURRENT_SCENARIOS):
            self.evidence["scenarios"].append(dict(name=name, passed=True, observation="Invented verifier test claim; no physical acceptance.", artifact_ids=list(self.documents), capture_ids=[r["capture_id"] for r in records]))

    @staticmethod
    def command(*args):
        return subprocess.run(args, check=True, capture_output=True, timeout=10).stdout

    def add(self, kind, value): self.documents[kind] = value

    def write(self):
        artifacts = []
        for kind, value in self.documents.items():
            raw = value if isinstance(value, bytes) else json.dumps(value, separators=(",", ":")).encode()
            path = self.root / (kind + ".capture"); path.write_bytes(raw)
            artifacts.append(dict(id=kind, path=path.name, kind=kind, sha256=V.digest(raw)))
        self.evidence["artifacts"] = artifacts
        self.args.plan.write_text(json.dumps(self.plan))
        self.evidence["plan_sha256"] = V.digest(self.args.plan.read_bytes())
        self.args.evidence.write_text(json.dumps(self.evidence))
        for signer, path, signature in (("plan", self.args.plan, self.args.plan_signature), ("operator", self.args.evidence, self.args.signature), ("controller", self.args.evidence, self.args.controller_signature)):
            self.command("openssl", "dgst", "-sha256", "-sign", str(self.root / (signer + ".key")), "-out", str(signature), str(path))

    def run(self):
        self.write()
        return Q.facility(self.args)


class FacilityEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="wms-facility-v2-tests-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.evidence = {"wms_tenant": "restore-rehearsal", "warehouse_id": "wh-main", "started_at": "2098-01-01T00:00:00Z", "ended_at": "2099-01-01T00:00:00Z"}

    def artifact(self, data, kind="command_journal", name="journal.json"):
        path = self.root / name
        path.write_text(json.dumps(data))
        entry = {"id": kind, "path": name, "kind": kind, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
        return entry

    def journal(self, fixture=None):
        data = deepcopy(FIXTURE if fixture is None else fixture)
        store = V.Artifacts(CONTEXT, self.root, [self.artifact(data)])
        return V.Journal(CONTEXT, store, self.evidence)

    def test_actual_synthetic_archive_requests_match_original_receipt_bytes_and_new_workflow_sources(self):
        journal = self.journal()
        counts = V.workflow_sources(CONTEXT, journal)
        self.assertGreater(counts["cancel_remainder"], 0)
        self.assertGreater(counts["record_transfer_loss"], 0)
        self.assertGreater(counts["handoff_shipment"], 0)
        self.assertEqual(V.unit_sources(CONTEXT, journal), 10)
        self.assertEqual(len(journal.records), len(journal.originals) + 5)

    def test_changed_actor_envelope_event_result_or_canonical_bytes_cannot_borrow_receipt(self):
        changes = [lambda r: r.update(actor="another-actor"),
                   lambda r: r["request"].update(expected_version=9999),
                   lambda r: r["event"].update(version=9999),
                   lambda r: r["body"]["result"].update(entity_id="another-entity"),
                   lambda r: r.update(canonical_request_json=r["canonical_request_json"] + " ")]
        for change in changes:
            fixture = deepcopy(FIXTURE); change(fixture["records"][0])
            with self.subTest(change=change), self.assertRaises(Q.Unqualified): self.journal(fixture)

    def test_replayed_capture_without_original_and_second_fresh_commit_are_rejected(self):
        for mode in ("missing", "second"):
            fixture = deepcopy(FIXTURE)
            if mode == "missing": fixture["records"] = [fixture["records"][-1]]
            else: fixture["records"][-1]["body"]["replayed"] = False
            with self.subTest(mode=mode), self.assertRaises(Q.Unqualified): self.journal(fixture)

    def test_balanced_stock_cannot_hide_excess_reversal_or_reinterpreted_original_packs(self):
        journal = self.journal()
        records = deepcopy(journal.records)
        row = next(r for r in records if r["request"]["command"]["type"] == "reverse_transfer_loss")
        row["request"]["command"]["quantity"] = 999
        row["event"]["transfer_loss_record"]["quantity"] = 999
        with self.assertRaises(Q.Unqualified): V.workflow_sources(CONTEXT, SimpleNamespace(records=records))
        originals = deepcopy(journal.originals)
        row = next(r for r in originals.values() if r["request"].get("unit_inputs"))
        row["request"]["unit_inputs"][0]["pack_quantity"] += 1
        with self.assertRaises(Q.Unqualified): V.unit_sources(CONTEXT, SimpleNamespace(originals=originals))

    def test_loss_and_pack_metadata_cannot_claim_stock_changes(self):
        journal = self.journal(); records = deepcopy(journal.records)
        row = next(r for r in records if r["request"]["command"]["type"] == "record_transfer_loss")
        row["event"]["inventory_changes"] = [{"invented": "physical credit"}]
        with self.assertRaises(Q.Unqualified): V.workflow_sources(CONTEXT, SimpleNamespace(records=records))

    def test_artifact_snapshots_are_consumed_without_reopening_a_replaced_path(self):
        entry = self.artifact({"original": True}, "physical_notes")
        store = V.Artifacts(CONTEXT, self.root, [entry])
        (self.root / entry["path"]).write_text('{"replacement":true}')
        self.assertEqual(store.objects("physical_notes")[0][1], {"original": True})

    def test_artifact_symlinks_path_escapes_duplicate_ids_and_changed_bytes_are_rejected(self):
        original = self.artifact({"original": True}, "physical_notes")
        for mode in ("escape", "symlink", "duplicate", "content"):
            entry = dict(original)
            if mode == "escape": entry["path"] = "../journal.json"
            if mode == "symlink":
                (self.root / "alias").symlink_to(self.root / original["path"]); entry["path"] = "alias"
            if mode == "content": entry["sha256"] = "0" * 64
            entries = [entry, entry] if mode == "duplicate" else [entry]
            with self.subTest(mode=mode), self.assertRaises((Q.Unqualified, OSError)):
                V.Artifacts(CONTEXT, self.root, entries)

    def test_approved_current_release_retains_distinct_software_and_image_build_hashes(self):
        release = ROOT / "docs/qualification/release"
        inputs = json.loads((release / "source-inputs-before.json").read_text())
        provenance = json.loads((release / "provenance.json").read_text())
        args = Namespace(qualified_release=release, expected_release_summary_sha256=hashlib.sha256((release / "summary.json").read_bytes()).hexdigest(),
                         expected_source_inputs_sha256=inputs["sha256"], expected_image_digest=provenance["image_id"])
        result = V.release(CONTEXT, args)
        self.assertEqual(result["source_inputs_sha256"], inputs["sha256"])
        self.assertEqual(result["image_source_inputs_sha256"], provenance["image_source_inputs_sha256"])
        for key, value in (("expected_release_summary_sha256", "0" * 64), ("expected_source_inputs_sha256", "0" * 64), ("expected_image_digest", "sha256:" + "0" * 64)):
            changed = Namespace(**vars(args)); setattr(changed, key, value)
            with self.subTest(key=key), self.assertRaises(Q.Unqualified): V.release(CONTEXT, changed)

    def test_release_rejects_reordered_or_duplicate_source_entries_with_recomputed_digests(self):
        original = ROOT / "docs/qualification/release"
        for mutation in ("reorder", "duplicate"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as tmp:
                release = Path(tmp) / "release"
                shutil.copytree(original, release)
                inputs = json.loads((release / "source-inputs-before.json").read_text())
                if mutation == "reorder":
                    inputs["inputs"][0], inputs["inputs"][1] = inputs["inputs"][1], inputs["inputs"][0]
                else:
                    inputs["inputs"].insert(1, deepcopy(inputs["inputs"][0]))
                inputs["sha256"] = V.digest(json.dumps(inputs["inputs"], sort_keys=True, separators=(",", ":")).encode())
                for name in ("source-inputs-before.json", "source-inputs-after.json"):
                    (release / name).write_text(json.dumps(inputs))
                summary = json.loads((release / "summary.json").read_text())
                for item in summary["artifacts"]:
                    item["sha256"] = V.digest((release / item["path"]).read_bytes())
                (release / "summary.json").write_text(json.dumps(summary))
                provenance = json.loads((release / "provenance.json").read_text())
                args = Namespace(qualified_release=release, expected_release_summary_sha256=V.digest((release / "summary.json").read_bytes()),
                                 expected_source_inputs_sha256=inputs["sha256"], expected_image_digest=provenance["image_id"])
                with self.assertRaisesRegex(Q.Unqualified, "repeats or reorders"):
                    V.release(CONTEXT, args)

    def test_current_default_cannot_silently_treat_legacy_evidence_as_current(self):
        report = Q.facility(Namespace())
        self.assertEqual(report["result"], "unqualified")
        self.assertEqual(report["profile"], "current-v2")
        self.assertFalse(report["production_qualified"])

    def test_native_text_matches_utf8_byte_and_control_bounds(self):
        for value in ("x" * 241, "é" * 121, "name\u0085hidden", "name\x00hidden"):
            with self.subTest(value=value), self.assertRaises(Q.Unqualified): Q.text(value, "identity")

    def test_complete_invented_dual_signed_submission_checks_actual_source_joins(self):
        trial = InventedSignedTrial(self.root)
        report = trial.run()
        self.assertEqual(report["result"], "passed", report["checks"])
        self.assertFalse(report["production_qualified"])
        detail = report["checks"][0]["detail"]
        self.assertEqual(len(detail["witness_attested_scenarios"]), 23)
        self.assertEqual(detail["peak"]["samples"], 600)
        self.assertEqual(detail["recovery"]["tables"], 43)

    def trial_sources(self):
        trial = InventedSignedTrial(self.root)
        trial.write()
        artifacts = V.Artifacts(CONTEXT, self.root, trial.evidence["artifacts"])
        journal = V.Journal(CONTEXT, artifacts, trial.evidence)
        return trial, artifacts, journal

    def test_current_submission_rejects_old_matrix_late_plan_and_unrelated_foreign_validation(self):
        trial = InventedSignedTrial(self.root)
        original_plan, original_evidence, original_documents = deepcopy(trial.plan), deepcopy(trial.evidence), deepcopy(trial.documents)
        for mode in ("old_matrix", "late_plan", "foreign_422", "synthetic", "wrong_image", "baseline_pack", "borrowed_recovery", "old_reconciliation"):
            trial.plan, trial.evidence, trial.documents = deepcopy(original_plan), deepcopy(original_evidence), deepcopy(original_documents)
            if mode == "old_matrix": trial.plan["scenarios"] = sorted(V.BASE_SCENARIOS)
            if mode == "late_plan": trial.plan["approved_at"] = trial.evidence["ended_at"]
            if mode == "foreign_422":
                row = next(r for r in trial.documents["command_journal"]["records"] if r["tenant"] == "facility-foreign")
                row["body"]["error"] = "Invalid quantity"
            if mode == "synthetic": trial.documents["command_journal"]["synthetic"] = True
            if mode == "wrong_image": trial.evidence["release"]["image_digest"] = "sha256:" + "0" * 64
            if mode == "baseline_pack":
                for row in trial.documents["command_journal"]["records"]:
                    if row["http_status"] == 200 and row["request"]["command"]["type"] == "set_unit_pack":
                        row["phase"] = "baseline"; row["event"]["occurred_at"] = (trial.start - timedelta(seconds=1)).isoformat()
            if mode == "borrowed_recovery":
                records = trial.documents["command_journal"]["records"]
                owner = next(r["request"]["command"]["order_id"] for r in records if r["request"]["command"]["type"] == "release_unpicked")
                excluded = {r["capture_id"] for r in records if r["request"]["command"]["type"] == "unpack_shipment" and r["event"]["shipment"]["order_id"] == owner}
                case = next(c for c in trial.evidence["scenarios"] if c["name"] == "partial_order_remainder_recovery")
                case["capture_ids"] = [key for key in case["capture_ids"] if key not in excluded]
            if mode == "old_reconciliation": trial.documents["full_reconciliation"]["response"]["version"] = 1
            with self.subTest(mode=mode):
                report = trial.run()
                self.assertEqual(report["result"], "unqualified", report["checks"])

    def test_independent_signatures_authenticate_same_evidence_and_plan_bytes(self):
        trial = InventedSignedTrial(self.root)
        for mode in ("same_key", "evidence_tampered", "plan_tampered", "wrong_controller"):
            trial.write()
            args = Namespace(**vars(trial.args))
            if mode == "same_key": args.controller_signature, args.controller_trust_key = args.signature, args.trust_key
            if mode == "evidence_tampered": args.evidence.write_bytes(args.evidence.read_bytes() + b" ")
            if mode == "plan_tampered": args.plan.write_bytes(args.plan.read_bytes() + b" ")
            if mode == "wrong_controller": args.controller_trust_key = args.trust_key
            with self.subTest(mode=mode): self.assertEqual(Q.facility(args)["result"], "unqualified")

    def test_peak_uses_original_samples_concurrent_actors_and_recomputed_summaries(self):
        trial, artifacts, journal = self.trial_sources()
        samples = [json.loads(line) for line in trial.documents["peak_samples"].splitlines()]
        targets = trial.plan["targets"]["peak"]
        self.assertEqual(V.peak(CONTEXT, artifacts, trial.evidence, targets, journal)["samples"], 600)
        for mode in ("duplicate", "fake_p95", "fake_rate", "sequential", "gap", "borrowed_event", "replay", "population", "duration"):
            data, evidence, changed_targets = deepcopy(samples), deepcopy(trial.evidence), deepcopy(targets)
            by_capture = deepcopy(journal.by_capture)
            if mode == "duplicate": data[-1] = data[0]
            if mode == "fake_p95": evidence["peak"]["p95_ms"] = 99
            if mode == "fake_rate": evidence["peak"]["observed_actions_per_second"] = 3
            if mode == "sequential":
                for index, row in enumerate(data):
                    row["start_elapsed_ms"] += 100 if index % 2 else 0
            if mode == "gap": data = [r for r in data if not 100000 <= r["start_elapsed_ms"] < 120000]
            if mode == "borrowed_event": by_capture[data[0]["capture_id"]]["event"]["occurred_at"] = trial.evidence["started_at"]
            if mode == "replay": by_capture[data[0]["capture_id"]]["body"]["replayed"] = True
            if mode == "population": changed_targets["concurrent_operators"] = evidence["peak"]["concurrent_operators"] = 1
            if mode == "duration": changed_targets["duration_seconds"] = evidence["peak"]["duration_seconds"] = 86401
            raw = b"\n".join(json.dumps(r).encode() for r in data)
            changed = SimpleNamespace(data={"peak_samples": raw}, entries={"peak_samples": {"kind": "peak_samples"}})
            with self.subTest(mode=mode), self.assertRaises(Q.Unqualified):
                V.peak(CONTEXT, changed, evidence, changed_targets, SimpleNamespace(by_capture=by_capture))

    def test_scans_and_gross_custody_cannot_borrow_lines_or_hide_balanced_moves(self):
        trial, artifacts, journal = self.trial_sources()
        V.scan_and_inventory(CONTEXT, trial.evidence, journal)
        V.custody_deltas(CONTEXT, trial.evidence, journal)
        for mode in ("owner", "sku", "quantity", "bool", "balanced_omission"):
            evidence = deepcopy(trial.evidence)
            row = evidence["scans"][0]
            if mode == "owner": row["owner_id"] = evidence["scans"][-2]["owner_id"]
            if mode == "sku": row["expected"]["sku_label"] = row["sku_label"] = "ANOTHER-SKU"
            if mode == "quantity": row["quantity_each"] += 1
            if mode == "bool": row["quantity_each"] = True
            if mode == "balanced_omission":
                stock = next(r for r in evidence["inventory"] if r["moved_in"] and r["moved_out"])
                stock["moved_in"] -= 1; stock["moved_out"] -= 1
                V.scan_and_inventory(CONTEXT, evidence, journal)
                with self.assertRaises(Q.Unqualified): V.custody_deltas(CONTEXT, evidence, journal)
            else:
                with self.subTest(mode=mode), self.assertRaises((Q.Unqualified, KeyError)):
                    V.scan_and_inventory(CONTEXT, evidence, journal)

    def test_trace_pages_require_all_sections_original_cursors_sources_and_custody(self):
        trial, artifacts, _ = self.trial_sources()
        V.trace_sources(CONTEXT, artifacts, trial.evidence)
        for mode in ("truncated", "duplicate", "cursor", "source", "custody", "bool_custody", "stale"):
            document = deepcopy(trial.documents["trace_pages"])
            if mode == "truncated": document["pages"].pop()
            if mode == "duplicate": document["pages"][0]["response"]["items"].append(document["pages"][0]["response"]["items"][0])
            if mode == "cursor": document["pages"][0]["query"]["after"] = "invented-cursor"
            if mode == "source": document["pages"][0]["response"]["page_source_ok"] = False
            if mode == "custody": document["after_custody"][0]["on_hand"] += 1
            if mode == "bool_custody": document["before_custody"][0]["reserved"] = True
            if mode == "stale": document["checked_at"] = (trial.start - timedelta(seconds=1)).isoformat()
            changed = SimpleNamespace(objects=lambda kind: [("trace", document)])
            with self.subTest(mode=mode), self.assertRaises(Q.Unqualified): V.trace_sources(CONTEXT, changed, trial.evidence)

    def test_native_replay_requires_original_frozen_intent_response_and_attempt_count(self):
        trial, artifacts, _ = self.trial_sources()
        V.native_sources(CONTEXT, artifacts, trial.evidence)
        for mode in ("intent", "response", "attempts", "missing_original"):
            document = deepcopy(trial.documents["native_receipts"])
            replay = document["records"][1]["response"]
            if mode == "intent": replay["evidence"] = {"changed": True}
            if mode == "response": replay["response"] = {"changed": True}
            if mode == "attempts": replay["attempts"] += 1
            if mode == "missing_original": document["records"].pop(0)
            with self.subTest(mode=mode), self.assertRaises(Q.Unqualified):
                V.native_sources(CONTEXT, SimpleNamespace(objects=lambda kind: [("native", document)]), trial.evidence)

    def test_recovery_recomputes_nanosecond_rpo_and_requires_all_original_tables_archive(self):
        trial, artifacts, _ = self.trial_sources()
        targets, binding = trial.plan["targets"]["recovery"], trial.evidence["release"]
        V.recovery(CONTEXT, artifacts, targets, binding)
        for mode in ("nanosecond_rpo", "negative_rto", "negative_rpo", "rto", "table", "archive", "hash"):
            document, entries = deepcopy(trial.documents["recovery"]), deepcopy(artifacts.entries)
            if mode == "nanosecond_rpo":
                document["last_restored_effect_at"] = trial.start.replace(microsecond=0).isoformat()
                document["last_durable_effect_at"] = trial.start.replace(microsecond=0).strftime("%Y-%m-%dT%H:%M:%S") + ".000000001Z"
            if mode in {"negative_rto", "negative_rpo"}:
                base = trial.start.replace(microsecond=0).strftime("%Y-%m-%dT%H:%M:%S")
                document.update(backup_at=base+"Z", incident_at=base+".000000003Z", ready_at=base+".000000002Z", last_durable_effect_at=base+"Z", last_restored_effect_at=base+"Z")
                if mode == "negative_rpo": document.update(ready_at=(trial.start + timedelta(seconds=1)).isoformat(), last_durable_effect_at=base+".000000001Z", last_restored_effect_at=base+".000000002Z")
            if mode == "rto": document["ready_at"] = (trial.start + timedelta(seconds=12)).isoformat()
            if mode == "table":
                key = next(iter(document["source_table_hashes"]))
                del document["source_table_hashes"][key]; del document["restored_table_hashes"][key]
            if mode == "archive": del entries["archive"]
            if mode == "hash": document["restored_table_hashes"][next(iter(document["restored_table_hashes"]))] = "0" * 64
            with self.subTest(mode=mode), self.assertRaises(Q.Unqualified):
                V.recovery(CONTEXT, SimpleNamespace(objects=lambda kind: [("recovery", document)], entries=entries), targets, binding)

    def test_large_archive_is_hashed_without_retention_and_still_rejects_changed_bytes(self):
        path = self.root / "backup.dump"
        chunk, sha = b"x" * 1024**2, hashlib.sha256()
        with path.open("wb") as stream:
            for _ in range(33): stream.write(chunk); sha.update(chunk)
        entry = dict(id="archive", kind="archive", path=path.name, sha256=sha.hexdigest())
        artifacts = V.Artifacts(CONTEXT, self.root, [entry])
        self.assertIsNone(artifacts.data["archive"])
        self.assertEqual(artifacts.total, 33 * 1024**2)
        with path.open("r+b") as stream: stream.write(b"y")
        with self.assertRaises(Q.Unqualified): V.Artifacts(CONTEXT, self.root, [entry])

    def test_templates_remain_unperformed_private_release_bound_and_never_overwrite(self):
        template = module("facility_template_tests", ROOT / "scripts/facility-template.py")
        trial = InventedSignedTrial(self.root)
        args = Namespace(**vars(trial.args), destination=self.root / "blank")
        destination = template.create(args)
        plan = json.loads((destination / "plan.json").read_text())
        evidence = json.loads((destination / "evidence.json").read_text())
        self.assertEqual(plan["release"], trial.plan["release"])
        self.assertEqual(len(evidence["scenarios"]), 23)
        self.assertIsNone(plan["approved_at"])
        self.assertFalse(evidence["performed"])
        self.assertTrue(evidence["synthetic"])
        self.assertTrue(all(not case["passed"] for case in evidence["scenarios"]))
        self.assertEqual(destination.stat().st_mode & 0o777, 0o700)
        for name in ("plan.json", "evidence.json"): self.assertEqual((destination / name).stat().st_mode & 0o777, 0o600)
        with self.assertRaises(FileExistsError): template.create(args)

    def test_stale_outcome_pending_switch_and_lost_response_sources_cannot_be_forged(self):
        trial, artifacts, journal = self.trial_sources()
        V.device_sources(CONTEXT, artifacts, trial.evidence, journal, trial.plan)
        for mode in ("display", "switch", "pending", "stale_actor", "stale_version", "plan_quantity", "lost_effect", "lost_identity"):
            evidence, document = deepcopy(trial.evidence), deepcopy(trial.documents["device_observations"])
            records = deepcopy(journal.records); by_capture = {r["capture_id"]:r for r in records}
            if mode == "display": document["observations"][0]["displayed_code"] = "OTHER"
            if mode == "switch": document["observations"][0]["switch_enabled"] = True
            if mode == "pending": document["observations"][0]["pending_request_id"] = "unknown-request"
            if mode.startswith("stale_"):
                stale = next(r for r in records if r["http_status"] == 409)
                if mode == "stale_actor": stale["body"]["command_outcome"]["actor"] = "another-actor"
                else: stale["body"]["command_outcome"]["current_version"] = stale["request"]["expected_version"]
            if mode == "plan_quantity": document["observations"][-1]["plan"]["tasks"][0]["quantity"] += 1
            if mode == "lost_effect": evidence["lost_response"]["physical_effects"] = 2
            if mode == "lost_identity": evidence["lost_response"]["retry_capture_id"] = evidence["lost_response"]["original_capture_id"]
            store = SimpleNamespace(objects=lambda kind: [("device", document)], entries=artifacts.entries)
            with self.subTest(mode=mode), self.assertRaises(Q.Unqualified):
                V.device_sources(CONTEXT, store, evidence, SimpleNamespace(records=records, by_capture=by_capture), trial.plan)

    def test_json_receipt_boolean_is_not_an_integer_and_success_cannot_borrow_operator_authority(self):
        for mode in ("boolean", "operator"):
            fixture = deepcopy(FIXTURE)
            row = next(r for r in fixture["records"] if r["http_status"] == 200 and r["request"]["command"]["type"] == "count_stock")
            if mode == "boolean": row["body"]["result"]["version"] = True
            else: row["role"] = "operator"
            with self.subTest(mode=mode), self.assertRaises(Q.Unqualified): self.journal(fixture)

    def test_directory_symlink_fifo_and_extreme_json_depth_are_rejected(self):
        target = self.root / "real"; target.mkdir(); (target / "capture.json").write_text('{}')
        (self.root / "alias").symlink_to(target, target_is_directory=True)
        os.mkfifo(self.root / "capture.pipe")
        for path in ("alias/capture.json", "capture.pipe"):
            with self.subTest(path=path), self.assertRaises((Q.Unqualified, OSError)): V.artifact_bytes(CONTEXT, self.root, path)
        with self.assertRaises(Q.Unqualified): Q.decode(b'[' * 2000 + b'0' + b']' * 2000)
        self.assertEqual(Q.nesting_depth(Q.decode(b'[' * Q.MAX_DEPTH + b']' * Q.MAX_DEPTH)), Q.MAX_DEPTH)
        with self.assertRaises(Q.Unqualified): Q.decode(b'{"a":' * Q.MAX_DEPTH + b'[]' + b'}' * Q.MAX_DEPTH)

    def test_template_cli_rejects_invalid_scope_cleanly_before_creating_destination(self):
        trial = InventedSignedTrial(self.root)
        argv = ["python3", str(ROOT / "scripts/facility-template.py")]
        for name in ("qualified_release", "expected_release_summary_sha256", "expected_source_inputs_sha256", "expected_image_digest", "expected_tenant", "expected_warehouse"):
            argv.extend(["--" + name.replace("_", "-"), str(getattr(trial.args, name))])
        destination = self.root / "invalid-scope"
        argv.extend(["--expected-facility", " invalid ", "--destination", str(destination)])
        result = subprocess.run(argv, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 2)
        self.assertNotIn("Traceback", result.stderr)
        self.assertFalse(destination.exists())

    def test_nanosecond_run_boundaries_do_not_borrow_prior_or_future_capture_times(self):
        trial, artifacts, journal = self.trial_sources()
        base = trial.start.replace(microsecond=0).strftime("%Y-%m-%dT%H:%M:%S")
        evidence = deepcopy(trial.evidence); evidence["started_at"] = base + ".000000002Z"
        for mode in ("journal", "trace_document", "trace_page", "peak"):
            with self.subTest(mode=mode), self.assertRaises(Q.Unqualified):
                if mode == "journal":
                    fixture = deepcopy(FIXTURE); fixture["records"] = [fixture["records"][0]]
                    row = fixture["records"][0]; row["phase"] = "performed"; row["event"]["occurred_at"] = base + ".000000001Z"
                    entry = self.artifact(fixture)
                    V.Journal(CONTEXT, V.Artifacts(CONTEXT, self.root, [entry]), evidence)
                elif mode in {"trace_document", "trace_page"}:
                    document = deepcopy(trial.documents["trace_pages"])
                    if mode == "trace_document": document["checked_at"] = base + ".000000001Z"
                    else:
                        document["checked_at"] = base + ".000000002Z"
                        document["pages"][0]["response"]["checked_at"] = base + ".000000001Z"
                    V.trace_sources(CONTEXT, SimpleNamespace(objects=lambda kind:[("trace", document)]), evidence)
                else:
                    evidence["peak"]["started_at"] = base + ".000000001Z"
                    V.peak(CONTEXT, artifacts, evidence, trial.plan["targets"]["peak"], journal)


if __name__ == "__main__": unittest.main()
