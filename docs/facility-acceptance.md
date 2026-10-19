# Physical warehouse acceptance

The default `current-v2` gate validates signed captures for 23 current workflows against an independently pinned software release. It requires a plan signed before work and separate operator/controller signatures on the same final bytes. No physical acceptance has been performed by adding this validator, passing synthetic tests or passing local software qualification.

A passing result means the supplied evidence satisfies the checks below. `production_qualified` always remains false. Public keys must be obtained independently; signatures do not establish authority, prove a physical count, inspect hardware or query an external receiver. The independent controller owns truthful observations and completeness. [Native deployment preflight](native-acceptance.md) and [operations](operations.md) remain separate prerequisites.

## Plan before work

Use a controlled acceptance area with actual printed labels, deployed scanner hardware and independently counted stock. Keep signing keys, credentials and raw evidence outside the repository. Use one document per facility/warehouse; predeclare counterpart warehouse IDs and every controlled SKU/lot/location, including receiving and staging. Reconcile starting physical counts with system stock.

Obtain the reviewed local software qualification bundle. Independently pin SHA256 of `summary.json`, its complete software-input hash and immutable image digest. The complete software hash and image-build hash have distinct scopes; the gate checks each against its own provenance. The bundle must retain all ten passing stages, unchanged source manifests, matching image evidence and current migration/table identities.

Create blank templates offline:

```bash
python3 scripts/facility-template.py \
  --qualified-release /secure/reviewed-software-release \
  --expected-release-summary-sha256 REVIEWED_SUMMARY_SHA256 \
  --expected-source-inputs-sha256 REVIEWED_COMPLETE_SOFTWARE_SHA256 \
  --expected-image-digest sha256:REVIEWED_IMAGE_SHA256 \
  --expected-facility YOUR-FACILITY --expected-tenant YOUR-TENANT \
  --expected-warehouse YOUR-WAREHOUSE \
  --destination /secure/new-facility-run
```

The destination must not exist. Its mode is 0700; `plan.json` and `evidence.json` are 0600. Templates contain `performed:false`, `synthetic:true`, no observations and no passing scenarios. They deliberately fail. No keys or witness facts are generated.

Before work, fill the plan's `inventory_scope` entries `{sku,lot,location}`, counterpart IDs, `approved_at` and targets. Choose peak duration (300–86400 seconds), distinct concurrent operators (1–256), positive successful actions/second and maximum save-through-refresh p95 milliseconds. Choose positive RTO and nonnegative RPO seconds. Do not derive targets from a measured result. Approval follows software qualification and precedes the complete run.

```bash
openssl dgst -sha256 -sign /secure/plan-authority-private.pem \
  -out /secure/new-facility-run/plan.sig /secure/new-facility-run/plan.json
sha256sum /secure/new-facility-run/plan.json
```

Put that exact digest in `evidence.plan_sha256`. Preserve signed plan bytes. Changed scope, targets or release require a new plan approved before a new run.

## Observe all workflows

Record actual start/end/observation times, literal scanner facts and independent named witnesses with public-key DER SHA256 fingerprints. Supply every scenario exactly once with `passed`, literal `observation`, `artifact_ids` and applicable original `capture_ids`. Retain failures and uncertain requests alongside successes; a timeout or missing receipt never proves rejection.

| Scenario | Required observation and captured sources |
|---|---|
| `receiving_exact_lines` | Two independent same-SKU lines in one receipt; actual receiving and replay custody |
| `putaway` | Actual source/destination scans and physical movement |
| `allocation_same_sku_lines` | Exact line ownership without overselling |
| `picking_exact_lines` | Two independent same-SKU lines in one order; exact bin/lot and staging |
| `packing_handoff` | Original package tuples and actual carrier/handoff |
| `wrong_label_rejected` | Literal wrong label rejected with zero physical effect |
| `short_pick_return_release_recount` | Exception, unpick, reset/release, physical count, resolution and new allocation |
| `stale_operator_rescan` | Actual definitive 409, fresh review, new request and accepted literal scan |
| `lost_response_exact_replay` | Lost response after commit; exact original request retains one physical effect |
| `tenant_and_role_denial` | Actual viewer denial; foreign lookup of a known original order |
| `oms_conditional_handoff` | Original completed, rejected, uncertain and exact completed-replay native receipts |
| `tms_planned_load` | Original planned-load intent and exact replay; no invented pickup/delivery |
| `backup_restore_reconcile` | Actual archive, complete current table hashes and effect-cutoff review |
| `facility_selection_pending_save` | Device ID/code/city; uncertain warehouse and native saves prevent switching |
| `replenishment_actual_move` | Actual move matches original plan, quantity and dependencies |
| `receipt_return_remainder_cancellation` | Original return/cap, physical receive and cancelled unreceived demand |
| `partial_packages_backorders_returns` | Available allocation, partial package handoff and original return allowance |
| `partial_order_remainder_recovery` | Same order: unpack, unpick, release unpicked ownership, cancel remainder |
| `facility_transfer_custody` | Original dispatch and exact-line destination arrival; distinct unresolved transit |
| `lot_expiry_quarantine_recovery` | Policy/expiry/holds; stop forward work, unpack/unpick and reviewed release |
| `cross_facility_lot_trace` | Nine nonempty sections, original reviews/cursors, source flags and unchanged custody |
| `transfer_loss_reversal_arrival` | Destination administrator loss, capped original reversal and separate arrival |
| `reviewed_pack_units` | Original factors/targets, size change, disabled code and exact old-factor replay |

These are witness-attested scenarios with machine checks of retained source relationships. The gate does not simulate every physical/UI step or prove every label, expiry or carrier fact. Independently observe exact expiry boundaries and facility offsets, missing-label denial, earliest-expiry allocation, multiple holds, changed restrictions, partial return caps and conservative transfer controls where applicable. Serial identity and fractional measured goods remain unsupported.

## Capture formats and machine checks

Each artifact entry has `{id,kind,path,sha256}`. Paths are relative to `--artifact-root`. Absolute paths, `..`, symlink components, nonregular files and changed hashes fail. The validator consumes one byte snapshot per structured artifact: 32 MiB each, 128 MiB combined; each decoded JSON document is at most 2 MiB. Split large journals across unique artifacts. Peak NDJSON lines are limited to 8192 bytes. Archive/device capture bytes are streamed from one open descriptor without retention, with a 1 TiB per-file limit. Keep originals immutable during validation.

| Kind | Structure |
|---|---|
| `command_journal` | `{schema_version:1,records:[...]}` with original request/response and durable event/receipt |
| `native_receipts` | `{records:[{tenant,warehouse_id,actor,request_id,response}]}` with original native bodies |
| `trace_pages` | `{checked_at,before_custody,after_custody,pages:[{query,response}]}` with original API pages |
| `full_reconciliation` | Exactly one `{tenant,warehouse_id,response}` from final full reconciliation |
| `peak_samples` | NDJSON `{capture_id,start_elapsed_ms,duration_ms}` for confirmed original save/refresh actions |
| `recovery` | Exactly one original backup/incident/ready/cutoff observation, all table hashes and reconciliation |
| `deployment_observation` | Exactly one facility/tenant/warehouse observation with the complete `release` binding |
| `device_observations` | `{observations:[...]}` for pending switching, stale rescan and replenishment |
| `device_capture` | Original hardware/display bytes referenced by pending-switch observations |
| `archive` | Actual backup bytes referenced by recovery |
| `physical_notes` | Optional original controller counts and custody notes |

Never capture authorization headers, tokens or private keys. A release-matched offline helper fingerprints an exact retained command envelope without network/database access:

```bash
cargo run --locked --example request_fingerprint < /secure/original-request.json
```

Command records contain `capture_id`, `tenant`, `warehouse_id`, `actor`, authenticated `role`, `phase` (`baseline`/`performed`), `request`, `canonical_request_json`, `http_status`, `body`, `event` and `receipt`. Successful event/receipt data must come from durable original sources. Scope, actor, request ID, result, version, command and canonical digest must agree. Replays follow the original and preserve request/event/receipt bytes. Denied captures have null event/receipt. Uncertain responses cannot count as confirmed work; preserve them separately until resolved.

Baseline records establish history but cannot satisfy performed work, scans, timing or the current facility's pack size-change/disable trial. That trial also requires an exact original conversion replay after a size change. Successful work stays within the signed facility/counterpart set. Original main-facility stock changes stay within signed inventory scope.

Scans contain `operation` (`receive`/`pick`), `owner_id`, `line_id`, literal `bin_label`, `sku_label`, `lot_label`, `quantity_each`, `accepted`, `expected` labels and `review_capture_id`. Accepted scans name an original `capture_id`; rejected scans have null `capture_id` and integer `physical_effects:0`. Labels must join original line/bin/lot/quantity. Independent same-SKU lines share one original receipt for receiving and one order for picking.

Inventory rows identify `{sku,lot,location}` and nonnegative integer each columns:

```text
start_physical + received + moved_in - moved_out - shipped
  + transfer_in - transfer_out + count_gain - count_loss
  = end_physical = end_system
```

Every gross column is independently rebuilt from original performed event stock deltas; final counts join captured full reconciliation. Balanced omitted moves fail. Identities match signed scope exactly. The gate cannot discover unobserved physical stock elsewhere. `lost_response` names `original_capture_id`, `retry_capture_id` and observed `physical_effects:1`.

Trace custody rows are `{warehouse_id,sku,lot,location,on_hand,reserved}`. All nine sections retain the same SKU/lot/review/summary, original section/after/review query fields, advancing cursors, unique item warehouse/key identities and successful page/item sources. Section totals are complete and nonempty. Before/after physical/reserved totals agree with summary, and each page's observation time belongs to the run. Supplied captures do not establish manufacturer genealogy or a tenant-wide recall.

Device observation `kind` selects these fields:

* `pending_facility_switch`: `pending_kind` (`warehouse`/`native`), `pending_save_state:"uncertain"`, `switch_enabled:false`, retained `pending_request_id`, actual `warehouse_metadata:{id,code,city}`, displayed warehouse/code/city, `attempted_warehouse_id` and `device_capture_artifact_id`.
* `stale_rescan`: `denied_capture_id` and `fresh_capture_id`. The original 409 `command_outcome` must bind definitive noncommit to scope/actor/request/dependencies and changed version. Fresh work uses a new request and literal scan.
* `replenishment_move`: `move_capture_id` and original `plan:{warehouse_id,version,tasks}`. Its matching task retains `command`, `quantity` and `resource_versions`.

Native completed replay preserves frozen `evidence`, original `response` and `attempts`; its completed original must precede it. These checks do not contact the real receiver or prove an external effect.

`evidence.peak` supplies `started_at`, `duration_seconds`, `concurrent_operators`, reported `p95_ms` and `observed_actions_per_second`. Samples join distinct successful performed original commits within their measured interval (five-second clock tolerance). Each actor contributes in every ten-second bucket throughout the run. Intervals must show declared distinct actors active concurrently. Rate and nearest-rank p95 are recomputed. Witnesses own series/error completeness; omitted physical actions or unreported device failures cannot be discovered offline.

Recovery contains the complete `release` binding, `backup_at`, `incident_at`, `ready_at`, `last_durable_effect_at`, `last_restored_effect_at`, `source_table_hashes`, `restored_table_hashes`, `reconciliation`, `post_backup_effects_reviewed:true` and `archive_artifact_id`. Both table maps match and cover the exact qualified schema. Reconciliation includes successful lot/unit controls. RTO/RPO are recomputed with nanosecond precision. Witnesses must verify actual restored data and post-backup physical/remote effects; equal supplied fingerprints alone do not prove a restore occurred.

## Sign and verify

After actual work, fill truthful observations and performed/synthetic flags. Both facility-owned witness keys sign the same final evidence bytes. Independently trusted operator/controller keys and names must differ. Plan authority may be an authorized existing signer. Observations expire after seven days.

```bash
openssl dgst -sha256 -sign /secure/operator-private.pem \
  -out /secure/new-facility-run/operator.sig /secure/new-facility-run/evidence.json
openssl dgst -sha256 -sign /secure/controller-private.pem \
  -out /secure/new-facility-run/controller.sig /secure/new-facility-run/evidence.json
python3 scripts/qualify-native.py facility \
  --qualified-release /secure/reviewed-software-release \
  --expected-release-summary-sha256 REVIEWED_SUMMARY_SHA256 \
  --expected-source-inputs-sha256 REVIEWED_COMPLETE_SOFTWARE_SHA256 \
  --expected-image-digest sha256:REVIEWED_IMAGE_SHA256 \
  --plan /secure/new-facility-run/plan.json \
  --plan-signature /secure/new-facility-run/plan.sig --plan-trust-key /secure/trust/plan-public.pem \
  --evidence /secure/new-facility-run/evidence.json \
  --signature /secure/new-facility-run/operator.sig --trust-key /secure/trust/operator-public.pem \
  --controller-signature /secure/new-facility-run/controller.sig --controller-trust-key /secure/trust/controller-public.pem \
  --artifact-root /secure/new-facility-run/artifacts \
  --expected-facility YOUR-FACILITY --expected-tenant YOUR-TENANT --expected-warehouse YOUR-WAREHOUSE \
  --output /secure/new-facility-run/validation.json
```

Missing, malformed, failed, changed or untrusted supplied input produces `unqualified` and exit 2; passing exits 0. Reports are written atomically with mode 0600 and contain digests/check summaries without raw labels or keys. Retain private original evidence; publish only approved sanitized summaries. Repeat after release, hardware, label or mapping changes.

Explicit `--profile legacy-v1` supports [historical thirteen-scenario evidence](facility-acceptance-legacy.md). Its limited report cannot qualify current workflows and is never selected automatically.
