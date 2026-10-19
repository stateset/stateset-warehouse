import { useEffect, useRef, useState, type FormEvent } from "react";
import type { WarehouseState } from "./types";
import { availabilityLabel, instant, validControl } from "./lotAvailability";
import "./Workflows.css";

const sections = {
  facilities: "Facilities and restrictions",
  positions: "Physical positions",
  allocations: "Reserved and staged work",
  receipts: "Receipts and customer returns",
  shipments: "Packages and original shipments",
  transfers: "Facility transfers and transit",
  movements: "Physical movement history",
  policies: "Expiry and quarantine history",
  decisions: "Original fulfillment eligibility",
} as const;
type Section = keyof typeof sections;
type Scope = { sku: string; lot: string };
type Item = {
  warehouse_id: string;
  key: string[];
  source_ok: boolean;
  document: Record<string, any>;
  observed_at?: string;
};
type View = Scope & {
  schema_version: number;
  review: string;
  section: Section;
  checked_at: string;
  items: Item[];
  next_cursor: string | null;
  page_source_ok: boolean;
  source_checks: string;
  limits: string[];
  summary: {
    physical: number;
    reserved: number;
    usable_storage: number;
    receiving: number;
    staged: number;
    physical_reconciliation_ok: boolean;
    sections: Record<Section, number>;
  };
};
const whole = (n: unknown): n is number =>
  typeof n === "number" && Number.isSafeInteger(n) && n >= 0;
const text = (s: unknown): s is string =>
  typeof s === "string" &&
  s.length > 0 &&
  s.trim() === s &&
  new TextEncoder().encode(s).length <= 240 &&
  !/[\u0000-\u001f\u007f-\u009f]/.test(s);
const quantity = (n: unknown) =>
  whole(n) ? new Intl.NumberFormat("en-US").format(n) : "Needs reconciliation";
const signedEvidence = (n: unknown) =>
  typeof n === "number" && Number.isSafeInteger(n)
    ? new Intl.NumberFormat("en-US").format(n)
    : "Needs reconciliation";
const label = (v: unknown) => (typeof v === "string" && v ? v : "—");
const date = (v: unknown) =>
  instant(v, 9) === undefined ? "—" : new Date(String(v)).toLocaleString();
const recordKey = (r: Item) => JSON.stringify([r.warehouse_id, r.key]);
function checked(
  value: View,
  scope: Scope,
  section: Section,
  review?: string,
): View {
  const fail = () => {
    throw new Error(
      "Lot trace evidence could not be verified. Refresh the review.",
    );
  };
  if (
    !value ||
    value.schema_version !== 1 ||
    value.sku !== scope.sku ||
    value.lot !== scope.lot ||
    value.section !== section ||
    !/^[0-9a-f]{64}$/.test(value.review) ||
    (review !== undefined && value.review !== review) ||
    instant(value.checked_at) === undefined ||
    !Array.isArray(value.items) ||
    value.items.length > 25 ||
    (value.next_cursor !== null &&
      (typeof value.next_cursor !== "string" ||
        value.next_cursor.length > 4096)) ||
    typeof value.page_source_ok !== "boolean" ||
    !value.summary ||
    typeof value.summary.physical_reconciliation_ok !== "boolean" ||
    ![
      value.summary.physical,
      value.summary.reserved,
      value.summary.usable_storage,
      value.summary.receiving,
      value.summary.staged,
    ].every(whole) ||
    !Object.keys(sections).every((k) =>
      whole(value.summary.sections?.[k as Section]),
    )
  )
    fail();
  if (
    value.source_checks !==
      "physical_and_reservation_reconstruction_plus_selected_page_sources" ||
    !Array.isArray(value.limits) ||
    value.limits.length === 0 ||
    value.limits.some((v) => typeof v !== "string") ||
    value.summary.usable_storage > value.summary.physical ||
    (value.summary.physical_reconciliation_ok &&
      value.summary.reserved > value.summary.physical)
  )
    fail();
  const seen = new Set<string>();
  for (const item of value.items) {
    if (
      !item ||
      !text(item.warehouse_id) ||
      !Array.isArray(item.key) ||
      item.key.length > 4 ||
      !item.key.every(text) ||
      typeof item.source_ok !== "boolean" ||
      !item.document ||
      typeof item.document !== "object" ||
      Array.isArray(item.document) ||
      seen.has(recordKey(item))
    )
      fail();
    seen.add(recordKey(item));
    const d = item.document;
    const nums: Record<Section, string[]> = {
      facilities: [],
      positions: ["physical", "reserved", "usable_storage"],
      allocations: [
        "quantity",
        "picked",
        "shipped",
        "released",
        "storage_reserved",
        "staged",
        "original_line_quantity",
      ],
      receipts: ["expected", "received", "cancelled"],
      shipments: ["quantity"],
      transfers: ["dispatched", "received", "lost", "loss_record_count"],
      movements: ["sequence", "quantity"],
      policies: ["version", "ordinal"],
      decisions: ["version"],
    };
    if (!nums[section].every((k) => whole(d[k]))) fail();
    if (section === "facilities") {
      if (
        d.warehouse?.id !== item.warehouse_id ||
        !whole(d.version) ||
        !whole(d.historic_eligibility_unknown_through_version) ||
        d.historic_eligibility_unknown_through_version > d.version ||
        typeof d.expiry_required !== "boolean" ||
        !d.balance ||
        ![
          d.balance.physical,
          d.balance.reserved,
          d.balance.usable_storage,
        ].every(whole) ||
        typeof d.balance.physical_reconciliation_ok !== "boolean" ||
        (d.control !== null && !validControl(d.control, scope.sku, scope.lot))
      )
        fail();
      const expected = d.control?.holds.length
        ? "quarantined"
        : d.control?.expires_at
          ? instant(value.checked_at)! >= instant(d.control.expires_at)!
            ? "expired"
            : "available"
          : d.expiry_required
            ? "expiry_missing"
            : "available";
      if (
        d.availability !== expected ||
        item.source_ok !== d.balance.physical_reconciliation_ok
      )
        fail();
    }
    if (
      section === "positions" &&
      (!Number.isSafeInteger(d.ledger) ||
        !Number.isSafeInteger(d.allocation_reserved))
    )
      fail();
    if (
      section === "positions" &&
      item.source_ok &&
      (d.physical !== d.ledger ||
        d.reserved !== d.allocation_reserved ||
        d.reserved > d.physical)
    )
      fail();
    if (section === "receipts" && d.received + d.cancelled > d.expected) fail();
    if (
      section === "allocations" &&
      (d.shipped > d.picked ||
        d.picked + d.released > d.quantity ||
        d.staged !== d.picked - d.shipped ||
        d.storage_reserved !== d.quantity - d.picked - d.released)
    )
      fail();
    if (
      section === "transfers" &&
      (d.received + d.lost > d.dispatched ||
        (d.lost > 0 && d.loss_record_count === 0) ||
        d.source_warehouse_id !== item.warehouse_id ||
        d.source_warehouse_id === d.destination_warehouse_id)
    )
      fail();
    if (
      section === "shipments" &&
      (!["packed", "unpacked", "handed_off"].includes(d.state) ||
        ![
          "package_event",
          "event",
          "legacy_snapshot",
          "synthetic_fixture",
        ].includes(d.source_kind) ||
        (item.source_ok &&
          d.state === "handed_off" &&
          d.source_kind === "package_event" &&
          !text(d.closure_event_id)) ||
        (item.source_ok && d.source_kind === "event" && !text(d.event_id)))
    )
      fail();
  }
  if (value.page_source_ok !== value.items.every((i) => i.source_ok)) fail();
  return value;
}
function Record({ item, section }: { item: Item; section: Section }) {
  const d = item.document;
  const row = (title: string, value: unknown) => (
    <span className="subline">
      <strong>{title}:</strong> {label(value)}
    </span>
  );
  switch (section) {
    case "facilities":
      return (
        <>
          <strong>
            {label(d.warehouse.code)} · {label(d.warehouse.name)}
          </strong>
          {row("City", d.warehouse.city)}
          {row(
            "Availability at observation",
            availabilityLabel[d.availability as keyof typeof availabilityLabel],
          )}
          {row(
            "Expiry",
            d.control?.expires_at
              ? date(d.control.expires_at)
              : d.expiry_required
                ? "Label required"
                : "Not recorded",
          )}
          {row("Active holds", String(d.control?.holds.length ?? 0))}
          {row(
            "Physical / reserved / usable storage",
            `${quantity(d.balance.physical)} / ${quantity(d.balance.reserved)} / ${quantity(d.balance.usable_storage)}`,
          )}
          {row("Observed", date(item.observed_at))}
          {row(
            "Historic eligibility",
            `Unknown through version ${d.historic_eligibility_unknown_through_version}`,
          )}
        </>
      );
    case "positions":
      return (
        <>
          <strong>{label(d.location)}</strong>
          {row("Location type", d.kind)}
          {row(
            "Physical / reserved / usable storage",
            `${quantity(d.physical)} / ${quantity(d.reserved)} / ${quantity(d.usable_storage)}`,
          )}
          {row(
            "Availability at observation",
            availabilityLabel[d.availability as keyof typeof availabilityLabel],
          )}
          {row(
            "Independent ledger / allocation reservation",
            `${signedEvidence(d.ledger)} / ${signedEvidence(d.allocation_reserved)}`,
          )}
          {row("Observed", date(item.observed_at))}
        </>
      );
    case "allocations":
      return (
        <>
          <strong>
            {label(d.order_reference)} · {label(d.order_status)}
          </strong>
          {row("Order / line", `${label(d.order_id)} / ${label(d.line_id)}`)}
          {row("Storage bin", d.location)}
          {row(
            "Storage reserved / staged",
            `${quantity(d.storage_reserved)} / ${quantity(d.staged)}`,
          )}
          {row(
            "Allocated / shipped / released",
            `${quantity(d.quantity)} / ${quantity(d.shipped)} / ${quantity(d.released)}`,
          )}
        </>
      );
    case "receipts":
      return (
        <>
          <strong>
            {label(d.reference)} · {label(d.status)}
          </strong>
          {row(
            "Receipt / line",
            `${label(d.receipt_id)} / ${label(d.line_id)}`,
          )}
          {row(
            "Expected / received / cancelled",
            `${quantity(d.expected)} / ${quantity(d.received)} / ${quantity(d.cancelled)}`,
          )}
          {d.return_order_id &&
            row(
              "Original return order / line",
              `${d.return_order_id} / ${d.return_order_line_id}`,
            )}
        </>
      );
    case "shipments":
      return (
        <>
          <strong>
            {d.source_kind === "legacy_snapshot"
              ? "Legacy shipment baseline"
              : d.source_kind === "synthetic_fixture"
                ? "Synthetic shipment baseline"
                : d.state === "handed_off"
                  ? "Recorded carrier handoff"
                  : d.state === "unpacked"
                    ? "Unpacked package"
                    : "Active packed package"}
          </strong>
          {row(
            "Package / order",
            `${label(d.package_reference)} / ${label(d.order_id)}`,
          )}
          {row(
            "Line / original bin",
            `${label(d.line_id)} / ${label(d.source_location)}`,
          )}
          {row("Each units", quantity(d.quantity))}
          {row(
            "Carrier / handoff",
            `${label(d.carrier)} / ${label(d.handoff_reference)}`,
          )}
          {row(
            "Original event",
            d.closure_event_id ?? d.event_id ?? d.packed_event_id,
          )}
          {row("Time", date(d.closed_at ?? d.shipped_at ?? d.packed_at))}
          {["legacy_snapshot", "synthetic_fixture"].includes(d.source_kind) && (
            <span className="subline">
              Retained baseline; no newly verified physical handoff.
            </span>
          )}
        </>
      );
    case "transfers":
      return (
        <>
          <strong>{label(d.reference)}</strong>
          {row(
            "Source → destination",
            `${label(d.source_warehouse_id)} → ${label(d.destination_warehouse_id)}`,
          )}
          {row(
            "Transfer / line",
            `${label(d.transfer_id)} / ${label(d.line_id)}`,
          )}
          {row(
            "Dispatched / arrived / recorded loss / in transit",
            `${quantity(d.dispatched)} / ${quantity(d.received)} / ${quantity(d.lost)} / ${quantity(d.dispatched - d.received - d.lost)}`,
          )}
          {row("Original loss/reversal records", quantity(d.loss_record_count))}
          {row(
            "Carrier / handoff",
            `${label(d.carrier)} / ${label(d.handoff_reference)}`,
          )}
          {row("Original dispatch event", d.source_event_id)}
        </>
      );
    case "movements":
      return (
        <>
          <strong>
            {label(d.kind).replaceAll("_", " ")} · {quantity(d.quantity)} each
          </strong>
          {row(
            "From → to",
            `${label(d.from_location)} → ${label(d.to_location)}`,
          )}
          {row("Reference", d.reference)}
          {row("Observed work", `${date(d.at)} · ${label(d.actor)}`)}
          {row("Movement", d.id)}
          {row(
            "Related receipt / package / transfer",
            d.receipt_id ?? d.package_id ?? d.transfer_id,
          )}
        </>
      );
    case "policies":
      return (
        <>
          <strong>{label(d.change?.kind).replaceAll("_", " ")}</strong>
          {row("Reason", d.change?.reason)}
          {row(
            "Actor / time",
            `${label(d.change?.actor)} · ${date(d.change?.recorded_at)}`,
          )}
          {row(
            "Expiry",
            d.change?.expires_at
              ? date(d.change.expires_at)
              : "Unknown or unchanged",
          )}
          {row("Hold", d.change?.hold_id)}
          {row("Original event", d.event_id)}
        </>
      );
    case "decisions":
      return (
        <>
          <strong>{label(d.kind).replaceAll("_", " ")}</strong>
          {row("Original event", d.event_id)}
          {row("Admission evidence", `Warehouse version ${d.version}`)}
          <span className="subline">
            The original fulfillment decision is checked independently of the
            lot’s current restriction.
          </span>
        </>
      );
  }
}
export default function LotTracePanel({
  state,
  disabled,
  request,
}: {
  state: WarehouseState;
  disabled: boolean;
  request: (path: string, body?: unknown) => Promise<any>;
}) {
  const [sku, setSku] = useState(""),
    [lot, setLot] = useState(""),
    [section, setSection] = useState<Section>("facilities");
  const [scope, setScope] = useState<Scope | null>(null),
    [view, setView] = useState<View | null>(null),
    [items, setItems] = useState<Item[]>([]);
  const [pages, setPages] = useState<
    { checked_at: string; records: number; page_source_ok: boolean }[]
  >([]);
  const [error, setError] = useState(""),
    [busy, setBusy] = useState(false),
    [old, setOld] = useState(false);
  const alive = useRef(true),
    sequence = useRef(0),
    inFlight = useRef(false),
    observed = useRef(0),
    cursors = useRef(new Set<string>());
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      ++sequence.current;
    };
  }, []);
  useEffect(() => {
    const id = setInterval(
      () =>
        setOld(
          !!observed.current && performance.now() - observed.current > 60000,
        ),
      5000,
    );
    return () => clearInterval(id);
  }, []);
  const clear = () => {
    ++sequence.current;
    setView(null);
    setItems([]);
    setPages([]);
    setScope(null);
    setError("");
    observed.current = 0;
    setOld(false);
    cursors.current.clear();
  };
  const load = async (
    target: Scope,
    nextSection: Section,
    review?: string,
    after?: string,
  ) => {
    if (inFlight.current || disabled) return;
    inFlight.current = true;
    const current = ++sequence.current;
    setBusy(true);
    setError("");
    if (!after) {
      setItems([]);
      setPages([]);
      setView(null);
      cursors.current.clear();
    }
    try {
      const query = new URLSearchParams({
        sku: target.sku,
        lot: target.lot,
        section: nextSection,
        limit: "25",
      });
      if (review) query.set("review", review);
      if (after) query.set("after", after);
      const result = checked(
        await request(`/v1/lot-trace?${query}`),
        target,
        nextSection,
        review,
      );
      if (!alive.current || current !== sequence.current) return;
      if (
        result.next_cursor &&
        (result.next_cursor === after ||
          cursors.current.has(result.next_cursor))
      )
        throw new Error(
          "Lot trace cursor did not advance. Refresh the review.",
        );
      if (after) cursors.current.add(after);
      const fetched = result.items.map((i) => ({
        ...i,
        observed_at: result.checked_at,
      }));
      const previous = after ? items : [];
      const seen = new Set(previous.map(recordKey));
      if (
        fetched.some((i) => seen.has(recordKey(i))) ||
        previous.length + fetched.length >
          result.summary.sections[nextSection] ||
        (!result.next_cursor &&
          previous.length + fetched.length !==
            result.summary.sections[nextSection])
      )
        throw new Error(
          "Lot trace pages disagree with the original record count. Refresh the review.",
        );
      setScope(target);
      setSection(nextSection);
      setView(result);
      setItems([...previous, ...fetched]);
      setPages((previousPages) => [
        ...(after ? previousPages : []),
        {
          checked_at: result.checked_at,
          records: fetched.length,
          page_source_ok: result.page_source_ok,
        },
      ]);
      observed.current = performance.now();
      setOld(false);
    } catch (failure) {
      if (alive.current && current === sequence.current) {
        setView(null);
        setItems([]);
        setPages([]);
        setError((failure as Error).message);
      }
    } finally {
      inFlight.current = false;
      if (alive.current && current === sequence.current) setBusy(false);
    }
  };
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (text(sku) && text(lot)) void load({ sku, lot }, section);
  };
  const exportLoaded = () => {
    if (!view || !scope || busy || old || error) return;
    const report = {
      schema_version: 1,
      scope: "loaded_lot_trace_section",
      sku: scope.sku,
      lot: scope.lot,
      review: view.review,
      section,
      checked_at: view.checked_at,
      summary: view.summary,
      pages,
      items,
      loaded_records: items.length,
      total_section_records: view.summary.sections[section],
      section_complete:
        view.next_cursor === null &&
        items.length === view.summary.sections[section],
      global_trace_complete: false,
      source_checks: view.source_checks,
      limits: view.limits,
    };
    const url = URL.createObjectURL(
      new Blob([JSON.stringify(report, null, 2) + "\n"], {
        type: "application/json",
      }),
    );
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = "warehouse-lot-trace.json";
    anchor.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };
  return (
    <details className="workflow-panel lot-trace-panel">
      <summary>Trace a lot across facilities</summary>
      <p>
        Find retained work for an exact SKU and lot label across your
        warehouses. Matching labels alone do not prove a common manufacturer
        batch. This review changes no inventory or restrictions.
      </p>
      <form className="workflow-form" onSubmit={submit}>
        <label>
          Trace SKU
          <input
            list="lot-trace-skus"
            value={sku}
            disabled={busy || disabled}
            onChange={(e) => {
              clear();
              setSku(e.target.value);
            }}
            required
            maxLength={240}
          />
          <datalist id="lot-trace-skus">
            {state.products.map((p) => (
              <option key={p.sku} value={p.sku} />
            ))}
          </datalist>
        </label>
        <label>
          Trace lot
          <input
            value={lot}
            disabled={busy || disabled}
            onChange={(e) => {
              clear();
              setLot(e.target.value);
            }}
            required
            maxLength={240}
          />
        </label>
        <button
          className="btn"
          disabled={busy || disabled || !text(sku) || !text(lot)}
        >
          {view ? "Refresh trace review" : "Review lot trace"}
        </button>
      </form>
      {error && (
        <p role="alert" className="workflow-error">
          {error} Old pages have been discarded.
        </p>
      )}
      {busy && <p role="status">Loading the original lot records…</p>}
      {old && (
        <p role="status" className="alert warning">
          This observation is older than one minute. Refresh the review before
          continuing or exporting.
        </p>
      )}
      {view && scope && (
        <>
          <p>
            <strong>
              {scope.sku} / {scope.lot}
            </strong>{" "}
            · Observed {date(view.checked_at)}
          </p>
          <p>
            Physical {quantity(view.summary.physical)} · Reserved{" "}
            {quantity(view.summary.reserved)} · Usable storage{" "}
            {quantity(view.summary.usable_storage)} · Receiving{" "}
            {quantity(view.summary.receiving)} · Staged{" "}
            {quantity(view.summary.staged)}
          </p>
          {!view.summary.physical_reconciliation_ok && (
            <p role="alert" className="workflow-error">
              Physical custody or reservation evidence disagrees. Retain the
              records for reconciliation.
            </p>
          )}
          {items.some((i) => !i.source_ok) && (
            <p role="alert" className="workflow-error">
              One or more original source records need reconciliation. Balanced
              stock alone does not verify their history.
            </p>
          )}
          <label>
            Trace records
            <select
              value={section}
              disabled={busy || disabled || old}
              onChange={(e) =>
                void load(scope, e.target.value as Section, view.review)
              }
            >
              {Object.entries(sections).map(([key, title]) => (
                <option key={key} value={key}>
                  {title} ({quantity(view.summary.sections[key as Section])})
                </option>
              ))}
            </select>
          </label>
          <p role="status">
            Showing {quantity(items.length)} of{" "}
            {quantity(view.summary.sections[section])}{" "}
            {sections[section].toLowerCase()} records.
            {view.next_cursor
              ? " More records remain."
              : " This section is complete."}
          </p>
          <div className="table-scroll">
            <table
              className="lot-trace-table"
              aria-label="Lot trace records"
              role="table"
            >
              <thead>
                <tr>
                  <th>Warehouse</th>
                  <th>Retained record</th>
                  <th>Source check</th>
                </tr>
              </thead>
              <tbody>
                {items.map((item) => (
                  <tr key={recordKey(item)}>
                    <td className="mono">{item.warehouse_id}</td>
                    <td>
                      <Record item={item} section={section} />
                    </td>
                    <td>
                      {item.source_ok
                        ? "Matches retained evidence"
                        : "Needs reconciliation"}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {!items.length && (
            <p>
              No retained records for this section. Unobserved lot ancestry is
              not inferred.
            </p>
          )}
          {view.next_cursor && (
            <button
              className="btn"
              disabled={busy || disabled || old}
              onClick={() =>
                void load(scope, section, view.review, view.next_cursor!)
              }
            >
              Load more trace records
            </button>
          )}
          <button
            className="btn"
            disabled={busy || old || !!error}
            onClick={exportLoaded}
          >
            Export loaded trace records
          </button>
          <p>
            Availability is observed per page and can expire without a new
            inventory movement. Refresh before separate physical work or hold
            changes. Exports identify the loaded section, remaining records and
            each page’s observation time. Earlier shipment baselines remain
            distinct from original recorded handoffs.
          </p>
        </>
      )}
    </details>
  );
}
