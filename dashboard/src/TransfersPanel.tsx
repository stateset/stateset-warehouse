import { useEffect, useRef, useState, type FormEvent } from "react";
import type { Command, WarehouseState, UnitInput } from "./types";
import "./Workflows.css";
import UnitQuantityInput from "./UnitQuantityInput";
import TransferLossPanel from "./TransferLossPanel";
type Line = {
  id: string;
  sku: string;
  lot: string;
  location: string;
  quantity: number;
  received: number;
  lost: number;
  loss_record_count: number;
};
type Transfer = {
  id: string;
  reference: string;
  source_warehouse_id: string;
  destination_warehouse_id: string;
  carrier: string;
  handoff_reference: string;
  lines: Line[];
  ok: boolean;
};
type Page = {
  warehouse_id: string;
  version: number;
  items: { cursor: number; data: Transfer }[];
  next_cursor: number | null;
};
type DraftLine = {
  id: string;
  bucket: string;
  quantity: string;
  input?: UnitInput;
};
function validTransfer(t: Transfer) {
  return (
    t &&
    typeof t.id === "string" &&
    typeof t.ok === "boolean" &&
    typeof t.source_warehouse_id === "string" &&
    typeof t.destination_warehouse_id === "string" &&
    t.source_warehouse_id !== t.destination_warehouse_id &&
    Array.isArray(t.lines) &&
    t.lines.length > 0 &&
    t.lines.length <= 100 &&
    new Set(t.lines.map((l) => l.id)).size === t.lines.length &&
    t.lines.every(
      (l) =>
        [l.id, l.sku, l.lot, l.location].every(
          (v) => typeof v === "string" && v.length > 0,
        ) &&
        Number.isSafeInteger(l.quantity) &&
        l.quantity > 0 &&
        l.quantity <= 1000000 &&
        Number.isSafeInteger(l.received) &&
        l.received >= 0 &&
        Number.isSafeInteger(l.lost) &&
        l.lost >= 0 &&
        Number.isSafeInteger(l.loss_record_count) &&
        l.loss_record_count >= 0 &&
        (l.lost === 0 || l.loss_record_count > 0) &&
        l.received + l.lost <= l.quantity,
    )
  );
}

export default function TransfersPanel({
  state,
  disabled,
  canWork,
  admin,
  request,
  onSubmit,
}: {
  state: WarehouseState;
  disabled: boolean;
  canWork: boolean;
  admin: boolean;
  request: (path: string, body?: unknown) => Promise<any>;
  onSubmit: (
    command: Command,
    version: number,
    versions?: Record<string, number>,
    inputs?: UnitInput[],
  ) => Promise<boolean>;
}) {
  const [records, setRecords] = useState<Page | null>(null),
    [openOnly, setOpenOnly] = useState(true),
    [error, setError] = useState(""),
    [notice, setNotice] = useState(""),
    [busy, setBusy] = useState(false);
  const [facilities, setFacilities] = useState<WarehouseState["warehouse"][]>(
      [],
    ),
    [after, setAfter] = useState<string | null>(null);
  const [destination, setDestination] = useState(""),
    [reference, setReference] = useState(""),
    [carrier, setCarrier] = useState(""),
    [handoff, setHandoff] = useState("");
  const [lines, setLines] = useState<DraftLine[]>([
    { id: crypto.randomUUID(), bucket: "", quantity: "1" },
  ]);
  const [review, setReview] = useState<{
    command: Command;
    version: number;
    destination: string;
    inputs: UnitInput[];
  } | null>(null);
  const [arrival, setArrival] = useState<{
      transfer: Transfer;
      line: Line;
      version: number;
    } | null>(null),
    [received, setReceived] = useState("1"),
    [location, setLocation] = useState("RECV-01"),
    [arrivalReference, setArrivalReference] = useState("");
  const [arrivalInput, setArrivalInput] = useState<UnitInput>();
  const alive = useRef(true),
    sequence = useRef(0);
  const base = `/v1/warehouses/${encodeURIComponent(state.warehouse.id)}`;
  const buckets = state.stocks.filter(
    (b) =>
      b.on_hand > b.reserved &&
      state.locations.some(
        (l) => l.code === b.location && l.kind === "storage",
      ),
  );
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      ++sequence.current;
    };
  }, []);
  const load = async (cursor: number | null = null) => {
    const current = ++sequence.current;
    setBusy(true);
    setError("");
    try {
      const page: Page = await request(
        `${base}/transfers?limit=50&open_only=${openOnly}${
          cursor ? `&after=${cursor}` : ""
        }`,
      );
      if (!alive.current || current !== sequence.current) return;
      if (
        page.warehouse_id !== state.warehouse.id ||
        !Number.isSafeInteger(page.version) ||
        page.version < 0 ||
        !Array.isArray(page.items) ||
        page.items.some(
          (row) =>
            !Number.isSafeInteger(row.cursor) ||
            row.cursor <= (cursor ?? 0) ||
            !validTransfer(row.data) ||
            (row.data.source_warehouse_id !== state.warehouse.id &&
              row.data.destination_warehouse_id !== state.warehouse.id),
        ) ||
        (page.next_cursor !== null &&
          (!Number.isSafeInteger(page.next_cursor) ||
            page.next_cursor <= (cursor ?? 0)))
      )
        throw Error(
          "Transfer page could not be verified. Refresh before receiving.",
        );
      if (cursor && records && records.version !== page.version)
        throw Error(
          "Warehouse changed while paging transfers. Refresh the complete list before receiving.",
        );
      setRecords((previous) => ({
        ...page,
        items:
          cursor && previous ? [...previous.items, ...page.items] : page.items,
      }));
    } catch (failure) {
      if (alive.current && current === sequence.current) {
        setError((failure as Error).message);
        setRecords(null);
        setArrival(null);
      }
    } finally {
      if (alive.current && current === sequence.current) setBusy(false);
    }
  };
  const destinations = async (cursor: string | null = null) => {
    setBusy(true);
    setError("");
    try {
      const page = await request(
        `/v1/warehouses?limit=100${
          cursor ? `&after=${encodeURIComponent(cursor)}` : ""
        }`,
      );
      if (!alive.current) return;
      if (
        !Array.isArray(page.warehouses) ||
        page.warehouses.some(
          (row: any) => typeof row.warehouse?.id !== "string",
        ) ||
        (page.next_after != null &&
          (typeof page.next_after !== "string" || page.next_after === cursor))
      )
        throw Error("Destination list could not be verified.");
      setFacilities((previous) =>
        [
          ...(cursor ? previous : []),
          ...page.warehouses.map((r: any) => r.warehouse),
        ].filter((w) => w.id !== state.warehouse.id),
      );
      setAfter(page.next_after ?? null);
    } catch (failure) {
      if (alive.current) setError((failure as Error).message);
    } finally {
      if (alive.current) setBusy(false);
    }
  };
  useEffect(() => {
    void load();
  }, [request, state.warehouse.id, state.version, openOnly]);
  const reviewDispatch = async (event: FormEvent) => {
    event.preventDefault();
    if (disabled || busy) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      if (!destination || destination === state.warehouse.id)
        throw Error("Choose another destination facility.");
      const [source, target]: WarehouseState[] = await Promise.all([
        request(base),
        request(`/v1/warehouses/${encodeURIComponent(destination)}`),
      ]);
      if (!alive.current) return;
      if (
        !Number.isSafeInteger(source.version) ||
        source.version < 0 ||
        source.warehouse.id !== state.warehouse.id ||
        target.warehouse.id !== destination
      )
        throw Error("Transfer facilities could not be verified.");
      const totals = new Map<string, number>();
      const dispatchLines = lines.map((line) => {
        const [sku, lot, location] = JSON.parse(
          line.bucket || "[]",
        ) as string[];
        const quantity = Number(line.quantity);
        if (
          !sku ||
          !lot ||
          !location ||
          !Number.isSafeInteger(quantity) ||
          quantity < 1 ||
          quantity > 1000000
        )
          throw Error(
            "Choose exact source stock and a whole each quantity for every line.",
          );
        if (!target.products.some((p) => p.sku === sku))
          throw Error(`Configure ${sku} at the destination before dispatch.`);
        totals.set(line.bucket, (totals.get(line.bucket) ?? 0) + quantity);
        return { id: line.id, sku, lot, location, quantity };
      });
      for (const [key, quantity] of totals) {
        const [sku, lot, location] = JSON.parse(key);
        const bucket = source.stocks.find(
          (b) => b.sku === sku && b.lot === lot && b.location === location,
        );
        if (
          !bucket ||
          bucket.on_hand - bucket.reserved < quantity ||
          !source.locations.some(
            (l) => l.code === location && l.kind === "storage",
          )
        )
          throw Error(
            "Source stock changed. Review the available unreserved quantity.",
          );
      }
      setReview({
        command: {
          type: "dispatch_transfer",
          reference,
          destination_warehouse_id: destination,
          carrier,
          handoff_reference: handoff,
          lines: dispatchLines,
        },
        inputs: lines.flatMap((l) =>
          l.input ? [structuredClone(l.input)] : [],
        ),
        version: source.version,
        destination: `${target.warehouse.code} · ${target.warehouse.name}`,
      });
    } catch (failure) {
      if (alive.current) setError((failure as Error).message);
    } finally {
      if (alive.current) setBusy(false);
    }
  };
  const dispatch = async () => {
    if (!review || disabled || busy) return;
    setBusy(true);
    setError("");
    try {
      if (
        await onSubmit(review.command, review.version, undefined, review.inputs)
      ) {
        if (alive.current) {
          setReview(null);
          setReference("");
          setHandoff("");
          setNotice(
            "Dispatch recorded. Goods stay in transit until the destination records their arrival.",
          );
          void load();
        }
      }
    } catch (failure) {
      if (alive.current) setError((failure as Error).message);
    } finally {
      if (alive.current) setBusy(false);
    }
  };
  const receive = async (event: FormEvent) => {
    event.preventDefault();
    if (!arrival || disabled || busy) return;
    setBusy(true);
    setError("");
    try {
      if (
        await onSubmit(
          {
            type: "receive_transfer",
            transfer_id: arrival.transfer.id,
            line_id: arrival.line.id,
            quantity: Number(received),
            location,
            arrival_reference: arrivalReference,
          },
          arrival.version,
          undefined,
          arrivalInput ? [arrivalInput] : undefined,
        )
      ) {
        if (alive.current) {
          setArrival(null);
          setArrivalReference("");
          setNotice(
            "Arrival recorded in the receiving bin. Put away verified goods through Inventory.",
          );
          void load();
        }
      }
    } catch (failure) {
      if (alive.current) setError((failure as Error).message);
    } finally {
      if (alive.current) setBusy(false);
    }
  };
  return (
    <section className="workflow-panel" aria-label="Facility transfers">
      <h2>Dispatch and receive transfers</h2>
      <p>
        Dispatched goods leave source availability. They stay in transit until
        the destination confirms each original line and lot. Record the carrier
        handoff only after goods physically leave.
      </p>
      {error && (
        <p role="alert" className="workflow-error">
          {error}
        </p>
      )}
      {notice && <p role="status">{notice}</p>}
      {canWork && (
        <details
          onToggle={(e) => {
            if (e.currentTarget.open && !facilities.length) void destinations();
          }}
        >
          <summary>New facility transfer</summary>
          {!review ? (
            <form className="workflow-form" onSubmit={reviewDispatch}>
              <label>
                Destination facility
                <select
                  required
                  disabled={disabled || busy}
                  aria-label="Destination facility"
                  value={destination}
                  onFocus={() => {
                    if (!facilities.length) void destinations();
                  }}
                  onChange={(e) => setDestination(e.target.value)}
                >
                  <option value="">Choose destination</option>
                  {facilities.map((w) => (
                    <option key={w.id} value={w.id}>
                      {w.code} · {w.name}
                    </option>
                  ))}
                </select>
              </label>
              <button
                type="button"
                className="btn"
                disabled={disabled || busy}
                onClick={() => void destinations(after)}
              >
                {after ? "Load more destinations" : "Refresh destinations"}
              </button>
              <label>
                Transfer reference
                <input
                  required
                  maxLength={240}
                  value={reference}
                  disabled={disabled || busy}
                  onChange={(e) => setReference(e.target.value)}
                />
              </label>
              <label>
                Transfer carrier
                <input
                  required
                  maxLength={240}
                  value={carrier}
                  disabled={disabled || busy}
                  onChange={(e) => setCarrier(e.target.value)}
                />
              </label>
              <label className="workflow-wide">
                Carrier handoff reference
                <input
                  required
                  maxLength={240}
                  value={handoff}
                  disabled={disabled || busy}
                  onChange={(e) => setHandoff(e.target.value)}
                />
              </label>
              {lines.map((line, index) => (
                <fieldset
                  key={line.id}
                  className="workflow-wide workflow-form"
                  disabled={disabled || busy}
                >
                  <legend>Dispatch line {index + 1}</legend>
                  <label>
                    Source stock {index + 1}
                    <select
                      required
                      aria-label={`Source stock ${index + 1}`}
                      value={line.bucket}
                      onChange={(e) =>
                        setLines((rows) =>
                          rows.map((r) =>
                            r.id === line.id
                              ? {
                                  ...r,
                                  bucket: e.target.value,
                                  quantity: "",
                                  input: undefined,
                                }
                              : r,
                          ),
                        )
                      }
                    >
                      <option value="">Choose SKU, lot and bin</option>
                      {buckets.map((b) => {
                        const key = JSON.stringify([b.sku, b.lot, b.location]);
                        return (
                          <option key={key} value={key}>
                            {b.sku} · {b.lot} · {b.location} ·{" "}
                            {b.on_hand - b.reserved} available
                          </option>
                        );
                      })}
                    </select>
                  </label>
                  <UnitQuantityInput
                    key={`${line.id}:${line.bucket}`}
                    packs={state.unit_packs}
                    sku={
                      buckets.find(
                        (b) =>
                          JSON.stringify([b.sku, b.lot, b.location]) ===
                          line.bucket,
                      )?.sku ?? ""
                    }
                    target={[line.id]}
                    value={line.quantity}
                    label={`Dispatch each units ${index + 1}`}
                    disabled={disabled || busy}
                    onChange={(each, input) =>
                      setLines((rows) =>
                        rows.map((r) =>
                          r.id === line.id
                            ? { ...r, quantity: each, input }
                            : r,
                        ),
                      )
                    }
                  />
                  {lines.length > 1 && (
                    <button
                      type="button"
                      className="btn"
                      onClick={() =>
                        setLines((rows) => rows.filter((r) => r.id !== line.id))
                      }
                    >
                      Remove line {index + 1}
                    </button>
                  )}
                </fieldset>
              ))}
              <button
                type="button"
                className="btn"
                disabled={disabled || busy || lines.length >= 100}
                onClick={() =>
                  setLines((rows) => [
                    ...rows,
                    { id: crypto.randomUUID(), bucket: "", quantity: "1" },
                  ])
                }
              >
                Add transfer line
              </button>
              <button className="btn primary" disabled={disabled || busy}>
                Review transfer dispatch
              </button>
            </form>
          ) : (
            <div>
              <h3>Confirm physical dispatch</h3>
              <p>
                {reference} · From {state.warehouse.code} to{" "}
                {review.destination} · {carrier} · Handoff {handoff}
              </p>
              <ul>
                {(review.command.lines as Line[]).map((l) => (
                  <li key={l.id}>
                    {l.sku} · {l.lot} · {l.location} · {l.quantity} each
                  </li>
                ))}
              </ul>
              <button
                className="btn primary"
                disabled={disabled || busy}
                onClick={() => void dispatch()}
              >
                Record carrier handoff
              </button>{" "}
              <button
                className="btn"
                disabled={disabled || busy}
                onClick={() => setReview(null)}
              >
                Change dispatch review
              </button>
            </div>
          )}
        </details>
      )}
      <div className="workflow-task">
        <label>
          <input
            type="checkbox"
            checked={!openOnly}
            disabled={busy}
            onChange={(e) => {
              setRecords(null);
              setArrival(null);
              setOpenOnly(!e.target.checked);
            }}
          />{" "}
          Include completed transfers
        </label>
        <button className="btn" disabled={busy} onClick={() => void load()}>
          Refresh transfers
        </button>
      </div>
      {records?.items.map(({ data: t }) => (
        <article key={t.id} className="transfer-record">
          <h3>{t.reference}</h3>
          <p>
            {t.source_warehouse_id} → {t.destination_warehouse_id} · {t.carrier}{" "}
            · Handoff {t.handoff_reference}
          </p>
          {t.lines.every((l) => l.received + l.lost === l.quantity) &&
            t.lines.some((l) => l.lost > 0) && (
              <p>
                Transit settled with recorded loss. This is not a complete
                physical arrival.
              </p>
            )}
          {!t.ok && (
            <p role="alert">
              Original transfer evidence needs reconciliation. Receiving is
              held.
            </p>
          )}
          <div className="table-scroll">
            <table>
              <thead>
                <tr>
                  <th>Original line / SKU / lot</th>
                  <th>Dispatched</th>
                  <th>Received</th>
                  <th>Recorded loss</th>
                  <th>In transit</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {t.lines.map((l) => (
                  <tr key={l.id}>
                    <td>
                      {l.id}
                      <span className="subline">
                        {l.sku} · {l.lot} · {l.location}
                      </span>
                    </td>
                    <td>{l.quantity}</td>
                    <td>{l.received}</td>
                    <td>{l.lost}</td>
                    <td>{l.quantity - l.received - l.lost}</td>
                    <td>
                      {canWork &&
                        t.destination_warehouse_id === state.warehouse.id &&
                        l.received + l.lost < l.quantity && (
                          <button
                            className="btn"
                            disabled={disabled || busy || !t.ok}
                            aria-label={`Receive transfer ${t.reference} line ${l.id}`}
                            onClick={() => {
                              setArrival({
                                transfer: t,
                                line: l,
                                version: records.version,
                              });
                              setReceived("1");
                              setArrivalInput(undefined);
                              setArrivalReference("");
                            }}
                          >
                            Receive arrival
                          </button>
                        )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <TransferLossPanel
            packs={state.unit_packs}
            transfer={t}
            warehouse={state.warehouse.id}
            version={records!.version}
            admin={admin}
            disabled={disabled || busy}
            request={request}
            onSubmit={onSubmit}
            onChanged={() => void load()}
          />
        </article>
      ))}
      {records && !records.items.length && (
        <p>
          No {openOnly ? "in-transit" : "recorded"} transfers for this facility.
        </p>
      )}
      {records?.next_cursor != null && (
        <button
          className="btn"
          disabled={busy}
          onClick={() => void load(records.next_cursor)}
        >
          Load older transfers
        </button>
      )}
      {arrival && (
        <form className="workflow-form" onSubmit={receive}>
          <h3 className="workflow-wide">
            Receive {arrival.transfer.reference} · {arrival.line.sku} ·{" "}
            {arrival.line.lot}
          </h3>
          <p className="workflow-wide">
            Original line {arrival.line.id}.{" "}
            {arrival.line.quantity - arrival.line.received - arrival.line.lost}{" "}
            each remain in transit. Verify this exact lot and observed quantity
            before recording arrival.
          </p>
          <UnitQuantityInput
            key={`${arrival.transfer.id}:${arrival.line.id}`}
            packs={state.unit_packs}
            sku={arrival.line.sku}
            target={[]}
            value={received}
            label="Arrived each units"
            max={
              arrival.line.quantity - arrival.line.received - arrival.line.lost
            }
            disabled={disabled || busy}
            onChange={(each, input) => {
              setReceived(each);
              setArrivalInput(input);
            }}
          />
          <label>
            Transfer receiving bin
            <select
              required
              disabled={disabled || busy}
              aria-label="Transfer receiving bin"
              value={location}
              onChange={(e) => setLocation(e.target.value)}
            >
              {state.locations
                .filter((l) => l.kind === "receiving")
                .map((l) => (
                  <option key={l.code}>{l.code}</option>
                ))}
            </select>
          </label>
          <label className="workflow-wide">
            Arrival evidence reference
            <input
              required
              maxLength={240}
              disabled={disabled || busy}
              value={arrivalReference}
              onChange={(e) => setArrivalReference(e.target.value)}
            />
          </label>
          <button
            className="btn primary"
            disabled={disabled || busy || !records}
          >
            Record transfer arrival
          </button>
          <button
            type="button"
            className="btn"
            disabled={disabled || busy}
            onClick={() => setArrival(null)}
          >
            Close arrival review
          </button>
        </form>
      )}
    </section>
  );
}
