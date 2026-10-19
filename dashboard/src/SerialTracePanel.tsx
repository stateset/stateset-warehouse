import { useEffect, useRef, useState } from "react";
import type { WarehouseState } from "./types";
import { serialText } from "./serialInputs";
type View = "current" | "history" | "observations";
type Page = {
  schema_version: number;
  version_scope: string;
  sku: string;
  serial: string | null;
  warehouse_id: string | null;
  view: View;
  review: number;
  items: any[];
  next_cursor: string | null;
  total_records: number;
  page_source_ok: boolean;
  current_source_ok: boolean | null;
};
function place(p: any): string {
  if (!p) return "Not previously registered";
  if (p.status === "in_transit" || p.status === "lost")
    return `${p.status === "lost" ? "Lost" : "In transit"} · ${p.source_warehouse_id} → ${p.destination_warehouse_id} · transfer ${p.transfer_id} / ${p.line_id}${p.loss_id ? ` · loss ${p.loss_id}` : ""}`;
  return `${String(p.status).replaceAll("_", " ")} · ${p.warehouse_id}${p.location ? ` / ${p.location}` : ""}${p.order_id ? ` · order ${p.order_id} / ${p.line_id}` : ""}${p.package_id ? ` · package ${p.package_id}` : ""}${p.return_receipt_id ? ` · RMA ${p.return_receipt_id}` : ""}`;
}
export default function SerialTracePanel({
  state,
  request,
}: {
  state: WarehouseState;
  request: (path: string) => Promise<any>;
}) {
  const products = state.products.filter((p) => p.serial_controlled === true);
  const [sku, setSku] = useState(products[0]?.sku ?? ""),
    [serial, setSerial] = useState(""),
    [view, setView] = useState<View>("current"),
    [local, setLocal] = useState(false);
  const firstSku = products[0]?.sku ?? "";
  useEffect(() => {
    if (!sku && firstSku) setSku(firstSku);
  }, [sku, firstSku]);
  const [page, setPage] = useState<Page | null>(null),
    [rows, setRows] = useState<any[]>([]),
    [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    [held, setHeld] = useState(false),
    [allSources, setAllSources] = useState(true);
  const sequence = useRef(0),
    alive = useRef(true);
  const seen = useRef(new Set<string>()),
    cursors = useRef(new Set<string>());
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      ++sequence.current;
    };
  }, []);
  const reset = () => {
    ++sequence.current;
    setPage(null);
    setRows([]);
    setError("");
    setHeld(false);
    setBusy(false);
    seen.current.clear();
    cursors.current.clear();
  };
  // A newly observed warehouse version invalidates the displayed review. Other
  // facilities are fenced by the global review when the next page is requested.
  useEffect(() => {
    reset();
  }, [state.version]);
  const load = async (next = false) => {
    if (
      busy ||
      !serialText(sku) ||
      (serial !== "" && !serialText(serial)) ||
      (view === "history" && !serial)
    )
      return;
    const current = ++sequence.current;
    setBusy(true);
    setError("");
    if (!next) {
      setRows([]);
      setPage(null);
      setHeld(false);
      seen.current.clear();
      cursors.current.clear();
      setAllSources(true);
    }
    const q = new URLSearchParams({
      sku,
      limit: String(Math.min(50, 1000 - (next ? rows.length : 0))),
    });
    if (serial) q.set("serial", serial);
    if (local) q.set("warehouse_id", state.warehouse.id);
    if (next && page?.next_cursor) {
      q.set("review", String(page.review));
      q.set("after", page.next_cursor);
    }
    try {
      const p: Page = await request(
        `/v1/${view === "current" ? "serials" : view === "history" ? "serial-history" : "serial-observations"}?${q}`,
      );
      if (!alive.current || current !== sequence.current) return;
      if (
        p.schema_version !== 1 ||
        p.version_scope !== "tenant_sku_identity" ||
        p.sku !== sku ||
        p.serial !== (serial || null) ||
        p.warehouse_id !== (local ? state.warehouse.id : null) ||
        p.view !== view ||
        !Number.isSafeInteger(p.review) ||
        p.review < 0 ||
        !Number.isSafeInteger(p.total_records) ||
        p.total_records < 0 ||
        !Array.isArray(p.items) ||
        p.items.length > 50 ||
        typeof p.page_source_ok !== "boolean" ||
        ![null, true, false].includes(p.current_source_ok) ||
        (p.next_cursor !== null &&
          (typeof p.next_cursor !== "string" ||
            p.next_cursor.length === 0 ||
            p.next_cursor.length > 4096)) ||
        (next &&
          (p.review !== page?.review ||
            p.total_records !== page?.total_records))
      )
        throw Error(
          "Serial page scope or review disagreed. Refresh the complete review.",
        );
      if (
        p.items.some(
          (i) =>
            !i ||
            i.sku !== sku ||
            typeof i.source_ok !== "boolean" ||
            !serialText(i.event_id) ||
            (view !== "observations" &&
              (!Number.isSafeInteger(i.revision) || i.revision < 1)) ||
            (view === "observations" &&
              (!Array.isArray(i.target) ||
                i.target.length > 3 ||
                !i.target.every(serialText) ||
                !Number.isSafeInteger(i.observed_serial_count) ||
                i.observed_serial_count < 0 ||
                !Number.isSafeInteger(i.serial_ordinal) ||
                i.serial_ordinal < 0 ||
                (i.observed_serial === null
                  ? i.observed_serial_count !== 0 || i.serial_ordinal !== 0
                  : i.serial_ordinal < 1 ||
                    i.serial_ordinal > i.observed_serial_count))) ||
            (view !== "observations" &&
              (!serialText(i.serial) || (serial && i.serial !== serial))) ||
            (view === "observations" &&
              ((i.observed_serial !== null && !serialText(i.observed_serial)) ||
                (serial && i.observed_serial !== serial))),
        )
      )
        throw Error(
          "Serial page contained an invalid literal identity. Refresh the complete review.",
        );
      const keys = p.items.map((i) =>
        view === "current"
          ? i.serial
          : view === "history"
            ? String(i.revision)
            : JSON.stringify([
                i.event_id,
                i.observation_ordinal,
                i.serial_ordinal,
              ]),
      );
      if (
        new Set(keys).size !== keys.length ||
        keys.some((k) => seen.current.has(k)) ||
        (p.next_cursor !== null && cursors.current.has(p.next_cursor))
      )
        throw Error(
          "Serial page repeated retained records or its cursor. Refresh the complete review.",
        );
      const count = (next ? rows.length : 0) + p.items.length;
      if (
        count > p.total_records ||
        (p.next_cursor === null && count !== p.total_records) ||
        (p.next_cursor !== null &&
          (p.items.length === 0 || count >= p.total_records))
      )
        throw Error(
          "Serial page completeness disagreed. Refresh the complete review.",
        );
      keys.forEach((k) => seen.current.add(k));
      if (p.next_cursor) cursors.current.add(p.next_cursor);
      setRows((prior) => (next ? [...prior, ...p.items] : p.items));
      setPage(p);
      setAllSources(
        (prior) =>
          (next ? prior : true) &&
          p.page_source_ok &&
          p.current_source_ok !== false &&
          p.items.every((i) => i.source_ok),
      );
    } catch (e) {
      if (alive.current && current === sequence.current) {
        setHeld(true);
        setRows([]);
        setPage(null);
        setError((e as Error).message);
      }
    } finally {
      if (alive.current && current === sequence.current) setBusy(false);
    }
  };
  if (!products.length) return null;
  return (
    <details className="panel serial-trace">
      <summary className="panel-head">
        Serial custody and original observations
      </summary>
      <div className="serial-observations">
        <p>
          Review retained identities across facilities. Source checks describe
          original records; they do not confirm a physical count.
        </p>
        <form
          className="form-grid"
          onSubmit={(e) => {
            e.preventDefault();
            void load();
          }}
        >
          <label>
            Serial SKU
            <select
              value={sku}
              disabled={busy}
              onChange={(e) => {
                reset();
                setSku(e.target.value);
              }}
            >
              {products.map((p) => (
                <option key={p.sku} value={p.sku}>
                  {p.name} · {p.sku}
                </option>
              ))}
            </select>
          </label>
          <label>
            Literal serial{view === "history" ? " (required)" : " (optional)"}
            <input
              value={serial}
              disabled={busy}
              autoComplete="off"
              autoCapitalize="none"
              spellCheck={false}
              onChange={(e) => {
                reset();
                setSerial(e.target.value);
              }}
            />
          </label>
          <label>
            Serial evidence view
            <select
              value={view}
              disabled={busy}
              onChange={(e) => {
                reset();
                setView(e.target.value as View);
              }}
            >
              <option value="current">Current custody</option>
              <option value="history">Original unit history</option>
              <option value="observations">Original observations</option>
            </select>
          </label>
          <label className="serial-zero">
            <input
              type="checkbox"
              checked={local}
              disabled={busy}
              onChange={(e) => {
                reset();
                setLocal(e.target.checked);
              }}
            />
            Current facility only
          </label>
          <button
            className="btn"
            disabled={busy || !sku || (view === "history" && !serial)}
          >
            {busy ? "Loading serial evidence…" : "Refresh serial review"}
          </button>
        </form>
        {error && (
          <p role="alert" className="scanner-error">
            {error} Retained pages were cleared. Explicitly refresh before
            continuing this review.
          </p>
        )}
        {page && (
          <>
            <p role="status">
              Global SKU review {page.review} · {rows.length} of{" "}
              {page.total_records} retained records ·{" "}
              {allSources
                ? "Selected original sources agree"
                : "Original source disagreement — hold affected work and reconcile"}
            </p>
            <div className="table-wrap">
              <table>
                <thead>
                  <tr>
                    <th>Literal serial</th>
                    <th>
                      {view === "observations"
                        ? "Original observation"
                        : "Custody"}
                    </th>
                    <th>Original source</th>
                  </tr>
                </thead>
                <tbody>
                  {rows.map((r, n) => (
                    <tr key={n}>
                      <td className="mono">
                        {view === "observations"
                          ? (r.observed_serial ?? "Witnessed empty group")
                          : r.serial}
                      </td>
                      <td>
                        {view === "current" ? (
                          place(r.place)
                        ) : view === "history" ? (
                          <>
                            {place(r.before_place)} → {place(r.after_place)} ·
                            revision {r.revision}
                          </>
                        ) : (
                          <>
                            Target{" "}
                            {r.target.length
                              ? r.target.join(" / ")
                              : "physical quantity"}{" "}
                            · label {r.serial_ordinal} of{" "}
                            {r.observed_serial_count}
                          </>
                        )}
                      </td>
                      <td>
                        {r.source_ok ? "Source agrees" : "Source disagreement"}
                        <details>
                          <summary>Original reference</summary>
                          <p>{r.event_id}</p>
                          <p>
                            {r.actor ?? r.last_source?.actor} ·{" "}
                            {r.recorded_at ?? r.last_source?.recorded_at}
                          </p>
                        </details>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {page.next_cursor && (
              <button
                className="btn"
                disabled={busy || held || rows.length >= 1000}
                onClick={() => void load(true)}
              >
                Load next serial page
              </button>
            )}
            {page.next_cursor && rows.length >= 1000 && (
              <p>
                This view holds up to 1000 records. Refine the literal
                serial/facility filter and refresh to review another scope.
              </p>
            )}
            {page.next_cursor === null && (
              <p>
                End of retained records in this selected scope. A zero result
                does not prove a physically empty location or complete
                historical identity reconciliation.
              </p>
            )}
          </>
        )}
      </div>
    </details>
  );
}
