"""Harness regression tests use simulated HTTP and invented evidence only.

They qualify the validator, never a real deployment or physical facility.
"""
from argparse import Namespace
from copy import deepcopy
from datetime import datetime, timezone
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
from threading import Thread
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("qualify_native", Path(__file__).with_name("qualify-native.py"))
QUALIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(QUALIFY)
TOKENS = {"WMS_QUALIFY_OPERATOR": "wms-operator-test-token-000000000001",
          "WMS_QUALIFY_VIEWER": "wms-viewer-test-token-0000000000001",
          "WMS_QUALIFY_FOREIGN": "wms-foreign-test-token-000000000001",
          "WMS_QUALIFY_OMS": "oms-tenant-test-token-0000000000001",
          "WMS_QUALIFY_FREIGHT": "freight-operations-test-token-00001"}
ORDER_ID = "00000000-0000-0000-0000-000000000001"
WMS_ORDER_ID = "10000000-0000-0000-0000-000000000001"
SOURCE = {"id": ORDER_ID, "tenantId": "oms-a", "status": "pending",
          "updatedAt": "2026-10-17T00:00:00.123456789Z",
          "lineItems": [{"shopifyLineItemId": "line-a", "sku": "SKU", "quantity": 2},
                        {"shopifyLineItemId": "line-b", "sku": "SKU", "quantity": 3}]}


class Fixture:
    def __init__(self):
        self.mode = "valid"
        self.calls = []
        self.reads = 0
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def send(self, status, body=None):
                raw = json.dumps(body).encode() if body is not None else b""
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def do_POST(self):
                fixture.calls.append(("POST", self.path, None))
                self.send(500)

            def do_GET(self):
                authorization = self.headers.get("Authorization", "")
                bearer = authorization[7:] if authorization.startswith("Bearer ") else authorization
                oms_token = self.headers.get("x-stateset-api-key")
                fixture.calls.append(("GET", self.path, bearer or oms_token))
                if self.path == "/redirect":
                    self.send_response(307)
                    self.send_header("Location", f"http://127.0.0.1:{fixture.server.server_port}/credential-leak")
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                if self.path == "/oversized":
                    self.send(200, {"secret": "x" * QUALIFY.MAX_BYTES})
                    return
                if self.path == "/ready":
                    self.send(200)
                    return
                if self.path.startswith("/v1/tenants/"):
                    if oms_token != TOKENS["WMS_QUALIFY_OMS"] or "/oms-foreign/" in self.path:
                        self.send(200 if fixture.mode == "auth_disabled" else 401, {"data": {}})
                        return
                    if self.path.endswith("/warehouse-fulfillments"):
                        self.send(200, {"data": {"contract": QUALIFY.CONTRACT, "tenant_id": "oms-a",
                                                "conditional_revision": True, "terminal_state_fence": True,
                                                "durable_idempotency": True,
                                                "durable_rejections": fixture.mode != "legacy_schema"}})
                        return
                    source = deepcopy(SOURCE)
                    fixture.reads += 1
                    if fixture.mode == "source_changed" and fixture.reads > 1:
                        source["updatedAt"] = "2026-10-17T00:00:00.123456790Z"
                    self.send(200, {"data": source})
                    return
                if self.path == "/v1/access":
                    if bearer != TOKENS["WMS_QUALIFY_FREIGHT"]:
                        self.send(401)
                        return
                    self.send(200, {"tenant_id": "freight-b" if fixture.mode == "wrong_tenant" else "freight-a",
                                    "access": {"capabilities": [] if fixture.mode == "wrong_role" else ["operations.write"]}})
                    return
                if self.path.startswith("/v1/sync/orders"):
                    self.send(200, {"items": [SOURCE]})
                    return
                if bearer not in {TOKENS[key] for key in ("WMS_QUALIFY_OPERATOR", "WMS_QUALIFY_VIEWER", "WMS_QUALIFY_FOREIGN")}:
                    self.send(401)
                    return
                if self.path == "/v1/session":
                    viewer = bearer == TOKENS["WMS_QUALIFY_VIEWER"]
                    foreign = bearer == TOKENS["WMS_QUALIFY_FOREIGN"]
                    self.send(200, {"demo": fixture.mode == "demo", "identity": {"tenant": "foreign" if foreign else "wms-a",
                        "role": "viewer" if viewer else "operator"}, "native_integrations": {
                            "oms": {"configured": True, "shipment_writeback": "conditional_v1"}, "tms": {"configured": True}}})
                    return
                if self.path == "/v1/warehouses/wh-main":
                    foreign = bearer == TOKENS["WMS_QUALIFY_FOREIGN"]
                    self.send(200, {"warehouse": {"id": "wh-main"}, "orders": [] if foreign or fixture.mode == "empty_target" else [{"id": WMS_ORDER_ID}]})
                    return
                if self.path == f"/v1/warehouses/wh-main/orders/{WMS_ORDER_ID}":
                    foreign = bearer == TOKENS["WMS_QUALIFY_FOREIGN"]
                    self.send(404 if foreign and fixture.mode != "foreign_leak" else 200, {"id": WMS_ORDER_ID})
                    return
                if bearer == TOKENS["WMS_QUALIFY_FOREIGN"]:
                    self.send(404)
                    return
                if self.path.startswith("/v1/integrations/oms/orders"):
                    if bearer == TOKENS["WMS_QUALIFY_VIEWER"]:
                        self.send(403)
                    else:
                        self.send(200, {"items": [SOURCE]})
                    return
                if self.path == "/v1/warehouses":
                    self.send(200, {"warehouses": [{"warehouse": {"id": "wh-main"}}]})
                    return
                if self.path.endswith("/reconciliation"):
                    self.send(200, {"version": 1, "ok": fixture.mode != "corrupt", "rows": [], "receipts": [],
                                    "method": "full_append_only_ledger_and_order_line_allocations"})
                    return
                self.send(404)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.base = f"http://127.0.0.1:{self.server.server_port}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


class QualificationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory(prefix="stateset-qualification-tests-")
        cls.root = Path(cls.temporary.name)
        cls.private = cls.root / "test-only-private.pem"
        cls.public = cls.root / "test-only-public.pem"
        subprocess.run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", str(cls.private)],
                       check=True, capture_output=True)
        subprocess.run(["openssl", "pkey", "-in", str(cls.private), "-pubout", "-out", str(cls.public)], check=True, capture_output=True)

    @classmethod
    def tearDownClass(cls):
        cls.temporary.cleanup()

    def setUp(self):
        self.fixture = Fixture()
        self.addCleanup(self.fixture.close)
        self.environment = patch.dict(os.environ, TOKENS, clear=False)
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.config = {"schema_version": 1, "environment": "staging", "warehouse_id": "wh-main", "source_order_id": ORDER_ID,
                       "foreign_oms_tenant": "oms-foreign", "wms": {"tenant": "wms-a", "base_url": self.fixture.base,
                        "credential_env": "WMS_QUALIFY_OPERATOR", "viewer_credential_env": "WMS_QUALIFY_VIEWER", "foreign_credential_env": "WMS_QUALIFY_FOREIGN"},
                       "oms": {"tenant": "oms-a", "base_url": self.fixture.base, "credential_env": "WMS_QUALIFY_OMS"},
                       "freight": {"tenant": "freight-a", "base_url": self.fixture.base, "credential_env": "WMS_QUALIFY_FREIGHT"}}
        self.config_path = self.root / "config.json"
        self.config_path.write_text(json.dumps(self.config))
        self.manifest = {"schema_version": 1, "environment": "staging", "observed_at": datetime.now(timezone.utc).isoformat(),
                         "signer": "Test-only release operator", "signer_role": "release_operator",
                         "services": {name: {"tenant": self.config[name]["tenant"], "base_url": self.fixture.base,
                                              "source_sha": "a" * 40, "image_digest": "sha256:" + "b" * 64} for name in ("wms", "oms", "freight")},
                         "recipient_mapping": {"wms_oms_url": self.fixture.base, "wms_oms_tenant": "oms-a",
                                               "wms_freight_url": self.fixture.base, "wms_freight_tenant": "freight-a",
                                               "freight_oms_url": self.fixture.base, "freight_oms_tenant": "oms-a"},
                         "oms_credential_scopes": ["orders:read", "orders:write"]}

    def sign(self, value, stem):
        path, signature = self.root / f"{stem}.json", self.root / f"{stem}.sig"
        path.write_text(json.dumps(value))
        subprocess.run(["openssl", "dgst", "-sha256", "-sign", str(self.private), "-out", str(signature), str(path)], check=True, capture_output=True)
        return path, signature

    def native(self):
        manifest, signature = self.sign(self.manifest, "deployment")
        return QUALIFY.preflight(Namespace(config=self.config_path, deployment_manifest=manifest,
                                           deployment_signature=signature, trust_key=self.public, allow_loopback=True))

    def test_successful_fixture_preflight_is_explicitly_not_production_qualification(self):
        report = self.native()
        self.assertEqual(report["result"], "passed", report)
        self.assertFalse(report["production_qualified"])
        self.assertEqual(report["transport_scope"], "local_or_tunnel")
        self.assertTrue(all(method == "GET" for method, _, _ in self.fixture.calls))
        serialized = json.dumps(report)
        self.assertNotIn("SKU", serialized)
        self.assertTrue(all(token not in serialized for token in TOKENS.values()))
        foreign = QUALIFY.Client(self.fixture.base, "Authorization", TOKENS["WMS_QUALIFY_FOREIGN"])
        self.assertEqual(foreign.get("/v1/warehouses/wh-main")[0], 200)

    def test_missing_credentials_fail_closed_before_network_and_write_private_report(self):
        del os.environ["WMS_QUALIFY_OPERATOR"]
        report = self.native()
        self.assertEqual(report["result"], "unqualified")
        self.assertEqual(self.fixture.calls, [])
        output = self.root / "missing.json"
        QUALIFY.write_report(output, report)
        self.assertEqual(output.stat().st_mode & 0o777, 0o600)

    def test_legacy_schema_disabled_auth_wrong_tenant_role_demo_and_corruption_fail(self):
        for mode in ("legacy_schema", "auth_disabled", "wrong_tenant", "wrong_role", "demo", "corrupt", "source_changed", "foreign_leak", "empty_target"):
            with self.subTest(mode=mode):
                self.fixture.mode, self.fixture.reads = mode, 0
                self.assertEqual(self.native()["result"], "unqualified")

    def test_mapping_is_unqualified_when_missing_changed_unsigned_or_stale(self):
        self.manifest["recipient_mapping"]["freight_oms_tenant"] = "oms-foreign"
        self.assertEqual(self.native()["result"], "unqualified")
        self.manifest["observed_at"] = "2020-01-01T00:00:00Z"
        self.assertEqual(self.native()["result"], "unqualified")
        args = Namespace(config=self.config_path, deployment_manifest=None, deployment_signature=None,
                         trust_key=self.public, allow_loopback=True)
        self.assertEqual(QUALIFY.preflight(args)["result"], "unqualified")

    def test_redirect_never_forwards_credential_and_response_is_bounded(self):
        client = QUALIFY.Client(self.fixture.base, "Authorization", TOKENS["WMS_QUALIFY_OPERATOR"])
        for path in ("/redirect", "/oversized"):
            with self.assertRaises(QUALIFY.Unqualified):
                client.get(path)
        self.assertFalse(any(path == "/credential-leak" for _, path, _ in self.fixture.calls))

    def test_source_exact_decimal_line_identity_and_nanosecond_revision(self):
        source = deepcopy(SOURCE)
        source["lineItems"][0]["quantity"] = QUALIFY.Decimal("2.00000000000000000001")
        with self.assertRaises(QUALIFY.Unqualified):
            QUALIFY.order_projection(source)
        source = deepcopy(SOURCE)
        source["lineItems"][1]["shopifyLineItemId"] = "line-a"
        with self.assertRaises(QUALIFY.Unqualified):
            QUALIFY.order_projection(source)
        source = deepcopy(SOURCE)
        source["updatedAt"] = "2026-10-17T00:00:00.123456790Z"
        self.assertNotEqual(QUALIFY.order_projection(source), QUALIFY.order_projection(SOURCE))
        source["updatedAt"] = "2026-10-17T01:00:00.123456789+01:00"
        self.assertEqual(QUALIFY.order_projection(source), QUALIFY.order_projection(SOURCE))

    def physical(self):
        artifact = self.root / "test-only-observations.txt"
        artifact.write_text("Invented test-only physical observations. This file is not facility evidence.\n")
        return {"schema_version": 1, "performed": True, "synthetic": False,
                "observed_at": datetime.now(timezone.utc).isoformat(), "facility_id": "TEST-ONLY", "wms_tenant": "wms-a", "warehouse_id": "wh-main",
                "release_sha": "a" * 40, "signed_by": "Test Operator",
                "witnesses": [{"name": "Test Operator", "role": "operator"}, {"name": "Test Controller", "role": "inventory_controller"}],
                "scanner": {"make_model": "TEST", "device_id": "TEST", "firmware": "TEST", "input_mode": "keyboard wedge",
                            "symbology": "Code128", "label_stock": "TEST", "printer_model": "TEST", "physical_hardware": True, "printed_labels": True},
                "scenarios": [{"name": name, "passed": True, "request_ids": ["test-request-original"],
                               "observation": "Invented harness observation", "artifact_ids": ["test-artifact"]} for name in sorted(QUALIFY.SCENARIOS)],
                "scans": [{"operation": operation, "line_id": line, "bin_label": "TEST-BIN", "sku_label": "TEST-SKU", "lot_label": "TEST-LOT",
                           "request_id": "test-request-original", "quantity_each": 1} for operation in ("receive", "pick") for line in ("line-a", "line-b")],
                "inventory": [{"sku": "TEST-SKU", "lot": "TEST-LOT", "location": "TEST-BIN", "start_physical": 0, "received": 2,
                               "moved_in": 0, "moved_out": 0, "shipped": 2, "count_gain": 0, "count_loss": 0, "end_physical": 0, "end_system": 0}],
                "lost_response": {"original_request_id": "test-request-original", "retry_request_id": "test-request-original", "committed_commands": 1,
                                  "physical_effects": 1, "replayed": True},
                "peak": {"observed_actions_per_second": 3, "required_actions_per_second": 2, "p95_ms": 100, "max_p95_ms": 1000,
                         "duration_seconds": 300, "concurrent_operators": 2},
                "recovery": {"actual_rto_seconds": 30, "max_rto_seconds": 60, "actual_rpo_seconds": 0, "max_rpo_seconds": 0,
                             "post_restore_reconciliation": True, "post_backup_effects_reviewed": True},
                "artifacts": [{"id": "test-artifact", "path": artifact.name, "sha256": hashlib.sha256(artifact.read_bytes()).hexdigest()}]}

    def facility(self, evidence):
        path, signature = self.sign(evidence, "facility")
        return QUALIFY.facility(Namespace(profile="legacy-v1", evidence=path, signature=signature, trust_key=self.public, artifact_root=self.root,
                                         expected_facility="TEST-ONLY", expected_tenant="wms-a", expected_warehouse="wh-main", expected_release_sha="a" * 40))

    def test_signed_evidence_validation_never_claims_physical_observation_or_production_grade(self):
        report = self.facility(self.physical())
        self.assertEqual(report["result"], "passed", report)
        self.assertFalse(report["production_qualified"])
        self.assertTrue(report["not_performed"])

    def test_physical_equation_missing_hardware_scenario_replay_slo_and_artifact_guards(self):
        changes = [lambda x: x.update(performed=False), lambda x: x.update(synthetic=True),
                   lambda x: x["scanner"].update(physical_hardware=False), lambda x: x["scenarios"].pop(),
                   lambda x: x["inventory"][0].update(end_physical=1), lambda x: x["inventory"][0].update(received=True),
                   lambda x: x["lost_response"].update(retry_request_id="new-request"),
                   lambda x: x["lost_response"].update(physical_effects=True),
                   lambda x: x["peak"].update(p95_ms=1001), lambda x: x["peak"].update(duration_seconds=299),
                   lambda x: x["recovery"].update(actual_rpo_seconds=1), lambda x: x["artifacts"][0].update(sha256="0" * 64),
                   lambda x: x["artifacts"][0].update(path="../outside.txt"),
                   lambda x: x["witnesses"][1].update(name="Test Operator"),
                   lambda x: x.update(facility_id="another-facility"), lambda x: x.update(warehouse_id="another-warehouse"),
                   lambda x: x.update(release_sha="b" * 40), lambda x: x.update(wms_tenant="another-tenant")]
        for change in changes:
            value = self.physical()
            change(value)
            with self.subTest(change=change):
                self.assertEqual(self.facility(value)["result"], "unqualified")

    def test_unsigned_or_tampered_evidence_is_rejected(self):
        path, signature = self.sign(self.physical(), "tampered")
        path.write_text(path.read_text() + " ")
        for sig in (None, signature):
            result = QUALIFY.facility(Namespace(profile="legacy-v1", evidence=path, signature=sig, trust_key=self.public, artifact_root=self.root,
                                               expected_facility="TEST-ONLY", expected_tenant="wms-a", expected_warehouse="wh-main", expected_release_sha="a" * 40))
            self.assertEqual(result["result"], "unqualified")

    def test_replaced_public_key_file_cannot_change_reported_verification_authority(self):
        path, signature = self.sign(self.physical(), "authority")
        original = self.public.read_bytes()
        expected = subprocess.run(["openssl", "pkey", "-pubin", "-in", str(self.public), "-outform", "DER"],
                                  check=True, capture_output=True).stdout
        original_run = subprocess.run

        def replace_after_verification(command, **kwargs):
            result = original_run(command, **kwargs)
            if command[1] == "dgst":
                self.public.write_bytes(b"replaced untrusted key bytes\n")
            return result

        try:
            with patch.object(QUALIFY.subprocess, "run", side_effect=replace_after_verification):
                _, fingerprint, _ = QUALIFY.signed_file(path, signature, self.public)
            self.assertEqual(fingerprint, hashlib.sha256(expected).hexdigest())
        finally:
            self.public.write_bytes(original)

    def test_duplicate_fields_urls_credentials_and_non_finite_json_are_rejected(self):
        for raw in (b'{"a":1,"a":2}', b'{"a":NaN}', b'{"a":Infinity}'):
            with self.assertRaises(QUALIFY.Unqualified):
                QUALIFY.decode(raw)
        for url in ("http://external.example", "https://u:p@example.com", "https://example.com/?token=secret", "https://example.com/path", self.fixture.base):
            with self.assertRaises(QUALIFY.Unqualified):
                QUALIFY.base_url(url)

    def test_release_gate_missing_url_fails_with_complete_stage_summary(self):
        with tempfile.TemporaryDirectory(prefix="stateset-gate-missing-") as directory:
            environment = os.environ.copy()
            environment.pop("WMS_TEST_DATABASE_URL", None)
            environment.pop("WMS_RESTORE_ADMIN_URL", None)
            environment["WMS_RELEASE_ARTIFACT_DIR"] = directory
            result = subprocess.run(["bash", str(Path(__file__).with_name("qualify-release.sh"))],
                                    env=environment, capture_output=True, text=True, timeout=15)
            self.assertNotEqual(result.returncode, 0)
            root = Path(directory)
            summary = json.loads((root / "summary.json").read_text())
            self.assertEqual(summary["result"], "failed")
            self.assertEqual(summary["stages"][0]["status"], "failed")
            self.assertTrue(all(stage["status"] == "not_run" for stage in summary["stages"][1:]))
            self.assertIn("Publishable evidence:", result.stdout)

    def test_release_gate_rejects_non_test_target_before_connecting(self):
        with tempfile.TemporaryDirectory(prefix="stateset-gate-unsafe-") as directory:
            environment = os.environ.copy()
            environment["WMS_TEST_DATABASE_URL"] = "postgresql://synthetic-user:test-only-private-password@127.0.0.1:1/production"
            environment.pop("WMS_RESTORE_ADMIN_URL", None)
            environment["WMS_RELEASE_ARTIFACT_DIR"] = directory
            result = subprocess.run(["bash", str(Path(__file__).with_name("qualify-release.sh"))],
                                    env=environment, capture_output=True, text=True, timeout=15)
            self.assertNotEqual(result.returncode, 0)
            root = Path(directory)
            summary = json.loads((root / "publish/summary.json").read_text())
            self.assertEqual(summary["stages"][0]["status"], "failed")
            self.assertTrue(all(stage["status"] == "not_run" for stage in summary["stages"][1:]))
            for path in (root / "publish").glob("*"):
                self.assertNotIn("test-only-private-password", path.read_text())

    def test_release_gate_refuses_to_overwrite_existing_evidence(self):
        with tempfile.TemporaryDirectory(prefix="stateset-gate-existing-") as directory:
            marker = Path(directory) / "existing-evidence.txt"
            marker.write_text("Original evidence must remain unchanged")
            environment = dict(os.environ, WMS_RELEASE_ARTIFACT_DIR=directory)
            result = subprocess.run(["bash", str(Path(__file__).with_name("qualify-release.sh"))],
                                    env=environment, capture_output=True, text=True, timeout=15)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(marker.read_text(), "Original evidence must remain unchanged")
            self.assertEqual(len(list(Path(directory).iterdir())), 1)


if __name__ == "__main__":
    unittest.main()
