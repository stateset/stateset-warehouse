import { useState, type FormEvent } from "react";
import type { Command, UnitInput, UnitPack } from "./types";
import "./Workflows.css";
import UnitQuantityInput from "./UnitQuantityInput";

type Allowance = {
  order_line_id: string;
  sku: string;
  lot: string;
  remaining: number;
};
type Review = {
  order_id: string;
  version: number;
  lines: Allowance[];
  shipment_source?: {
    kind: string;
    event_id: string | null;
    event_ids?: string[];
    version: number;
  };
};
export default function ReturnForm({
  packs,
  warehouse,
  orderId,
  disabled,
  request,
  onSubmit,
}: {
  packs?: UnitPack[];
  warehouse: string;
  orderId: string;
  disabled: boolean;
  request: (path: string, body?: unknown) => Promise<any>;
  onSubmit: (
    command: Command,
    version: number,
    versions?: Record<string, number>,
    inputs?: UnitInput[],
  ) => Promise<boolean>;
}) {
  const [review, setReview] = useState<Review | null>(null);
  const [selection, setSelection] = useState("");
  const [reference, setReference] = useState("");
  const [reason, setReason] = useState("");
  const [lineId, setLineId] = useState(crypto.randomUUID());
  const [unitInput, setUnitInput] = useState<UnitInput>();
  const [quantity, setQuantity] = useState("1");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const selected = review?.lines.find(
    (l) => JSON.stringify([l.order_line_id, l.lot]) === selection,
  );
  const load = async () => {
    setBusy(true);
    setError("");
    setReview(null);
    setLineId(crypto.randomUUID());
    setUnitInput(undefined);
    try {
      const result: Review = await request(
        `/v1/warehouses/${encodeURIComponent(warehouse)}/orders/${encodeURIComponent(orderId)}/return-eligibility`,
      );
      if (
        result.order_id !== orderId ||
        !Number.isSafeInteger(result.version) ||
        !Array.isArray(result.lines)
      )
        throw new Error("Return allowance could not be verified.");
      const source = result.shipment_source;
      if (
        source !== undefined &&
        (!source ||
          typeof source !== "object" ||
          ![
            "event",
            "legacy_snapshot",
            "synthetic_fixture",
            "handoff_events",
          ].includes(source.kind) ||
          !Number.isSafeInteger(source.version) ||
          source.version < 0 ||
          (source.kind === "handoff_events" &&
            (!Array.isArray(source.event_ids) ||
              source.event_ids.length === 0 ||
              source.event_ids.some((id) => typeof id !== "string" || !id) ||
              new Set(source.event_ids).size !== source.event_ids.length)) ||
          (source.kind === "event"
            ? typeof source.event_id !== "string" || !source.event_id
            : source.event_id !== null))
      )
        throw new Error("Return allowance source could not be verified.");
      setReview(result);
      const first = result.lines.find((l) => l.remaining > 0);
      setSelection(
        first ? JSON.stringify([first.order_line_id, first.lot]) : "",
      );
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!review || !selected || busy || disabled) return;
    if (
      !/^[1-9]\d*$/.test(quantity) ||
      !Number.isSafeInteger(Number(quantity)) ||
      Number(quantity) > selected.remaining
    ) {
      setError(
        "Quantity must fit this exact line and lot's remaining return allowance.",
      );
      return;
    }
    setBusy(true);
    setError("");
    try {
      await onSubmit(
        {
          type: "create_return",
          order_id: orderId,
          reference,
          reason,
          lines: [
            {
              id: lineId,
              order_line_id: selected.order_line_id,
              lot: selected.lot,
              quantity: Number(quantity),
            },
          ],
        },
        review.version,
        undefined,
        unitInput ? [unitInput] : undefined,
      );
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className="workflow-panel" aria-label="Customer return">
      <h3>Customer return</h3>
      <p>
        Authorize an exact shipped line and lot. Receive the goods against the
        new RMA, inspect them, then put approved units away. Authorization
        changes no stock or commercial refund.
      </p>
      <button
        type="button"
        className="btn"
        disabled={disabled || busy}
        onClick={() => void load()}
      >
        Review return allowance
      </button>
      {error && (
        <p role="alert" className="workflow-error">
          {error}
        </p>
      )}
      {review?.shipment_source && (
        <p className="detail-note">
          {review.shipment_source.kind === "synthetic_fixture"
            ? "Synthetic shipment fixture — use for testing only."
            : review.shipment_source.kind === "legacy_snapshot"
              ? "Allowance comes from a retained legacy snapshot. Verify the original handoff before receiving returned goods."
              : review.shipment_source.kind === "handoff_events"
                ? "Allowance checked against the recorded shipment handoffs. Unshipped units are excluded."
                : "Allowance checked against the original recorded handoff."}
        </p>
      )}
      {review &&
        (review.lines.some((l) => l.remaining > 0) ? (
          <form onSubmit={submit} className="workflow-form">
            <label>
              RMA reference
              <input
                required
                maxLength={240}
                value={reference}
                onChange={(e) => setReference(e.target.value)}
              />
            </label>
            <label>
              Original line and lot
              <select
                aria-label="Original line and lot"
                required
                value={selection}
                onChange={(e) => {
                  setSelection(e.target.value);
                  setUnitInput(undefined);
                  setQuantity("1");
                }}
              >
                {review.lines
                  .filter((l) => l.remaining > 0)
                  .map((l) => (
                    <option
                      key={JSON.stringify([l.order_line_id, l.lot])}
                      value={JSON.stringify([l.order_line_id, l.lot])}
                    >
                      {l.order_line_id} · {l.sku} · {l.lot} · {l.remaining}{" "}
                      remaining
                    </option>
                  ))}
              </select>
            </label>
            <UnitQuantityInput
              key={selection}
              packs={packs}
              sku={selected?.sku ?? ""}
              target={[lineId]}
              value={quantity}
              label="Return quantity (each)"
              max={Math.min(selected?.remaining ?? 1, 1000000)}
              disabled={disabled || busy}
              onChange={(each, input) => {
                setQuantity(each);
                setUnitInput(input);
              }}
            />
            <label className="workflow-wide">
              Return reason / inspection evidence
              <textarea
                required
                maxLength={240}
                value={reason}
                onChange={(e) => setReason(e.target.value)}
              />
            </label>
            <button
              className="btn primary"
              disabled={disabled || busy}
              type="submit"
            >
              Authorize return receipt
            </button>
          </form>
        ) : (
          <p>Every shipped unit already has a return authorization.</p>
        ))}
    </section>
  );
}
