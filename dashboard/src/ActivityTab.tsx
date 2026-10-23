import { Badge, Empty, Section, label } from "./WarehouseUi";
import { Download, RefreshCw, ShieldCheck } from "lucide-react";
import type { Activity, HistoryKind, WarehouseState } from "./types";
import type { ProofView } from "./proofView";
import { fmt, when } from "./format";

/** Movement ledger tab: full reconciliation status and movement history. */
export default function ActivityTab({
  s,
  proof,
  fullReconciliationBusy,
  fullReconciliationError,
  runFullReconciliation,
  filter,
  matches,
  historyControl,
}: {
  s: WarehouseState;
  proof: ProofView;
  fullReconciliationBusy: boolean;
  fullReconciliationError: string;
  runFullReconciliation: (warehouse: string) => Promise<unknown> | void;
  filter: string;
  matches: (...values: (string | undefined | null)[]) => boolean;
  historyControl: (kind: HistoryKind) => JSX.Element;
}) {
  const {
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
  } = proof;
  return (
    <>
      <div className={`reconciliation-banner ${fullProofOk ? "" : "failed"}`}>
        <ShieldCheck size={22} />
        <div>
          <strong>
            {fullReconciliationBusy
              ? "Checking full inventory history…"
              : fullReconciliationError
                ? "Full reconciliation unavailable"
                : fullProofReady
                  ? fullProofOk
                    ? "Inventory ledger reconciles"
                    : "Full reconciliation needs review"
                  : "Full reconciliation needs refresh"}
          </strong>
          <p>
            {fullProofReady
              ? fullProofOk
                ? `All physical balances and receipt line totals match the complete movement history. Reservations match order-line allocations. Independently checked at version ${fullProof?.version}.`
                : `Differences were found while rebuilding the complete movement history, receipt totals, and reservations at version ${fullProof?.version}. Review the exported proof before continuing.`
              : "This check rebuilds physical balances, received line totals, and reservations from the complete recorded history."}
          </p>
          {fullReconciliationError && (
            <p role="alert">{fullReconciliationError}</p>
          )}
          {fullProofReady &&
            (qualityProofRequired || qualityProof !== undefined) &&
            (qualityProofValid && qualityProof?.ok ? (
              <p>
                Lot control history reconciles. Current quarantine and expiry
                retain the original handoff facts.
              </p>
            ) : (
              <p role="alert">
                Lot policy or original eligibility evidence needs review. Export
                the proof and reconcile the affected work before further
                fulfillment.
              </p>
            ))}
          {fullProofReady &&
            (unitProofRequired || unitProof !== undefined) &&
            (!unitProofValid || unitProof?.ok !== true) && (
              <p className="alert warning">
                Original pack definitions or quantity observations disagree.
                Hold work and review the source records before recording further
                inventory changes.
              </p>
            )}
          {packageDifferences.length > 0 && (
            <p role="alert">
              Shipment package evidence needs review for{" "}
              {packageDifferences.length} order(s). Export the proof and
              reconcile original packing and handoff records.
            </p>
          )}
          {transferDifferences.length > 0 && (
            <p role="alert">
              Transfer evidence needs review for {transferDifferences.length}{" "}
              handoff(s). Reconcile original dispatch and arrival records in the
              exported proof before receiving.
            </p>
          )}
          {shipmentDifferences.length > 0 && (
            <p role="alert">
              Shipment evidence needs review for {shipmentDifferences.length}{" "}
              order
              {shipmentDifferences.length === 1 ? "" : "s"}. Export the proof
              and reconcile the original handoff before authorizing returns.
            </p>
          )}
        </div>
        <button
          className="btn"
          disabled={fullReconciliationBusy}
          onClick={() => void runFullReconciliation(s.warehouse.id)}
        >
          <RefreshCw
            size={16}
            className={fullReconciliationBusy ? "spin" : ""}
          />
          Run full reconciliation
        </button>
        <button
          className="btn"
          disabled={!fullProofReady}
          onClick={() => {
            if (!fullProofReady || !fullProof) return;
            const url = URL.createObjectURL(
              new Blob([JSON.stringify(fullProof, null, 2)], {
                type: "application/json",
              }),
            );
            const link = document.createElement("a");
            link.href = url;
            link.download = `warehouse-reconciliation-v${fullProof.version}.json`;
            link.click();
            URL.revokeObjectURL(url);
          }}
        >
          <Download size={16} />
          Export proof
        </button>
      </div>
      <Section
        title="Movement ledger"
        detail={`${s.movements.length} movements · newest first`}
      >
        <div className="table-scroll">
          <table>
            <thead>
              <tr>
                <th>Movement</th>
                <th>SKU / lot</th>
                <th>From → to</th>
                <th>Units</th>
                <th>Reference / reason</th>
                <th>Actor / time</th>
              </tr>
            </thead>
            <tbody>
              {s.movements
                .filter(
                  (m) =>
                    (filter === "all" || m.kind === filter) &&
                    matches(
                      m.sku,
                      m.lot,
                      m.reference,
                      m.reason,
                      m.actor,
                      m.from,
                      m.to,
                    ),
                )
                .slice()
                .reverse()
                .map((m) => (
                  <tr key={m.id}>
                    <td>
                      <Badge tone={m.kind === "shipment" ? "blue" : "green"}>
                        {m.kind}
                      </Badge>
                      <span className="subline mono">{m.id.slice(0, 8)}</span>
                    </td>
                    <td>
                      <strong className="mono">{m.sku}</strong>
                      <span className="subline">{m.lot}</span>
                    </td>
                    <td>
                      {m.from ?? "Supplier / adjustment"}
                      <span className="subline">
                        → {m.to ?? "Carrier / adjustment"}
                      </span>
                    </td>
                    <td className="mono strong">{fmt(m.quantity)}</td>
                    <td>
                      {m.reference}
                      <span className="subline">{m.reason}</span>
                      {m.receipt_line_id && (
                        <span className="subline">
                          Receipt line {m.receipt_line_id}
                        </span>
                      )}
                    </td>
                    <td>
                      {m.actor}
                      <span className="subline">{when(m.at)}</span>
                    </td>
                  </tr>
                ))}
            </tbody>
          </table>
          {!s.movements.length && (
            <Empty
              title="No movements recorded"
              detail="Receipts, transfers, picks, counts and shipments appear here."
            />
          )}
        </div>
        {historyControl("movements")}
      </Section>
      <Section
        title="Command audit"
        detail="Recorded warehouse actions, reasons, and responsible operators."
      >
        <div className="table-scroll">
          <table>
            <thead>
              <tr>
                <th>Action</th>
                <th>Reference / evidence</th>
                <th>Actor / time</th>
              </tr>
            </thead>
            <tbody>
              {s.activity
                .slice()
                .reverse()
                .filter((item: Activity) =>
                  matches(item.kind, item.reference, item.detail, item.actor),
                )
                .map((item: Activity) => (
                  <tr key={item.id}>
                    <td>{label(item.kind)}</td>
                    <td>
                      {item.reference}
                      <span className="subline">{item.detail}</span>
                    </td>
                    <td>
                      {item.actor}
                      <span className="subline">{when(item.at)}</span>
                    </td>
                  </tr>
                ))}
            </tbody>
          </table>
        </div>
        {historyControl("activity")}
      </Section>
    </>
  );
}
