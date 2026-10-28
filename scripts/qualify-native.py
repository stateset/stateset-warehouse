#!/usr/bin/env python3
"""Read-only deployed native preflight and signed facility evidence validation.

Only GET requests are supported. Passing checks never imply a production grade,
physical observation, or a successful mutating OMS/Freight lifecycle.
"""
import argparse
from datetime import datetime, timedelta, timezone
from decimal import Decimal, InvalidOperation
import hashlib
import importlib.util
from http.client import HTTPException
import json
import os
from pathlib import Path
import re
import secrets
import subprocess
import tempfile
from types import SimpleNamespace
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener
import uuid

MAX_BYTES = 2 * 1024 * 1024
MAX_DEPTH = 128
CONTRACT = "stateset.wms-fulfillment.v1"
SCENARIOS = {
    "receiving_exact_lines", "putaway", "allocation_same_sku_lines",
    "picking_exact_lines", "packing_handoff", "wrong_label_rejected",
    "short_pick_return_release_recount", "stale_operator_rescan",
    "lost_response_exact_replay", "tenant_and_role_denial",
    "oms_conditional_handoff", "tms_planned_load", "backup_restore_reconcile",
}


class Unqualified(Exception):
    """An unmet evidence or deployment condition; messages contain no secrets."""


def require(condition, message):
    if not condition:
        raise Unqualified(message)


def unique_object(pairs):
    value = {}
    for key, item in pairs:
        require(key not in value, "Duplicate JSON object field")
        value[key] = item
    return value


def nesting_depth(value):
    """Iterative, so the limit holds on every Python; 3.12+ parses far deeper than 3.8 before RecursionError."""
    deepest, pending = 0, [(value, 1)]
    while pending:
        item, level = pending.pop()
        if isinstance(item, (dict, list)):
            deepest = max(deepest, level)
            pending.extend((child, level + 1) for child in (item.values() if isinstance(item, dict) else item))
    return deepest


def decode(raw):
    require(len(raw) <= MAX_BYTES, "JSON exceeds 2 MiB")
    try:
        value = json.loads(raw, object_pairs_hook=unique_object, parse_float=Decimal,
                           parse_constant=lambda _: (_ for _ in ()).throw(Unqualified("Non-finite JSON number")))
    except (ValueError, UnicodeError, RecursionError):
        raise Unqualified("Invalid JSON") from None
    require(nesting_depth(value) <= MAX_DEPTH, f"JSON nests deeper than {MAX_DEPTH} levels")
    return value


def load(path):
    try:
        with Path(path).open("rb") as stream:
            return decode(stream.read(MAX_BYTES + 1))
    except OSError:
        raise Unqualified("Required evidence/configuration file unavailable") from None


def stamp(value):
    require(isinstance(value, str) and len(value) <= 64, "Missing RFC3339 observation timestamp")
    require(re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,9})?(?:Z|[+-]\d\d:[0-5]\d)", value),
            "Observation timestamp must use RFC3339")
    # Python 3.8 accepts only millisecond/microsecond fractions. Source revision
    # equality separately retains all nine digits in order_projection.
    parseable = re.sub(r"\.(\d+)(?=(?:Z|[+-]\d\d:\d\d)$)",
                       lambda match: "." + match.group(1)[:6].ljust(6, "0"), value)
    try:
        result = datetime.fromisoformat(parseable.replace("Z", "+00:00"))
    except ValueError:
        raise Unqualified("Invalid observation timestamp") from None
    require(result.tzinfo is not None, "Observation timestamp requires a time zone")
    return result.astimezone(timezone.utc)


def recent(value, hours=24):
    observed = stamp(value)
    age = datetime.now(timezone.utc) - observed
    require(timedelta(minutes=-5) <= age <= timedelta(hours=hours),
            "Evidence is stale or its timestamp is in the future")


def text(value, label):
    require(isinstance(value, str) and 1 <= len(value.encode("utf-8")) <= 240 and value.strip() == value
            and not any(ord(c) < 32 or 127 <= ord(c) <= 159 for c in value), f"Invalid {label}")
    return value


def identifier(value):
    require(isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9_-]{1,100}", value), "Invalid tenant/warehouse identity")
    return value


def base_url(value, allow_loopback=False):
    require(isinstance(value, str), "Missing service URL")
    try:
        url = urlsplit(value)
        port = url.port
    except ValueError:
        raise Unqualified("Invalid service URL") from None
    require(url.hostname and url.scheme in {"https", "http"} and not url.username
            and not url.password and not url.query and not url.fragment
            and url.path in {"", "/"} and (port is None or 1 <= port <= 65535),
            "Service URL must be an HTTP(S) origin without credentials, path, query or fragment")
    loopback = url.hostname in {"localhost", "127.0.0.1", "::1"}
    require(url.scheme == "https" or (allow_loopback and loopback), "Remote qualification requires HTTPS")
    require(not loopback or allow_loopback, "Loopback requires explicit --allow-loopback and remains local qualification")
    return value.rstrip("/")


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class Client:
    def __init__(self, base, header, token):
        self.base, self.header, self.token = base, header, token
        # Ignore ambient proxies: credentials are sent only to the pinned origin.
        self.opener = build_opener(ProxyHandler({}), NoRedirect())

    def get(self, path, token=True):
        require(path.startswith("/") and not path.startswith("//"), "Invalid qualification path")
        headers = {"Accept": "application/json", "User-Agent": "stateset-wms-qualification/1"}
        if token is not False:
            credential = self.token if token is True else token
            headers[self.header] = ("Bearer " if self.header == "Authorization" else "") + credential
        request = Request(self.base + path, headers=headers, method="GET")
        try:
            response = self.opener.open(request, timeout=8)
        except HTTPError as error:
            response = error
        except (URLError, TimeoutError, OSError):
            raise Unqualified("Service request failed, timed out or failed TLS verification") from None
        try:
            with response:
                status = response.code
                raw = response.read(MAX_BYTES + 1)
                require(len(raw) <= MAX_BYTES, "Service response exceeds 2 MiB")
                require(not 300 <= status < 400, "Service redirected; credentials were not forwarded")
                if not 200 <= status < 300:
                    return status, None
                # Readiness intentionally has an empty body.
                return status, decode(raw) if raw else None
        except (OSError, HTTPException):
            raise Unqualified("Service response interrupted or timed out") from None

    def json(self, path, token=True):
        status, result = self.get(path, token)
        require(status == 200 and isinstance(result, dict), "Service did not return HTTP 200 JSON object")
        return result


def credential(config, name):
    env = config.get(name)
    require(isinstance(env, str) and re.fullmatch(r"[A-Z][A-Z0-9_]{1,99}", env), "Missing credential environment variable name")
    token = os.environ.get(env, "")
    require(32 <= len(token) <= 512 and token.isascii() and all(33 <= ord(c) <= 126 for c in token),
            "Missing or invalid qualification credential; supply secrets through environment variables")
    return token


def signed_file(path, signature, key, versions=(1,)):
    require(path and signature and key, "A detached signature and independently trusted public key are required")
    try:
        with Path(path).open("rb") as stream:
            raw = stream.read(MAX_BYTES + 1)
        require(len(raw) <= MAX_BYTES, "Evidence exceeds 2 MiB")
        with Path(key).open("rb") as stream:
            trusted_key = stream.read(64 * 1024 + 1)
        require(len(trusted_key) <= 64 * 1024, "Trusted public key exceeds 64 KiB")
        # The verified bytes and decoded bytes must be the same snapshot even
        # if supplied files are replaced while OpenSSL is running. The same
        # public-key snapshot must also produce the reported authority digest.
        with tempfile.NamedTemporaryFile(prefix="stateset-evidence-") as snapshot, \
                tempfile.NamedTemporaryFile(prefix="stateset-trusted-key-") as key_snapshot:
            snapshot.write(raw)
            snapshot.flush()
            key_snapshot.write(trusted_key)
            key_snapshot.flush()
            result = subprocess.run(["openssl", "dgst", "-sha256", "-verify", key_snapshot.name,
                                     "-signature", str(signature), snapshot.name],
                                    capture_output=True, timeout=10, check=False)
            public = subprocess.run(["openssl", "pkey", "-pubin", "-in", key_snapshot.name, "-outform", "DER"],
                                    capture_output=True, timeout=10, check=False)
    except (OSError, subprocess.TimeoutExpired):
        raise Unqualified("Signature verifier unavailable") from None
    require(result.returncode == 0 and public.returncode == 0, "Evidence signature is invalid for the independently trusted key")
    data = decode(raw)
    require(isinstance(data, dict) and type(data.get("schema_version")) is int and data["schema_version"] in versions, "Unsupported evidence schema")
    return data, hashlib.sha256(public.stdout).hexdigest(), hashlib.sha256(raw).hexdigest()


def number(value, label, minimum=0):
    require(not isinstance(value, bool) and isinstance(value, (int, Decimal)) and value >= minimum,
            f"Invalid {label}")
    return value


def integer(value, label, minimum=0):
    require(type(value) is int and value >= minimum, f"Invalid whole-each {label}")
    return value


def order_projection(order):
    require(isinstance(order, dict), "Missing canonical source order")
    order_id = order.get("id")
    try:
        require(str(uuid.UUID(order_id)) == order_id, "Source order UUID must be canonical")
    except (ValueError, TypeError, AttributeError):
        raise Unqualified("Invalid source order UUID") from None
    require(order.get("status") in {"pending", "processing"}, "Qualification source must remain pending/processing")
    fulfillment = order.get("fulfillmentStatus")
    require(fulfillment is None or isinstance(fulfillment, str) and fulfillment.strip().lower() in {"", "unfulfilled", "pending"},
            "Qualification source is already fulfilled, partial or unknown")
    source = order.get("sourceOrderPayload", order.get("source_order_payload"))
    if isinstance(source, dict):
        for key in ("cancelled_at", "cancelledAt"):
            require(source.get(key) in {None, ""}, "Qualification source has a cancellation")
        for key in ("fulfillment_status", "fulfillmentStatus"):
            value = source.get(key)
            require(value is None or isinstance(value, str) and value.strip().lower() in {"", "unfulfilled", "pending"},
                    "Qualification source payload has partial/unknown fulfillment")
    raw_lines = order.get("lineItems")
    require(isinstance(raw_lines, list) and 1 <= len(raw_lines) <= 100, "Source requires 1–100 lines")
    lines, identities = [], set()
    for index, line in enumerate(raw_lines):
        require(isinstance(line, dict), "Invalid canonical source line")
        sku = text(line.get("sku"), "source SKU")
        raw_quantity = line.get("quantity")
        require(not isinstance(raw_quantity, bool), "Invalid source quantity")
        try:
            quantity = Decimal(str(raw_quantity))
        except InvalidOperation:
            raise Unqualified("Invalid source quantity") from None
        require(quantity.is_finite() and 1 <= quantity <= 1_000_000 and quantity == quantity.to_integral_value(),
                "Source quantity must be exact whole each")
        source_id = line.get("shopifyLineItemId")
        identity = f"source:{source_id}" if source_id is not None else f"position:{index}"
        require(identity not in identities, "Duplicate source line identity")
        identities.add(identity)
        lines.append({"identity": identity, "sku": sku, "quantity_each": int(quantity)})
    revision = order.get("updatedAt")
    parsed_revision = stamp(revision)
    match = re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.(\d{1,9}))?(?:Z|[+-]\d\d:\d\d)", revision)
    require(match, "Canonical source revision requires RFC3339 with at most nanosecond precision")
    # Preserve all nine fractional digits when comparing a source revision;
    # datetime alone truncates the final three digits.
    normalized_revision = parsed_revision.replace(microsecond=0).isoformat() + ":" + (match.group(1) or "").ljust(9, "0")
    return {"id": order_id, "status": order["status"], "updated_at": normalized_revision, "lines": lines}


class Report:
    def __init__(self, scope):
        self.data = {"schema_version": 1, "scope": scope,
                     "recorded_at": datetime.now(timezone.utc).isoformat(),
                     "result": "unqualified", "production_qualified": False,
                     "checks": [], "not_performed": []}

    def check(self, name, callback):
        try:
            detail = callback()
            self.data["checks"].append({"name": name, "passed": True, "detail": detail})
            return True
        except (Unqualified, KeyError, TypeError, ValueError, AttributeError, OverflowError, OSError, RecursionError) as error:
            # Only our explicit messages are safe; arbitrary exception text may contain input/secrets.
            detail = str(error) if isinstance(error, Unqualified) else "Evidence shape/field is invalid"
            self.data["checks"].append({"name": name, "passed": False, "detail": detail})
            return False

    def finish(self):
        if self.data["checks"] and all(check["passed"] for check in self.data["checks"]):
            self.data["result"] = "passed"
        return self.data


def deployment_check(args, config):
    manifest, fingerprint, evidence_digest = signed_file(args.deployment_manifest, args.deployment_signature, args.trust_key)
    recent(manifest.get("observed_at"))
    require(manifest.get("environment") == config["environment"], "Deployment environment differs from qualification target")
    text(manifest.get("signer"), "deployment signer")
    require(manifest.get("signer_role") == "release_operator", "Deployment mapping requires release operator attestation")
    for name in ("wms", "oms", "freight"):
        service = manifest["services"][name]
        require(service["base_url"].rstrip("/") == config[name]["base_url"].rstrip("/")
                and service["tenant"] == config[name]["tenant"], "Attested service recipient differs from configured target")
        require(re.fullmatch(r"[0-9a-f]{40}", service.get("source_sha", ""))
                and re.fullmatch(r"sha256:[0-9a-f]{64}", service.get("image_digest", "")),
                "Deployment requires exact source and image identities")
    mapping = manifest["recipient_mapping"]
    require(mapping == {"wms_oms_url": config["oms"]["base_url"].rstrip("/"),
                        "wms_oms_tenant": config["oms"]["tenant"],
                        "wms_freight_url": config["freight"]["base_url"].rstrip("/"),
                        "wms_freight_tenant": config["freight"]["tenant"],
                        "freight_oms_url": config["oms"]["base_url"].rstrip("/"),
                        "freight_oms_tenant": config["oms"]["tenant"]},
            "Signed WMS/Freight recipient mapping does not select the same OMS authority")
    require({"orders:read", "orders:write"} <= set(manifest["oms_credential_scopes"]),
            "OMS write credential scope requires separate deployment attestation")
    return {"trust_key_sha256": fingerprint, "signed_evidence_sha256": evidence_digest,
            "evidence": "signed_release_operator_attestation",
            "limitation": "Public endpoints do not expose deployed image identity or private recipient mapping"}


def preflight(args):
    report = Report("read_only_native_preflight")
    report.data["not_performed"] = ["Mutating OMS/Freight lifecycle and idempotent replay", "Physical scanner and warehouse observation",
                                     "Database failover", "Independent verification of private deployment configuration"]
    context = {}

    def configuration():
        config = load(args.config)
        require(isinstance(config, dict) and config.get("schema_version") == 1, "Unsupported qualification configuration")
        require(config.get("environment") in {"staging", "production"}, "Explicit staging/production environment required")
        identifier(config.get("warehouse_id"))
        identifier(config.get("foreign_oms_tenant"))
        require(config["foreign_oms_tenant"] != config["oms"]["tenant"], "Foreign OMS tenant must differ from target")
        require(str(uuid.UUID(config["source_order_id"])) == config["source_order_id"], "Source order UUID must be canonical")
        clients = {}
        for name in ("wms", "oms", "freight"):
            service = config[name]
            identifier(service["tenant"])
            base = base_url(service["base_url"], args.allow_loopback)
            clients[name] = Client(base, "x-stateset-api-key" if name == "oms" else "Authorization", credential(service, "credential_env"))
        viewer = credential(config["wms"], "viewer_credential_env")
        foreign = credential(config["wms"], "foreign_credential_env")
        require(len({clients["wms"].token, viewer, foreign}) == 3, "Operator, viewer and foreign credentials must differ")
        context.update(config=config, clients=clients, viewer=viewer, foreign=foreign)
        report.data["environment"] = config["environment"]
        report.data["configuration_sha256"] = hashlib.sha256(json.dumps(config, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        report.data["transport_scope"] = "local_or_tunnel" if args.allow_loopback else "remote_https"
        return "Explicit recipients and environment-only credentials validated; no secrets retained"

    if not report.check("configuration", configuration):
        return report.finish()
    config, clients = context["config"], context["clients"]
    wms, oms, freight = clients["wms"], clients["oms"], clients["freight"]
    report.check("deployment_mapping_attestation", lambda: deployment_check(args, config))
    contract_path = f'/v1/tenants/{config["oms"]["tenant"]}/warehouse-fulfillments'

    def denied():
        for client, path in ((wms, "/v1/session"), (oms, contract_path), (freight, "/v1/access")):
            for token in (False, "invalid-qualification-" + secrets.token_hex(32)):
                require(client.get(path, token)[0] in {401, 403}, "Omitted/invalid credential was not denied")
        require(oms.get(f'/v1/tenants/{config["foreign_oms_tenant"]}/warehouse-fulfillments')[0] in {401, 403},
                "OMS credential was not denied by an existing foreign tenant")
        viewer = wms.json("/v1/session", context["viewer"])
        require(viewer["identity"]["tenant"] == config["wms"]["tenant"] and viewer["identity"]["role"] == "viewer",
                "Viewer credential does not select the expected tenant/role")
        require(wms.get("/v1/integrations/oms/orders?page=1", context["viewer"])[0] == 403,
                "Viewer can access operator-only native review")
        foreign = wms.json("/v1/session", context["foreign"])
        require(foreign["identity"]["tenant"] != config["wms"]["tenant"], "Foreign credential selects target tenant")
        # Warehouse names such as wh-main are deliberately tenant-local and
        # normally exist in both tenants. Test a positively resolved opaque
        # committed record, not the globally nonunique warehouse name.
        target = wms.json(f'/v1/warehouses/{config["warehouse_id"]}')
        candidates = [(kind, item.get("id")) for kind in ("orders", "receipts") for item in target.get(kind, [])]
        require(candidates, "Target warehouse needs one committed order/receipt for a read-only foreign-tenant proof")
        kind, record_id = candidates[0]
        require(isinstance(record_id, str) and str(uuid.UUID(record_id)) == record_id, "Target proof record requires a canonical UUID")
        path = f'/v1/warehouses/{config["warehouse_id"]}/{kind}/{record_id}'
        require(wms.json(path).get("id") == record_id, "Target proof record did not resolve under the selected operator")
        require(wms.get(path, context["foreign"])[0] == 404, "Foreign tenant resolved the target's committed record")
        return "Actual omitted/invalid/foreign credentials and viewer role checks denied"

    report.check("authentication_and_tenant_boundaries", denied)

    def session():
        require(wms.get("/ready", False)[0] == 200, "WMS database readiness failed")
        value = wms.json("/v1/session")
        require(value.get("demo") is False and value["identity"]["tenant"] == config["wms"]["tenant"]
                and value["identity"]["role"] in {"operator", "admin"}, "WMS requires real mode and correct operator tenant")
        native = value["native_integrations"]
        require(native["oms"]["configured"] is True and native["oms"]["shipment_writeback"] == "conditional_v1"
                and native["tms"]["configured"] is True, "Native recipients/conditional OMS mode are not enabled")
        warehouses = wms.json("/v1/warehouses")["warehouses"]
        require(any(item["warehouse"]["id"] == config["warehouse_id"] for item in warehouses), "Target warehouse not listed for operator")
        return "Real-mode operator, warehouse visibility, conditional OMS and Freight configuration confirmed"

    report.check("wms_real_mode_and_operator", session)

    def contract():
        value = oms.json(contract_path)["data"]
        require(value.get("contract") == CONTRACT and value.get("tenant_id") == config["oms"]["tenant"],
                "OMS contract or tenant identity mismatch")
        for capability in ("conditional_revision", "terminal_state_fence", "durable_idempotency", "durable_rejections"):
            require(value.get(capability) is True, "Migration-backed OMS capability missing")
        return "Migration-backed conditional revision, terminal fence, idempotency and rejection contract confirmed"

    report.check("oms_conditional_receiver", contract)

    def freight_access():
        value = freight.json("/v1/access")
        require(value.get("tenant_id") == config["freight"]["tenant"] and "operations.write" in value["access"]["capabilities"],
                "Freight credential selects wrong tenant or lacks operations.write")
        return "Actual Freight tenant and operations.write authorization confirmed"

    report.check("freight_tenant_and_permission", freight_access)

    def reconciliation():
        value = wms.json(f'/v1/warehouses/{config["warehouse_id"]}/reconciliation')
        require(value.get("ok") is True and value.get("method") == "full_append_only_ledger_and_order_line_allocations",
                "Full historical warehouse reconciliation failed or is unsupported")
        integer(value["version"], "reconciliation version")
        require(isinstance(value["rows"], list) and isinstance(value["receipts"], list)
                and all(row.get("ok") is True for row in value["rows"] + value["receipts"]), "A stock/receipt proof failed")
        return {"warehouse_version": value["version"], "stock_proofs": len(value["rows"]), "receipt_proofs": len(value["receipts"])}

    report.check("independent_full_reconciliation", reconciliation)

    def same_source():
        path = f'/v1/tenants/{config["oms"]["tenant"]}/orders/{config["source_order_id"]}'
        before = oms.json(path)["data"]
        source = order_projection(before)
        require(source["id"] == config["source_order_id"], "OMS returned a different source order")
        require(before.get("tenantId", config["oms"]["tenant"]) == config["oms"]["tenant"], "Canonical OMS source tenant differs")
        lists = (wms.json("/v1/integrations/oms/orders?page=1"), freight.json("/v1/sync/orders?page=1"))
        for page in lists:
            require(isinstance(page.get("items"), list) and len(page["items"]) <= 100, "Invalid native source list")
            matches = [item for item in page["items"] if item.get("id") == config["source_order_id"]]
            require(len(matches) == 1 and order_projection(matches[0]) == source,
                    "WMS/Freight do not return the same selected OMS revision and exact lines on page one")
        require(order_projection(oms.json(path)["data"]) == source, "OMS source changed during preflight; rerun after review")
        digest = hashlib.sha256(json.dumps(source, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        return {"source_projection_sha256": digest, "lines": len(source["lines"]),
                "limitation": "Matching observed content supplements signed mapping; it cannot prove private tenant configuration"}

    report.check("same_source_revision_and_exact_lines", same_source)
    return report.finish()


def facility_legacy(args):
    report = Report("signed_facility_acceptance_evidence")
    report.data["profile"] = "legacy-v1"
    report.data["qualified_scenarios"] = sorted(SCENARIOS)
    report.data["current_workflows_qualified"] = False
    report.data["not_performed"] = ["The validator does not observe physical work or establish witness identity independently"]

    def verify():
        evidence, fingerprint, evidence_digest = signed_file(args.evidence, args.signature, args.trust_key)
        require(evidence.get("performed") is True and evidence.get("synthetic") is False,
                "Physical acceptance has not been performed or uses synthetic fixtures")
        recent(evidence.get("observed_at"), hours=168)
        text(evidence.get("facility_id"), "facility identity")
        identifier(evidence.get("wms_tenant"))
        identifier(evidence.get("warehouse_id"))
        require(re.fullmatch(r"[0-9a-f]{40}", evidence.get("release_sha", "")), "Facility run requires an exact release SHA")
        require(evidence["facility_id"] == args.expected_facility and evidence["wms_tenant"] == args.expected_tenant
                and evidence["warehouse_id"] == args.expected_warehouse
                and evidence["release_sha"] == args.expected_release_sha,
                "Signed evidence belongs to a different facility, tenant, warehouse or release")
        witnesses = evidence["witnesses"]
        require(isinstance(witnesses, list) and len(witnesses) >= 2, "Two independent named facility witnesses required")
        names = [text(witness["name"], "witness name") for witness in witnesses]
        require(len(set(names)) == len(names) and {"operator", "inventory_controller"} <= {w["role"] for w in witnesses},
                "Distinct operator and inventory controller witnesses required")
        require(evidence.get("signed_by") in names, "Signer must be a named witness")
        scanner = evidence["scanner"]
        for key in ("make_model", "device_id", "firmware", "input_mode", "symbology", "label_stock", "printer_model"):
            text(scanner[key], "scanner/printed-label fact")
        require(scanner.get("physical_hardware") is True and scanner.get("printed_labels") is True,
                "Acceptance requires actual scanner hardware and printed labels")
        cases = evidence["scenarios"]
        require(isinstance(cases, list) and len(cases) == len(SCENARIOS)
                and {case["name"] for case in cases} == SCENARIOS, "Required facility scenario set is incomplete or duplicated")
        for case in cases:
            require(case.get("passed") is True, "A facility scenario did not pass")
            require(isinstance(case.get("request_ids"), list) and case["request_ids"], "Scenario lacks original request identities")
            for request_id in case["request_ids"]:
                text(request_id, "request identity")
            text(case.get("observation"), "literal operator observation")
            require(isinstance(case.get("artifact_ids"), list) and case["artifact_ids"], "Scenario lacks captured evidence artifacts")
        scans = evidence["scans"]
        require(isinstance(scans, list) and len(scans) >= 4, "Literal receipt/pick scans were not captured")
        for scan in scans:
            require(scan["operation"] in {"receive", "pick"}, "Unknown physical scan operation")
            for key in ("line_id", "bin_label", "sku_label", "lot_label", "request_id"):
                text(scan[key], "literal scan identity")
            integer(scan["quantity_each"], "scanned quantity", 1)
        for operation in ("receive", "pick"):
            rows = [scan for scan in scans if scan["operation"] == operation]
            require(any(a["sku_label"] == b["sku_label"] and a["line_id"] != b["line_id"]
                        for a in rows for b in rows), "Same-SKU independent lines were not physically verified")
        inventory = evidence["inventory"]
        require(isinstance(inventory, list) and inventory, "Start/end physical inventory evidence missing")
        seen = set()
        for row in inventory:
            identity = tuple(text(row[key], "inventory custody identity") for key in ("sku", "lot", "location"))
            require(identity not in seen, "Duplicate physical inventory custody row")
            seen.add(identity)
            values = {key: integer(row[key], key) for key in ("start_physical", "received", "moved_in", "moved_out", "shipped", "count_gain", "count_loss", "end_physical", "end_system")}
            expected = values["start_physical"] + values["received"] + values["moved_in"] - values["moved_out"] - values["shipped"] + values["count_gain"] - values["count_loss"]
            require(expected == values["end_physical"] == values["end_system"], "Physical stock equation or final system count differs")
        lost = evidence["lost_response"]
        text(lost["original_request_id"], "lost-response request")
        require(lost["retry_request_id"] == lost["original_request_id"] and integer(lost["committed_commands"], "replayed committed commands") == 1
                and integer(lost["physical_effects"], "replayed physical effects") == 1 and lost["replayed"] is True,
                "Lost-response proof does not preserve one original command/effect")
        peak = evidence["peak"]
        require(number(peak["observed_actions_per_second"], "observed peak", 0) >= number(peak["required_actions_per_second"], "required peak", 1), "Observed facility peak misses the declared requirement")
        require(number(peak["p95_ms"], "observed latency") <= number(peak["max_p95_ms"], "latency SLO", 1)
                and integer(peak["duration_seconds"], "peak duration", 300) >= 300
                and integer(peak["concurrent_operators"], "peak operators", 1) >= 1, "Sustained peak/latency evidence is insufficient")
        recovery = evidence["recovery"]
        require(number(recovery["actual_rto_seconds"], "actual RTO") <= number(recovery["max_rto_seconds"], "RTO target", 1)
                and number(recovery["actual_rpo_seconds"], "actual RPO") <= number(recovery["max_rpo_seconds"], "RPO target")
                and recovery["post_restore_reconciliation"] is True and recovery["post_backup_effects_reviewed"] is True,
                "Recovery target or post-backup physical/remote review failed")
        artifacts = evidence["artifacts"]
        require(isinstance(artifacts, list) and artifacts, "Evidence artifacts missing")
        artifact_ids = set()
        for artifact in artifacts:
            artifact_id = text(artifact["id"], "artifact identity")
            require(artifact_id not in artifact_ids, "Duplicate evidence artifact")
            artifact_ids.add(artifact_id)
            require(re.fullmatch(r"[0-9a-f]{64}", artifact["sha256"]), "Artifact content digest missing")
            local = Path(args.artifact_root) / artifact["path"]
            root = Path(args.artifact_root).resolve()
            resolved = local.resolve()
            require((resolved == root or root in resolved.parents) and local.is_file(), "Artifact path escapes evidence root or is missing")
            digest = hashlib.sha256()
            with local.open("rb") as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(chunk)
            require(digest.hexdigest() == artifact["sha256"], "Captured artifact digest differs from signed evidence")
        require(all(set(case["artifact_ids"]) <= artifact_ids for case in cases), "Scenario references an uncaptured artifact")
        return {"trust_key_sha256": fingerprint, "signed_evidence_sha256": evidence_digest,
                "evidence": "signed_facility_witness_observations",
                "scenarios": len(cases), "scans": len(scans), "inventory_rows": len(inventory), "artifacts_verified": len(artifacts),
                "limitation": "Signature authenticates the supplied key; trust and physical truth require independent facility ownership"}

    report.check("signed_physical_evidence", verify)
    return report.finish()



def facility(args):
    if getattr(args, "profile", "current-v2") == "legacy-v1":
        return facility_legacy(args)
    spec = importlib.util.spec_from_file_location("stateset_facility_evidence", Path(__file__).with_name("facility_evidence.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    context = SimpleNamespace(**{name: globals()[name] for name in ("Report", "require", "text", "identifier", "integer", "number", "stamp", "recent", "signed_file", "decode")})
    return module.run(args, context)

def write_report(path, report):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".qualification-", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as stream:
            json.dump(report, stream, indent=2, default=str)
            stream.write("\n")
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)
    native = subcommands.add_parser("preflight", help="GET-only checks against explicitly configured real services")
    native.add_argument("--config", required=True)
    native.add_argument("--deployment-manifest")
    native.add_argument("--deployment-signature")
    native.add_argument("--trust-key")
    native.add_argument("--allow-loopback", action="store_true")
    native.add_argument("--output", required=True)
    physical = subcommands.add_parser("facility", help="Validate signed facility observations and captured artifact bytes")
    physical.add_argument("--evidence", required=True)
    physical.add_argument("--signature")
    physical.add_argument("--trust-key")
    physical.add_argument("--artifact-root", required=True)
    physical.add_argument("--expected-facility", required=True)
    physical.add_argument("--expected-tenant", required=True)
    physical.add_argument("--expected-warehouse", required=True)
    physical.add_argument("--profile", choices=("current-v2", "legacy-v1"), default="current-v2")
    physical.add_argument("--expected-release-sha", help="Exact git SHA for the explicitly limited legacy-v1 profile")
    physical.add_argument("--qualified-release", help="Reviewed local software qualification bundle directory")
    physical.add_argument("--expected-release-summary-sha256")
    physical.add_argument("--expected-source-inputs-sha256")
    physical.add_argument("--expected-image-digest")
    physical.add_argument("--plan")
    physical.add_argument("--plan-signature")
    physical.add_argument("--plan-trust-key")
    physical.add_argument("--controller-signature")
    physical.add_argument("--controller-trust-key")
    physical.add_argument("--output", required=True)
    args = parser.parse_args()
    result = preflight(args) if args.command == "preflight" else facility(args)
    write_report(args.output, result)
    print(f'{result["scope"]}: {result["result"]}; production qualification remains separate')
    return 0 if result["result"] == "passed" else 2


if __name__ == "__main__":
    raise SystemExit(main())
