#!/usr/bin/env python3
"""Create unperformed, release-bound facility plan/evidence templates offline."""
import argparse
import importlib.util
import json
import os
from pathlib import Path


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def create(args):
    directory = Path(__file__).resolve().parent
    q = load_module("facility_template_qualification", directory / "qualify-native.py")
    v = load_module("facility_template_evidence", directory / "facility_evidence.py")
    try:
        binding = v.release(q, args)
        for value in (args.expected_facility, args.expected_tenant, args.expected_warehouse): q.text(value, "planned facility identity")
    except q.Unqualified: raise ValueError("Reviewed qualification or planned facility identity is invalid") from None
    scope = dict(facility_id=args.expected_facility, wms_tenant=args.expected_tenant, warehouse_id=args.expected_warehouse)
    plan = dict(schema_version=2, profile=v.PROFILE, **scope, release=binding, approved_at=None,
                scenarios=sorted(v.CURRENT_SCENARIOS), counterpart_warehouse_ids=[], inventory_scope=[],
                targets=dict(peak=dict(duration_seconds=300, concurrent_operators=0, required_actions_per_second=0, max_p95_ms=0),
                             recovery=dict(max_rto_seconds=0, max_rpo_seconds=0)))
    evidence = dict(schema_version=2, profile=v.PROFILE, performed=False, synthetic=True, **scope, release=binding,
                    plan_sha256=None, started_at=None, ended_at=None, observed_at=None,
                    witnesses=[dict(role=role, name=None, key_sha256=None) for role in ("operator", "inventory_controller")],
                    scanner=dict(physical_hardware=False, printed_labels=False, **dict.fromkeys(("make_model", "device_id", "firmware", "input_mode", "symbology", "label_stock", "printer_model"))),
                    scenarios=[dict(name=name, passed=False, observation=None, artifact_ids=[], capture_ids=[]) for name in sorted(v.CURRENT_SCENARIOS)],
                    scans=[], inventory=[], lost_response=dict(original_capture_id=None, retry_capture_id=None, physical_effects=0),
                    peak=dict(started_at=None, duration_seconds=0, concurrent_operators=0, p95_ms=0, observed_actions_per_second=0), artifacts=[])
    # Never overwrite a prior plan, signature or observation. A new directory
    # and exclusive 0600 creates also keep unpublished facility data private.
    destination = Path(args.destination)
    destination.mkdir(mode=0o700)
    for name, value in (("plan.json", plan), ("evidence.json", evidence)):
        fd = os.open(destination / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w") as stream: json.dump(value, stream, indent=2); stream.write("\n")
    return destination


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("qualified-release", "expected-release-summary-sha256", "expected-source-inputs-sha256", "expected-image-digest", "expected-facility", "expected-tenant", "expected-warehouse", "destination"):
        parser.add_argument("--" + name, required=True)
    args = parser.parse_args()
    try: destination = create(args)
    except (OSError, ValueError, KeyError, TypeError, AttributeError):
        parser.exit(2, "Templates were not created: invalid release pins, unavailable input, or destination already exists.\n")
    print("Created unperformed plan.json and evidence.json; choose targets and scope before signing the plan.")
    return 0


if __name__ == "__main__": raise SystemExit(main())
