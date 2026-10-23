import type { WarehouseState } from "./types";

/** The full reconciliation report returned by the API. */
export type ReconciliationProof = {
  ok: boolean;
  rows: Record<string, unknown>[];
  receipts?: Record<string, unknown>[];
  shipments?: Record<string, unknown>[];
  transfers?: Record<string, unknown>[];
  packages?: Record<string, unknown>[];
  version: number;
  method?: string;
  units?: {
    ok: boolean;
    definition_changes: number;
    quantity_observations: number;
    method: string;
  };
  lot_controls?: {
    ok: boolean;
    baseline_version: number;
    policy_changes: number;
    eligibility_decisions: number;
  };
};

/**
 * What the dashboard may claim about a full reconciliation report: it
 * belongs to this warehouse and version, was not interrupted, and every
 * required section (unit packs, lot controls) is present and well formed.
 */
export function deriveProofView(
  s: WarehouseState,
  fullReconciliation: { warehouse: string; proof: ReconciliationProof } | null,
  fullReconciliationBusy: boolean,
  fullReconciliationError: string,
) {
  const fullProof =
    fullReconciliation?.warehouse === s.warehouse.id
      ? fullReconciliation.proof
      : null;
  const fullProofReady =
    !!fullProof &&
    fullProof.version === s.version &&
    !fullReconciliationBusy &&
    !fullReconciliationError;
  const qualityProof = fullProof?.lot_controls;
  const qualityProofRequired =
    !!s.lot_controls?.length || s.products.some((p) => p.expiry_required);
  const qualityProofValid =
    !!qualityProof &&
    typeof qualityProof.ok === "boolean" &&
    [
      qualityProof.baseline_version,
      qualityProof.policy_changes,
      qualityProof.eligibility_decisions,
    ].every((v) => Number.isSafeInteger(v) && v >= 0) &&
    qualityProof.baseline_version <= (fullProof?.version ?? -1);
  const unitProof = fullProof?.units;
  const unitProofRequired = !!s.unit_packs?.length;
  const unitProofValid =
    !!unitProof &&
    typeof unitProof.ok === "boolean" &&
    [unitProof.definition_changes, unitProof.quantity_observations].every(
      (n) => Number.isSafeInteger(n) && n >= 0,
    ) &&
    typeof unitProof.method === "string";
  const fullProofOk =
    fullProofReady &&
    fullProof.ok &&
    ((unitProof === undefined && !unitProofRequired) ||
      (unitProofValid && unitProof?.ok)) &&
    ((qualityProof === undefined && !qualityProofRequired) ||
      (qualityProofValid && qualityProof?.ok));
  const shipmentDifferences =
    fullProofReady && Array.isArray(fullProof.shipments)
      ? fullProof.shipments.filter((proof) => proof?.ok !== true)
      : [];
  const transferDifferences =
    fullProofReady && Array.isArray(fullProof.transfers)
      ? fullProof.transfers.filter((p) => p?.ok !== true)
      : [];
  const packageDifferences =
    fullProofReady && Array.isArray(fullProof.packages)
      ? fullProof.packages.filter((p) => p?.ok !== true)
      : [];
  return {
    fullProof,
    fullProofReady,
    qualityProof,
    qualityProofRequired,
    qualityProofValid,
    unitProof,
    unitProofRequired,
    unitProofValid,
    fullProofOk,
    shipmentDifferences,
    transferDifferences,
    packageDifferences,
  };
}

export type ProofView = ReturnType<typeof deriveProofView>;
