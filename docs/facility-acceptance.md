# Physical warehouse acceptance

Facility acceptance requires observations from an actual operator and independent inventory controller using the deployed release, scanner hardware, printed labels and physical stock. Automated browser input and synthetic database fixtures do not supply those observations. No facility acceptance has been performed merely by adding this runbook or passing its validator.

`scripts/qualify-native.py facility` validates signed witness evidence, recomputes custody equations and verifies the actual captured artifact bytes. A missing signature, missing physical fact, synthetic run, failed scenario, changed artifact or missed declared service target produces `unqualified` and exit 2. A passing document yields `passed` only for the supplied signed evidence. `production_qualified` remains false: key trust, truthful observations and the deployment release decision belong to the facility owner.

## Run and capture

Use a controlled acceptance area and reconcile its starting physical counts against system stock before work. Record the exact warehouse, deployed source SHA/image, time, operator and inventory controller; record scanner make/model/device/firmware/input mode, symbology, label stock and printer. Choose expected peak action rate, p95 latency, concurrent operators, RTO and RPO before the run. Do not derive a passing target from a result already measured.

Keep the original label values and exact receipt/allocation line IDs. For each receive/pick, capture bin, SKU, explicit lot, whole-each quantity, selected line and original request ID. Include two independent receipt lines and two independent picked lines sharing one SKU. Have the controller observe physical custody and count independently; a screenshot of system stock alone is not a physical count.

| Required scenario name | Acceptance observation |
|---|---|
| `receiving_exact_lines` | Receive same-SKU lines independently; a replay adds no second physical receipt |
| `putaway` | Scan actual source/destination labels and count stock moved into storage |
| `allocation_same_sku_lines` | Exact line owners reserve available stock without overselling |
| `picking_exact_lines` | Scan actual bin/SKU/lot; staged quantities belong to the selected same-SKU line |
| `packing_handoff` | Complete packing and record an actual carrier/handoff reference and time |
| `wrong_label_rejected` | Wrong bin/SKU/lot is denied; no stock, receipt or reservation movement occurs |
| `short_pick_return_release_recount` | Block work, return staged units, release, physically recount, resolve and allocate again |
| `stale_operator_rescan` | A second operator changes stock after labels are captured; confirmation requires reload/rescan |
| `lost_response_exact_replay` | Interrupt response after commit, reload and retry one original request; one committed command and physical effect remain |
| `tenant_and_role_denial` | Actual viewer/foreign credentials cannot perform or resolve unauthorized work |
| `oms_conditional_handoff` | Deployed conditional receiver confirms exact immutable shipment identity; rejected and uncertain recovery behave as documented |
| `tms_planned_load` | One intended planned load is created and replayed without a second load or invented pickup/delivery |
| `backup_restore_reconcile` | Restore an actual backup, reconcile physical/remote effects after its cutoff, and meet predeclared RTO/RPO |

Capture at least five continuous minutes at the declared operator concurrency/peak rate; retain original timing samples and errors, not just a p95 number. Restore to an isolated target before any cutover and review physical/remote effects occurring after the backup cutoff. The [native deployment preflight](native-acceptance.md) and [operations runbook](operations.md) provide the service and recovery prerequisites.

## Evidence document

Keep evidence, artifacts and signing material outside the repository. This is the required shape; `performed: false` and `synthetic: true` intentionally prevent this template from passing. Replace values only after recording actual observations. The full required scenario set above must appear exactly once. `observation` contains a literal observed result, and every scenario references at least one captured artifact.

```json
{
  "schema_version": 1,
  "performed": false,
  "synthetic": true,
  "observed_at": "2026-10-17T16:00:00Z",
  "facility_id": "REPLACE-AFTER-ACTUAL-RUN",
  "wms_tenant": "warehouse-tenant",
  "warehouse_id": "wh-main",
  "release_sha": "0000000000000000000000000000000000000000",
  "signed_by": "Named operator",
  "witnesses": [
    {"name": "Named operator", "role": "operator"},
    {"name": "Independent controller", "role": "inventory_controller"}
  ],
  "scanner": {
    "make_model": "RECORD", "device_id": "RECORD", "firmware": "RECORD",
    "input_mode": "RECORD", "symbology": "RECORD", "label_stock": "RECORD",
    "printer_model": "RECORD", "physical_hardware": false, "printed_labels": false
  },
  "scenarios": [
    {
      "name": "receiving_exact_lines", "passed": false,
      "request_ids": ["original-request-id"],
      "observation": "Record actual observation", "artifact_ids": ["controller-notes"]
    }
  ],
  "scans": [
    {
      "operation": "receive", "line_id": "exact-receipt-line-id",
      "bin_label": "literal-bin-code", "sku_label": "literal-SKU-code",
      "lot_label": "literal-lot-code", "quantity_each": 1,
      "request_id": "original-request-id"
    }
  ],
  "inventory": [
    {
      "sku": "literal-SKU-code", "lot": "literal-lot-code", "location": "literal-bin-code",
      "start_physical": 0, "received": 0, "moved_in": 0, "moved_out": 0,
      "shipped": 0, "count_gain": 0, "count_loss": 0,
      "end_physical": 0, "end_system": 0
    }
  ],
  "lost_response": {
    "original_request_id": "original-request-id", "retry_request_id": "original-request-id",
    "committed_commands": 0, "physical_effects": 0, "replayed": false
  },
  "peak": {
    "required_actions_per_second": 1, "observed_actions_per_second": 0,
    "max_p95_ms": 1000, "p95_ms": 0, "duration_seconds": 0, "concurrent_operators": 1
  },
  "recovery": {
    "max_rto_seconds": 60, "actual_rto_seconds": 0,
    "max_rpo_seconds": 0, "actual_rpo_seconds": 0,
    "post_restore_reconciliation": false, "post_backup_effects_reviewed": false
  },
  "artifacts": [
    {
      "id": "controller-notes", "path": "notes/controller-notes.txt",
      "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
    }
  ]
}
```

Every inventory custody row must independently satisfy:

```text
start_physical + received + moved_in - moved_out - shipped
  + count_gain - count_loss = end_physical = end_system
```

All values are nonnegative integer each quantities; gains/losses have explicit separate columns. Supply every affected SKU/lot/location and preserve the independent physical count notes and system ledger exports. The validator checks each supplied row and rejects duplicate custody identities. It cannot discover an omitted physical location or infer whether a signed count is truthful; the independent controller owns completeness and accuracy.

The document requires all thirteen scenarios; at least four literal scans with independent same-SKU lines for both receiving and picking; one original lost-response identity and a single command/effect; a sustained peak meeting declared throughput/latency; and recovery meeting declared RTO/RPO with reviewed post-backup effects. Observations expire after seven days for this acceptance run. Requalify changes to hardware, label formats, tenant mappings or deployed releases using a new signed document.

## Sign and verify captured bytes

Use an existing facility-owned RSA/ECDSA signing key held by the named witness. The controller obtains its public key through the facility trust process, independently of the evidence bundle. Signing with an arbitrary newly generated key does not establish facility authority. A single signature authenticates the witness's complete statement, including the second named controller's observation; organizations requiring separate countersignatures should retain those artifacts as well.

```bash
openssl dgst -sha256 -sign /secure/facility-signing-private.pem \
  -out /secure/acceptance/facility.sig /secure/acceptance/facility.json
python3 scripts/qualify-native.py facility \
  --evidence /secure/acceptance/facility.json \
  --signature /secure/acceptance/facility.sig \
  --trust-key /secure/trust/facility-signing-public.pem \
  --artifact-root /secure/acceptance/artifacts \
  --expected-facility YOUR-ACTUAL-FACILITY \
  --expected-tenant warehouse-tenant \
  --expected-warehouse wh-main \
  --expected-release-sha ACTUAL-REVIEWED-40-CHARACTER-RELEASE-SHA \
  --output /secure/acceptance/validation.json
```

The caller pins the expected facility, WMS tenant, warehouse and reviewed release separately from the evidence file; a valid signed run for another scope fails. Artifact paths resolve beneath `--artifact-root`; missing files, path escapes and SHA256 mismatches fail. Hash original exported bytes before signing; edits require a new digest and signature. The validator creates a mode-0600 JSON result atomically, binds its result to the exact signed document digest, and retains no raw label values, customer details, credentials or signing keys. Retain the full private acceptance bundle under the facility's evidence policy and publish only an approved sanitized summary.
