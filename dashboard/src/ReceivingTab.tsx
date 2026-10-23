import { Empty, Section, Status } from "./WarehouseUi";
import { PutawayPanel } from "./PlanningPanels";
import { fmt, when } from "./format";
import type { Modal, RequestJson, SubmitCommand, Totals } from "./uiTypes";
import { ChevronRight } from "lucide-react";
import type { HistoryKind, WarehouseState } from "./types";

/** Receiving tab: expected receipts, receiving stock and returns. */
export default function ReceivingTab({
  canWork,
  disabled,
  filter,
  historyControl,
  matches,
  request,
  s,
  setModal,
  submit,
  totals,
}: {
  canWork: boolean;
  disabled: boolean;
  filter: string;
  historyControl: (kind: HistoryKind) => JSX.Element;
  matches: (...values: (string | undefined | null)[]) => boolean;
  request: RequestJson;
  s: WarehouseState;
  setModal: (next: Modal) => void;
  submit: SubmitCommand;
  totals: Totals;
}) {
  return (
    <>
      {canWork && (
        <PutawayPanel
          key={`putaway-${s.warehouse.id}`}
          state={s}
          disabled={disabled}
          request={request}
          onSubmit={submit}
        />
      )}
      <div className="inventory-summary">
        <span>{totals.receiving} inbound receipts open</span>
        <span>
          {fmt(
            s.stocks
              .filter((b) =>
                s.locations.some(
                  (l) => l.code === b.location && l.kind === "receiving",
                ),
              )
              .reduce((n, b) => n + b.on_hand, 0),
          )}{" "}
          units awaiting putaway
        </span>
      </div>
      <Section
        title="Inbound receipts"
        detail="Receive against an expected line, then move the goods to storage."
      >
        <div className="table-scroll">
          <table>
            <thead>
              <tr>
                <th>Receipt / supplier</th>
                <th>Lines</th>
                <th>Received / expected</th>
                <th>Status</th>
                <th>Created</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {s.receipts
                .filter(
                  (r) =>
                    (filter === "all" || r.status === filter) &&
                    matches(r.reference, r.supplier),
                )
                .slice()
                .reverse()
                .map((r) => (
                  <tr key={r.id}>
                    <td>
                      <button
                        className="table-link"
                        onClick={() => setModal({ receipt: r.id })}
                      >
                        {r.reference}
                      </button>
                      <span className="subline">{r.supplier}</span>
                    </td>
                    <td>{r.lines.length}</td>
                    <td>
                      <span className="mono">
                        {fmt(r.lines.reduce((n, l) => n + l.received, 0))}{" "}
                        <span className="muted-text">
                          / {fmt(r.lines.reduce((n, l) => n + l.quantity, 0))}
                        </span>
                      </span>
                      <div className="progress-track">
                        <span
                          style={{
                            width: `${
                              (100 *
                                r.lines.reduce((n, l) => n + l.received, 0)) /
                              r.lines.reduce((n, l) => n + l.quantity, 0)
                            }%`,
                          }}
                        />
                      </div>
                    </td>
                    <td>
                      <Status value={r.status} />
                    </td>
                    <td className="muted-text">{when(r.created_at)}</td>
                    <td>
                      <button
                        className="mini-btn"
                        onClick={() => setModal({ receipt: r.id })}
                      >
                        {["received", "cancelled"].includes(r.status)
                          ? "View receipt"
                          : "Receive goods"}
                        <ChevronRight size={14} />
                      </button>
                    </td>
                  </tr>
                ))}
            </tbody>
          </table>
          {!s.receipts.length && (
            <Empty
              title="Your receiving dock is clear"
              detail="Schedule an inbound receipt to begin."
            />
          )}
        </div>
        {historyControl("receipts")}
      </Section>
    </>
  );
}
