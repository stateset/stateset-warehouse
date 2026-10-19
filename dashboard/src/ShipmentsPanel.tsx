import { useEffect, useRef, useState, type FormEvent } from "react";
import type {
  Allocation,
  Command,
  Order,
  WarehouseState,
  UnitInput,
} from "./types";
import "./Workflows.css";
import UnitQuantityInput from "./UnitQuantityInput";
import { availability, availabilityLabel } from "./lotAvailability";
type PackedLine = {
  line_id: string;
  sku: string;
  location: string;
  lot: string;
  quantity: number;
};
type Package = {
  id: string;
  order_id: string;
  reference: string;
  lines: PackedLine[];
  closure: null | {
    kind: "handed_off" | "unpacked";
    carrier: string;
    handoff_reference: string;
    reason: string;
    closed_at: string;
    actor: string;
  };
};
type View = {
  warehouse_id: string;
  order_id: string;
  version: number;
  ok: boolean;
  order: Order;
  packages: Package[];
  progress: {
    line_id: string;
    sku: string;
    ordered: number;
    allocated: number;
    picked: number;
    shipped: number;
    cancelled?: number;
    unallocated: number;
  }[];
};
const allocationKey = (a: Allocation) =>
  JSON.stringify([a.line_id, a.location, a.lot]);
export default function ShipmentsPanel({
  warehouse,
  orderId,
  disabled,
  forwardBlocked = false,
  state,
  at,
  canWork,
  request,
  refreshWarehouse,
  onSubmit,
}: {
  warehouse: string;
  orderId: string;
  disabled: boolean;
  forwardBlocked?: boolean;
  state: WarehouseState;
  at: bigint | undefined;
  canWork: boolean;
  request: (path: string, body?: unknown) => Promise<any>;
  refreshWarehouse: (warehouse: string) => Promise<WarehouseState | undefined>;
  onSubmit: (
    command: Command,
    version: number,
    versions?: Record<string, number>,
    inputs?: UnitInput[],
  ) => Promise<boolean>;
}) {
  const [view, setView] = useState<View | null>(null),
    [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const [reference, setReference] = useState(""),
    [quantities, setQuantities] = useState<Record<string, string>>({}),
    [reason, setReason] = useState("");
  const [unitInputs, setUnitInputs] = useState<
    Record<string, UnitInput | undefined>
  >({});
  const [cancelReason, setCancelReason] = useState(""),
    [cancelConfirmed, setCancelConfirmed] = useState(false);
  const [selected, setSelected] = useState<{
      id: string;
      action: "handoff" | "unpack";
    } | null>(null),
    [carrier, setCarrier] = useState(""),
    [handoff, setHandoff] = useState("");
  const alive = useRef(true),
    sequence = useRef(0);
  const load = async (refreshPolicy = false) => {
    const current = ++sequence.current;
    setBusy(true);
    setError("");
    setView(null);
    setSelected(null);
    setQuantities({});
    setUnitInputs({});
    setCancelConfirmed(false);
    try {
      // A fresh shipment version cannot authorize work using an older lot policy.
      // Refresh both observations only after the operator explicitly requests it.
      if (refreshPolicy) {
        await refreshWarehouse(warehouse);
        if (!alive.current || current !== sequence.current) return;
      }
      const result: View = await request(
        `/v1/warehouses/${encodeURIComponent(warehouse)}/orders/${encodeURIComponent(orderId)}/shipments`,
      );
      if (!alive.current || current !== sequence.current) return;
      if (
        result.warehouse_id !== warehouse ||
        result.order_id !== orderId ||
        result.order?.id !== orderId ||
        !Number.isSafeInteger(result.version) ||
        result.version < 0 ||
        typeof result.ok !== "boolean" ||
        !Array.isArray(result.packages) ||
        !Array.isArray(result.progress) ||
        !Array.isArray(result.order.allocations) ||
        !Array.isArray(result.order.lines) ||
        result.order.allocations.some(
          (a) =>
            !result.order.lines.some(
              (l) => l.id === a.line_id && l.sku === a.sku,
            ) ||
            ![a.quantity, a.picked, a.shipped, a.released].every(
              Number.isSafeInteger,
            ) ||
            a.quantity <= 0 ||
            a.quantity > 1000000 ||
            a.picked < 0 ||
            a.shipped < 0 ||
            a.released < 0 ||
            a.shipped > a.picked ||
            a.picked + a.released > a.quantity,
        ) ||
        result.progress.length !== result.order.lines.length ||
        result.progress.some(
          (l) =>
            !result.order.lines.some(
              (original) =>
                original.id === l.line_id &&
                original.sku === l.sku &&
                original.quantity === l.ordered,
            ),
        ) ||
        new Set(result.packages.map((p) => p.id)).size !==
          result.packages.length ||
        result.progress.some(
          (l) =>
            ![
              l.ordered,
              l.allocated,
              l.picked,
              l.shipped,
              l.unallocated,
              l.cancelled ?? 0,
            ].every(Number.isSafeInteger) ||
            l.ordered <= 0 ||
            l.shipped < 0 ||
            l.shipped > l.picked ||
            l.picked > l.allocated ||
            l.allocated > l.ordered ||
            (l.cancelled ?? 0) < 0 ||
            l.allocated + (l.cancelled ?? 0) > l.ordered ||
            ((l.cancelled ?? 0) > 0 &&
              !result.order.remainder_cancellation &&
              result.order.status !== "cancelled") ||
            l.unallocated !== l.ordered - l.allocated - (l.cancelled ?? 0),
        ) ||
        result.progress.some((l) => {
          const exact = result.order.allocations.filter(
            (a) => a.line_id === l.line_id,
          );
          return (
            l.allocated !==
              exact.reduce((n, a) => n + a.quantity - a.released, 0) ||
            l.picked !== exact.reduce((n, a) => n + a.picked, 0) ||
            l.shipped !== exact.reduce((n, a) => n + a.shipped, 0)
          );
        }) ||
        new Set(result.progress.map((l) => l.line_id)).size !==
          result.progress.length ||
        result.packages.some(
          (p) =>
            typeof p.id !== "string" ||
            !p.id ||
            p.order_id !== orderId ||
            (p.closure !== null &&
              (!p.closure ||
                !["unpacked", "handed_off"].includes(p.closure.kind))) ||
            !Array.isArray(p.lines) ||
            p.lines.length === 0 ||
            p.lines.length > 100 ||
            p.lines.some(
              (l) =>
                ![l.line_id, l.sku, l.location, l.lot].every(
                  (v) => typeof v === "string" && !!v,
                ) ||
                !Number.isSafeInteger(l.quantity) ||
                l.quantity <= 0 ||
                l.quantity > 1000000 ||
                !result.order.allocations.some(
                  (a) =>
                    a.line_id === l.line_id &&
                    a.sku === l.sku &&
                    a.location === l.location &&
                    a.lot === l.lot,
                ),
            ) ||
            new Set(
              p.lines.map((l) =>
                JSON.stringify([l.line_id, l.location, l.lot]),
              ),
            ).size !== p.lines.length,
        ) ||
        result.order.allocations.some(
          (a) =>
            result.packages
              .filter((p) => !p.closure)
              .flatMap((p) => p.lines)
              .filter(
                (l) =>
                  l.line_id === a.line_id &&
                  l.location === a.location &&
                  l.lot === a.lot,
              )
              .reduce((n, l) => n + l.quantity, 0) >
            a.picked - a.shipped,
        )
      )
        throw Error("Shipment ownership review could not be verified.");
      const closure = result.order.remainder_cancellation;
      if (
        result.order.status === "cancelled" &&
        (result.progress.some(
          (l) =>
            l.cancelled !== l.ordered ||
            l.shipped !== 0 ||
            l.picked !== 0 ||
            l.allocated !== 0,
        ) ||
          result.packages.some((p) => p.closure?.kind !== "unpacked"))
      )
        throw Error("Original cancellation review could not be verified.");
      if (result.order.status === "closed_partial" || closure) {
        if (
          !closure ||
          result.order.status !== "closed_partial" ||
          !closure.reason?.trim() ||
          !closure.actor?.trim() ||
          !Number.isFinite(Date.parse(closure.cancelled_at)) ||
          !closure.lines ||
          Object.keys(closure.lines).length !== result.progress.length ||
          result.progress.some((l) => {
            const original = closure.lines[l.line_id];
            return (
              !original ||
              original.sku !== l.sku ||
              original.ordered !== l.ordered ||
              original.shipped !== l.shipped ||
              original.cancelled !== l.cancelled ||
              original.shipped + original.cancelled !== original.ordered
            );
          }) ||
          !result.progress.some((l) => (l.cancelled ?? 0) > 0) ||
          !result.progress.some((l) => l.shipped > 0) ||
          result.order.allocations.some(
            (a) =>
              a.picked !== a.shipped || a.released !== a.quantity - a.shipped,
          ) ||
          result.packages.some((p) => !p.closure)
        )
          throw Error("Original remainder cancellation could not be verified.");
      }
      setView(result);
    } catch (failure) {
      if (alive.current && current === sequence.current)
        setError((failure as Error).message);
    } finally {
      if (alive.current && current === sequence.current) setBusy(false);
    }
  };
  useEffect(() => {
    alive.current = true;
    void load();
    return () => {
      alive.current = false;
      ++sequence.current;
    };
  }, [warehouse, orderId, request]);
  const remaining = (a: Allocation) =>
    a.picked -
    a.shipped -
    (view?.packages
      .filter((p) => !p.closure)
      .flatMap((p) => p.lines)
      .filter(
        (l) =>
          l.line_id === a.line_id &&
          l.location === a.location &&
          l.lot === a.lot,
      )
      .reduce((n, l) => n + l.quantity, 0) ?? 0);
  const lotStatus = (line: { sku: string; lot: string }) =>
    availability(state, line.sku, line.lot, at);
  const eligible = (line: { sku: string; lot: string }) =>
    lotStatus(line) === "available";
  const stale = !!view && view.version !== state.version;
  const blocked = disabled || busy || !view || !view.ok || stale;
  const selectedUnavailable =
    !!view &&
    view.order.allocations.some(
      (a) => Number(quantities[allocationKey(a)] || 0) > 0 && !eligible(a),
    );
  const selectedHandoffHeld =
    selected?.action === "handoff" &&
    !!view?.packages
      .find((p) => p.id === selected.id)
      ?.lines.some((l) => !eligible(l));
  const done =
    view &&
    ["shipped", "cancelled", "closed_partial"].includes(view.order.status);
  const cancellationHeld =
    !!view &&
    (view.packages.some((p) => !p.closure) ||
      view.order.allocations.some((a) => a.picked !== a.shipped));
  const save = async (command: Command, inputs?: UnitInput[]) => {
    if (
      blocked ||
      !view ||
      (forwardBlocked &&
        ["allocate_available", "pack_shipment", "handoff_shipment"].includes(
          command.type,
        ))
    )
      return false;
    if (
      (command.type === "pack_shipment" && selectedUnavailable) ||
      (command.type === "handoff_shipment" &&
        view.packages
          .find((p) => p.id === command.shipment_id)
          ?.lines.some((l) => !eligible(l)))
    ) {
      setError(
        "An unavailable lot cannot be packed or handed off. Unpack and physical recovery remain available.",
      );
      return false;
    }
    setBusy(true);
    setError("");
    try {
      const ok = await onSubmit(command, view.version, undefined, inputs);
      if (ok && alive.current) void load();
      return ok;
    } catch (failure) {
      if (alive.current) setError((failure as Error).message);
      return false;
    } finally {
      if (alive.current) setBusy(false);
    }
  };
  const pack = async (event: FormEvent) => {
    event.preventDefault();
    if (!view) return;
    const lines = view.order.allocations.flatMap((a) => {
      const quantity = Number(quantities[allocationKey(a)] || 0);
      return quantity > 0
        ? [{ line_id: a.line_id, location: a.location, lot: a.lot, quantity }]
        : [];
    });
    if (
      !lines.length ||
      lines.length > 100 ||
      lines.some((l) => !Number.isSafeInteger(l.quantity))
    ) {
      setError(
        "Select 1–100 exact allocation lines and whole each quantities.",
      );
      return;
    }
    await save(
      { type: "pack_shipment", order_id: orderId, reference, lines },
      lines.flatMap((l) => {
        const i = unitInputs[JSON.stringify([l.line_id, l.location, l.lot])];
        return i ? [i] : [];
      }),
    );
  };
  return (
    <section
      className="workflow-panel"
      aria-label="Shipment packages and backorders"
    >
      <h3>Shipment packages and backorders</h3>
      <p>
        Allocate available storage stock, pick exact units, pack a shipment,
        then record its actual carrier handoff. Unavailable lines stay open.
        Every package keeps its original lines, lots and handoff.
      </p>
      {error && (
        <p role="alert" className="workflow-error">
          {error}
        </p>
      )}
      <button
        type="button"
        className="btn"
        disabled={busy || disabled}
        onClick={() => void load(true)}
      >
        Refresh shipment review
      </button>
      {stale && (
        <p role="status">
          Warehouse changed. Refresh shipment review before making another
          change.
        </p>
      )}
      {view && !view.ok && (
        <p role="alert">
          Packing or handoff evidence needs reconciliation. Further changes are
          held.
        </p>
      )}
      {view && (
        <>
          <div className="table-scroll">
            <table>
              <thead>
                <tr>
                  <th>Original line / SKU</th>
                  <th>Ordered</th>
                  <th>Allocated</th>
                  <th>Picked</th>
                  <th>Handed off</th>
                  <th>Unallocated</th>
                  <th>Cancelled remainder</th>
                </tr>
              </thead>
              <tbody>
                {view.progress.map((l) => (
                  <tr key={l.line_id}>
                    <td>
                      {l.line_id}
                      <span className="subline">{l.sku}</span>
                    </td>
                    <td>{l.ordered}</td>
                    <td>{l.allocated}</td>
                    <td>{l.picked}</td>
                    <td>{l.shipped}</td>
                    <td>{l.unallocated}</td>
                    <td>{l.cancelled ?? 0}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {view.order.remainder_cancellation && (
            <div className="alert warning" role="status">
              <p>
                <strong>Unshipped remainder cancelled</strong>
                <br />
                {view.order.remainder_cancellation.reason}
                <br />
                {view.order.remainder_cancellation.actor} ·{" "}
                {new Date(
                  view.order.remainder_cancellation.cancelled_at,
                ).toLocaleString()}
                <br />
                Original ordered quantities and actual handoffs remain retained.
                Returns use only units actually handed off.
              </p>
            </div>
          )}
          {canWork && view.order.status === "partially_shipped" && (
            <form
              className="workflow-form"
              onSubmit={(event) => {
                event.preventDefault();
                if (
                  !cancelConfirmed ||
                  cancellationHeld ||
                  !cancelReason.trim() ||
                  done
                )
                  return;
                void save({
                  type: "cancel_remainder",
                  order_id: orderId,
                  reason: cancelReason.trim(),
                });
              }}
            >
              <h4 className="workflow-wide">Cancel the unshipped remainder</h4>
              <p className="workflow-wide">
                Closes{" "}
                {view.progress.reduce((n, l) => n + l.ordered - l.shipped, 0)}{" "}
                original each units that have not departed. Storage reservations
                are released. Existing handoffs and return allowance stay
                unchanged.
              </p>
              {cancellationHeld && (
                <p role="status" className="workflow-wide">
                  Unpack every active package and physically return every
                  unshipped staged unit before cancelling the remainder.
                </p>
              )}
              <label className="workflow-wide">
                Remainder cancellation reason
                <textarea
                  required
                  maxLength={240}
                  disabled={blocked}
                  value={cancelReason}
                  onChange={(event) => setCancelReason(event.target.value)}
                />
              </label>
              <label className="workflow-wide">
                <input
                  type="checkbox"
                  disabled={blocked || cancellationHeld}
                  checked={cancelConfirmed}
                  onChange={(event) => setCancelConfirmed(event.target.checked)}
                />
                I confirmed cancellation of all remaining original demand.
              </label>
              <button
                className="btn"
                disabled={
                  blocked ||
                  cancellationHeld ||
                  !cancelConfirmed ||
                  !cancelReason.trim()
                }
              >
                Cancel unshipped remainder
              </button>
            </form>
          )}
          {canWork && !done && (
            <>
              <button
                type="button"
                className="btn"
                disabled={
                  blocked ||
                  forwardBlocked ||
                  !view.progress.some((l) => l.unallocated > 0)
                }
                onClick={() =>
                  void save({ type: "allocate_available", order_id: orderId })
                }
              >
                Allocate available stock
              </button>
              <form className="workflow-form" onSubmit={pack}>
                <h4 className="workflow-wide">Confirm a packed shipment</h4>
                <label className="workflow-wide">
                  Package reference
                  <input
                    required
                    maxLength={240}
                    disabled={blocked}
                    value={reference}
                    onChange={(e) => setReference(e.target.value)}
                  />
                </label>
                {view.order.allocations
                  .filter((a) => remaining(a) > 0)
                  .map((a) => (
                    <div key={allocationKey(a)}>
                      <p>
                        {a.line_id} · {a.sku} · {a.lot} · {a.location} ·{" "}
                        {remaining(a)} unpacked each ·{" "}
                        {availabilityLabel[lotStatus(a)]}
                      </p>
                      <UnitQuantityInput
                        packs={state.unit_packs}
                        sku={a.sku}
                        target={[a.line_id, a.location, a.lot]}
                        label={`Package each units ${a.line_id} ${a.location} ${a.lot}`}
                        min={0}
                        max={remaining(a)}
                        disabled={blocked || !eligible(a)}
                        value={quantities[allocationKey(a)] ?? "0"}
                        onChange={(each, input) => {
                          setQuantities((q) => ({
                            ...q,
                            [allocationKey(a)]: each,
                          }));
                          setUnitInputs((q) => ({
                            ...q,
                            [allocationKey(a)]: input,
                          }));
                        }}
                      />
                    </div>
                  ))}
                <button
                  className="btn primary"
                  disabled={
                    blocked ||
                    forwardBlocked ||
                    selectedUnavailable ||
                    !view.order.allocations.some(
                      (a) => remaining(a) > 0 && eligible(a),
                    )
                  }
                >
                  Confirm packed shipment
                </button>
              </form>
              {view.order.shipment_mode === "packages" && (
                <div className="workflow-form">
                  <label className="workflow-wide">
                    Reservation release reason
                    <textarea
                      maxLength={240}
                      disabled={blocked}
                      value={reason}
                      onChange={(e) => setReason(e.target.value)}
                    />
                  </label>
                  <button
                    type="button"
                    className="btn"
                    disabled={
                      blocked ||
                      !reason.trim() ||
                      !view.order.allocations.some(
                        (a) => a.quantity > a.picked + a.released,
                      )
                    }
                    onClick={() =>
                      void save({
                        type: "release_unpicked",
                        order_id: orderId,
                        reason,
                      })
                    }
                  >
                    Release unpicked reservations
                  </button>
                  <p>
                    Picked and handed-off units retain their ownership. Unpack a
                    shipment before returning its staged stock.
                  </p>
                </div>
              )}
            </>
          )}
          {view.packages.map((p) => (
            <article key={p.id} className="workflow-task">
              <div>
                <strong>{p.reference}</strong>
                <p>
                  {p.closure?.kind === "handed_off"
                    ? `Handed off · ${p.closure.carrier} · ${p.closure.handoff_reference}`
                    : p.closure?.kind === "unpacked"
                      ? `Unpacked · ${p.closure.reason}`
                      : "Packed — staged units protected"}
                </p>
                <ul>
                  {p.lines.map((l) => (
                    <li key={JSON.stringify([l.line_id, l.location, l.lot])}>
                      {l.line_id} · {l.sku} · {l.lot} · {l.location} ·{" "}
                      {l.quantity} each
                      {!p.closure && ` · ${availabilityLabel[lotStatus(l)]}`}
                    </li>
                  ))}
                </ul>
              </div>
              {canWork && !p.closure && (
                <div>
                  <button
                    type="button"
                    className="btn"
                    disabled={
                      blocked ||
                      forwardBlocked ||
                      p.lines.some((l) => !eligible(l))
                    }
                    onClick={() => {
                      setSelected({ id: p.id, action: "handoff" });
                      setCarrier("");
                      setHandoff("");
                    }}
                  >
                    Hand off {p.reference}
                  </button>{" "}
                  <button
                    type="button"
                    className="btn"
                    disabled={blocked}
                    onClick={() => {
                      setSelected({ id: p.id, action: "unpack" });
                      setReason("");
                    }}
                  >
                    Unpack {p.reference}
                  </button>
                </div>
              )}
            </article>
          ))}
          {selected && (
            <form
              className="workflow-form"
              onSubmit={(e) => {
                e.preventDefault();
                void save(
                  selected.action === "handoff"
                    ? {
                        type: "handoff_shipment",
                        shipment_id: selected.id,
                        carrier,
                        handoff_reference: handoff,
                      }
                    : {
                        type: "unpack_shipment",
                        shipment_id: selected.id,
                        reason,
                      },
                );
              }}
            >
              {selected.action === "handoff" ? (
                <>
                  <label>
                    Shipment carrier
                    <input
                      required
                      maxLength={240}
                      disabled={blocked}
                      value={carrier}
                      onChange={(e) => setCarrier(e.target.value)}
                    />
                  </label>
                  <label>
                    Shipment handoff reference
                    <input
                      required
                      maxLength={240}
                      disabled={blocked}
                      value={handoff}
                      onChange={(e) => setHandoff(e.target.value)}
                    />
                  </label>
                  <label className="workflow-wide workflow-checkbox">
                    <input required type="checkbox" disabled={blocked} />{" "}
                    Carrier has physically received this exact package
                  </label>
                </>
              ) : (
                <label className="workflow-wide">
                  Unpacking evidence / reason
                  <textarea
                    required
                    maxLength={240}
                    disabled={blocked}
                    value={reason}
                    onChange={(e) => setReason(e.target.value)}
                  />
                </label>
              )}
              <button
                className="btn primary"
                disabled={
                  blocked ||
                  ((forwardBlocked || selectedHandoffHeld) &&
                    selected.action === "handoff")
                }
              >
                {selected.action === "handoff"
                  ? "Record shipment handoff"
                  : "Confirm unpacking"}
              </button>
              <button
                type="button"
                className="btn"
                disabled={busy}
                onClick={() => setSelected(null)}
              >
                Close shipment action
              </button>
            </form>
          )}
        </>
      )}
    </section>
  );
}
