import { Empty, Section, Status } from "./WarehouseUi";
import { when } from "./format";
import type { Modal } from "./uiTypes";
import { AlertTriangle, ArrowRight, CheckCheck } from "lucide-react";
import type { HistoryKind, WarehouseState } from "./types";

/** Exceptions tab: open and resolved warehouse exceptions. */
export default function ExceptionsTab({
  canWork,
  disabled,
  filter,
  historyControl,
  matches,
  s,
  setModal,
}: {
  canWork: boolean;
  disabled: boolean;
  filter: string;
  historyControl: (kind: HistoryKind) => JSX.Element;
  matches: (...values: (string | undefined | null)[]) => boolean;
  s: WarehouseState;
  setModal: (next: Modal) => void;
}) {
  return (
    <Section
      title="Warehouse exceptions"
      detail="Resolution records preserve the issue, evidence, and responsible operator."
    >
      <div className="exception-list">
        {s.exceptions
          .filter(
            (e) =>
              (filter === "all" || e.status === filter) &&
              matches(e.category, e.detail, e.resolution),
          )
          .map((e) => (
            <article key={e.id}>
              <span className={`exception-symbol ${e.status}`}>
                <AlertTriangle size={20} />
              </span>
              <div className="exception-content">
                <div>
                  <h3>{e.category}</h3>
                  <Status value={e.status} />
                </div>
                <p>{e.detail}</p>
                <small>
                  {when(e.created_at)} · {e.actor}{" "}
                  {e.order_id && (
                    <>
                      ·{" "}
                      <button
                        className="table-link"
                        onClick={() => setModal({ order: e.order_id! })}
                      >
                        {s.orders.find((o) => o.id === e.order_id)?.reference ??
                          e.order_id}
                      </button>
                    </>
                  )}
                </small>
                {e.resolution && (
                  <div className="resolution">
                    <CheckCheck size={16} />
                    {e.resolution}
                    <small>{e.resolved_by}</small>
                  </div>
                )}
              </div>
              {e.status === "open" && canWork && (
                <button
                  className="btn"
                  disabled={disabled}
                  onClick={() =>
                    setModal({
                      title: "Resolve exception",
                      subtitle:
                        "Record what was checked or corrected. Resolution does not change inventory quantities.",
                      fields: [
                        {
                          name: "resolution",
                          label: "Resolution evidence",
                          type: "textarea",
                        },
                      ],
                      submit: "Resolve issue",
                      build: (d) => ({
                        type: "resolve_exception",
                        exception_id: e.id,
                        ...d,
                      }),
                    })
                  }
                >
                  Resolve <ArrowRight size={14} />
                </button>
              )}
            </article>
          ))}
        {!s.exceptions.length && (
          <Empty
            title="No warehouse exceptions"
            detail="Report observed issues here so they can be tracked and resolved."
          />
        )}
      </div>
      {historyControl("exceptions")}
    </Section>
  );
}
