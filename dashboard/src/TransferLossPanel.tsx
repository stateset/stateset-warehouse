import { useEffect, useRef, useState, type FormEvent } from "react";
import type { Command, UnitInput, UnitPack } from "./types";

import UnitQuantityInput from "./UnitQuantityInput";

type Line = {
  id: string;
  sku: string;
  lot: string;
  quantity: number;
  received: number;
  lost: number;
};
type Transfer = {
  id: string;
  reference: string;
  destination_warehouse_id: string;
  lines: Line[];
  ok: boolean;
};
type Record = {
  id: string;
  transfer_id: string;
  line_id: string;
  warehouse_id: string;
  kind: "loss" | "reversal";
  quantity: number;
  reversed: number;
  original_loss_id: string | null;
  incident_reference: string | null;
  reason: string;
  actor: string;
  recorded_at: string;
  version: number;
  event_id: string;
  source_ok: boolean;
};
type Page = {
  warehouse_id: string;
  transfer_id: string;
  destination_warehouse_id: string;
  version: number;
  review: number;
  items: Record[];
  next_cursor: number | null;
  page_source_ok: boolean;
};
const whole = (v: unknown): v is number =>
  Number.isSafeInteger(v) && (v as number) >= 0;
const text = (v: unknown): v is string =>
  typeof v === "string" &&
  !!v.trim() &&
  v.trim() === v &&
  !/[\x00-\x1f\x7f]/.test(v);

export default function TransferLossPanel({
  packs,
  transfer: t,
  warehouse,
  version,
  admin,
  disabled,
  request,
  onSubmit,
  onChanged,
}: {
  packs?: UnitPack[];
  transfer: Transfer;
  warehouse: string;
  version: number;
  admin: boolean;
  disabled: boolean;
  request: (path: string) => Promise<any>;
  onSubmit: (
    command: Command,
    version: number,
    versions?: { [key: string]: number },
    inputs?: UnitInput[],
  ) => Promise<boolean>;
  onChanged: () => void;
}) {
  const [page, setPage] = useState<Page | null>(null),
    [rows, setRows] = useState<Record[]>([]);
  const [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const [form, setForm] = useState<{
    line: Line;
    original?: Record;
    version: number;
  } | null>(null);
  const [quantity, setQuantity] = useState("1"),
    [incident, setIncident] = useState(""),
    [reason, setReason] = useState(""),
    [confirmed, setConfirmed] = useState(false);
  const [unitInput, setUnitInput] = useState<UnitInput>();
  const alive = useRef(true),
    sequence = useRef(0);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      ++sequence.current;
    };
  }, []);
  useEffect(() => {
    ++sequence.current;
    setPage(null);
    setRows([]);
    setForm(null);
    setBusy(false);
  }, [version]);
  const load = async (after?: number) => {
    if (busy || disabled) return;
    const current = ++sequence.current;
    setBusy(true);
    setError("");
    setForm(null);
    try {
      const q = new URLSearchParams({ limit: "25" });
      if (after !== undefined) {
        if (!page) return;
        q.set("after", String(after));
        q.set("review", String(page.review));
      }
      const result: Page = await request(
        `/v1/warehouses/${encodeURIComponent(warehouse)}/transfers/${encodeURIComponent(t.id)}/losses?${q}`,
      );
      if (!alive.current || current !== sequence.current) return;
      if (
        result.warehouse_id !== warehouse ||
        result.transfer_id !== t.id ||
        result.destination_warehouse_id !== t.destination_warehouse_id ||
        result.version !== version ||
        !whole(result.review) ||
        (warehouse === t.destination_warehouse_id &&
          result.review !== result.version) ||
        (after !== undefined && result.review !== page?.review) ||
        typeof result.page_source_ok !== "boolean" ||
        !Array.isArray(result.items) ||
        result.items.length > 25 ||
        (result.next_cursor !== null &&
          (!whole(result.next_cursor) || result.next_cursor <= (after ?? 0))) ||
        result.items.some(
          (r, n) =>
            ![
              r.id,
              r.line_id,
              r.reason,
              r.actor,
              r.event_id,
              r.recorded_at,
            ].every(text) ||
            r.transfer_id !== t.id ||
            r.warehouse_id !== t.destination_warehouse_id ||
            !t.lines.some((l) => l.id === r.line_id) ||
            !["loss", "reversal"].includes(r.kind) ||
            !whole(r.quantity) ||
            r.quantity < 1 ||
            r.quantity > 1000000 ||
            !whole(r.reversed) ||
            r.reversed > r.quantity ||
            !whole(r.version) ||
            r.version <= (n ? result.items[n - 1].version : (after ?? 0)) ||
            r.version > result.review ||
            !Number.isFinite(Date.parse(r.recorded_at)) ||
            typeof r.source_ok !== "boolean" ||
            (r.kind === "loss"
              ? !text(r.incident_reference) || r.original_loss_id !== null
              : !text(r.original_loss_id) ||
                r.incident_reference !== null ||
                r.reversed !== 0),
        ) ||
        (result.next_cursor !== null &&
          result.next_cursor !== result.items.at(-1)?.version) ||
        (result.page_source_ok && result.items.some((r) => !r.source_ok))
      )
        throw Error(
          "Loss journal could not be verified. Refresh transfers and review again.",
        );
      const previous = after !== undefined ? rows : [];
      if (result.items.some((r) => previous.some((p) => p.id === r.id)))
        throw Error(
          "Loss journal repeated a record. Refresh the complete review.",
        );
      setPage(result);
      setRows([...previous, ...result.items]);
    } catch (failure) {
      if (alive.current && current === sequence.current) {
        setPage(null);
        setRows([]);
        setForm(null);
        setError((failure as Error).message);
      }
    } finally {
      if (alive.current && current === sequence.current) setBusy(false);
    }
  };
  const start = (line: Line, original?: Record) => {
    if (
      !page?.page_source_ok ||
      !t.ok ||
      !admin ||
      warehouse !== t.destination_warehouse_id ||
      page.version !== version
    )
      return;
    setForm({ line, original, version: page.version });
    setQuantity("1");
    setUnitInput(undefined);
    setIncident("");
    setReason("");
    setConfirmed(false);
    setError("");
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!form || busy || disabled || !confirmed || !page?.page_source_ok)
      return;
    const max = form.original
      ? form.original.quantity - form.original.reversed
      : form.line.quantity - form.line.received - form.line.lost;
    if (
      !/^[1-9]\d*$/.test(quantity) ||
      !Number.isSafeInteger(Number(quantity)) ||
      Number(quantity) > max ||
      !text(reason) ||
      reason.length > 240 ||
      (!form.original && (!text(incident) || incident.length > 240))
    ) {
      setError(
        "Enter an exact whole-each quantity within this original allowance and the reviewed evidence.",
      );
      return;
    }
    setBusy(true);
    setError("");
    const command: Command = form.original
      ? {
          type: "reverse_transfer_loss",
          loss_id: form.original.id,
          quantity: Number(quantity),
          reason,
        }
      : {
          type: "record_transfer_loss",
          transfer_id: t.id,
          line_id: form.line.id,
          quantity: Number(quantity),
          incident_reference: incident,
          reason,
        };
    try {
      if (
        await onSubmit(
          command,
          form.version,
          undefined,
          unitInput ? [unitInput] : undefined,
        )
      ) {
        if (alive.current) {
          setForm(null);
          setPage(null);
          setRows([]);
          onChanged();
        }
      }
    } catch (failure) {
      if (alive.current) setError((failure as Error).message);
    } finally {
      if (alive.current) setBusy(false);
    }
  };
  return (
    <section
      className="transfer-loss-journal"
      aria-label={`Loss records for ${t.reference}`}
    >
      <details>
        <summary>Loss settlements and reversals</summary>
        <p>
          A confirmed loss settles only missing in-transit units. Neither a loss
          nor its reversal changes warehouse stock. Reversing reopens the
          original quantity pending actual arrival.
        </p>
        <button
          className="btn"
          disabled={disabled || busy}
          onClick={() => void load()}
        >
          Review loss journal
        </button>
        {error && (
          <p role="alert" className="workflow-error">
            {error} Retained pages have been discarded.
          </p>
        )}
        {page && !page.page_source_ok && (
          <p role="alert">
            Original transfer or loss evidence needs reconciliation. Loss
            changes are held.
          </p>
        )}
        {rows.map((r) => (
          <article className="transfer-loss-record" key={r.id}>
            <strong>
              {r.kind === "loss" ? "Recorded loss" : "Original loss reversal"} ·{" "}
              {r.quantity} each · Line {r.line_id}
            </strong>
            <p>
              {r.id} · {r.actor} · {r.recorded_at}
            </p>
            <p>
              {r.kind === "loss"
                ? `Incident ${r.incident_reference} · Reversed ${r.reversed} each`
                : `Original loss ${r.original_loss_id}`}{" "}
              · {r.reason}
            </p>
            <p>
              {r.source_ok
                ? "Matches original event and command receipt"
                : "Original source check failed"}{" "}
              · Event {r.event_id}
            </p>
            {r.kind === "loss" &&
              r.quantity > r.reversed &&
              admin &&
              warehouse === t.destination_warehouse_id && (
                <button
                  className="btn"
                  disabled={disabled || busy || !page?.page_source_ok || !t.ok}
                  onClick={() =>
                    start(t.lines.find((l) => l.id === r.line_id)!, r)
                  }
                >
                  Review reversal of loss {r.id}
                </button>
              )}
          </article>
        ))}
        {page?.next_cursor !== null && page && (
          <button
            className="btn"
            disabled={disabled || busy}
            onClick={() => void load(page.next_cursor!)}
          >
            Load older loss records
          </button>
        )}
        {page && rows.length === 0 && <p>No loss records in this review.</p>}
        {page &&
          admin &&
          warehouse === t.destination_warehouse_id &&
          t.lines
            .filter((l) => l.quantity > l.received + l.lost)
            .map((l) => (
              <button
                key={l.id}
                className="btn"
                disabled={disabled || busy || !page.page_source_ok || !t.ok}
                onClick={() => start(l)}
              >
                Review loss for line {l.id}
              </button>
            ))}
        {form && (
          <form
            className="workflow-form"
            onSubmit={(event) => void submit(event)}
          >
            <h4>
              {form.original
                ? "Reverse original transfer loss"
                : "Record confirmed transfer loss"}
            </h4>
            <p>
              {t.reference} · {form.line.id} · {form.line.sku} · {form.line.lot}{" "}
              · Destination {warehouse}
            </p>
            {form.original && (
              <p>
                Original loss {form.original.id} ·{" "}
                {form.original.incident_reference}
              </p>
            )}
            <UnitQuantityInput
              key={`${form.line.id}:${form.original?.id ?? "loss"}`}
              packs={packs}
              sku={form.line.sku}
              target={[]}
              value={quantity}
              label="Loss quantity each"
              max={
                form.original
                  ? form.original.quantity - form.original.reversed
                  : form.line.quantity - form.line.received - form.line.lost
              }
              disabled={disabled || busy}
              onChange={(each, input) => {
                setQuantity(each);
                setUnitInput(input);
                setConfirmed(false);
              }}
            />
            {!form.original && (
              <label>
                Carrier incident reference
                <input
                  maxLength={240}
                  required
                  disabled={disabled || busy}
                  value={incident}
                  onChange={(e) => {
                    setIncident(e.target.value);
                    setConfirmed(false);
                  }}
                />
              </label>
            )}
            <label>
              Loss review reason
              <input
                maxLength={240}
                required
                disabled={disabled || busy}
                value={reason}
                onChange={(e) => {
                  setReason(e.target.value);
                  setConfirmed(false);
                }}
              />
            </label>
            <label className="workflow-checkbox loss-confirmation">
              <input
                type="checkbox"
                required
                disabled={disabled || busy}
                checked={confirmed}
                onChange={(e) => setConfirmed(e.target.checked)}
              />
              I reviewed this original line, quantity and incident evidence. No
              warehouse stock will be credited.
            </label>
            <button
              className="btn primary"
              disabled={disabled || busy || !confirmed}
            >
              {form.original ? "Record loss reversal" : "Record confirmed loss"}
            </button>
            <button
              type="button"
              className="btn"
              disabled={disabled || busy}
              onClick={() => setForm(null)}
            >
              Close loss review
            </button>
          </form>
        )}
      </details>
    </section>
  );
}
