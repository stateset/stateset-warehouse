import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type FormEvent,
} from "react";
import type { Command, LotControl, WarehouseState } from "./types";
import {
  availability,
  availabilityLabel,
  instant,
  localExpiry,
  validControl,
  sameControl,
  type Availability,
} from "./lotAvailability";
import "./Workflows.css";
type Item = {
  cursor: string;
  sku: string;
  lot: string;
  physical: number;
  reserved: number;
  unreserved: number;
  usable: number;
  availability: Availability;
  expiry_required: boolean;
  control: LotControl | null;
};
type View = {
  warehouse_id: string;
  version: number;
  checked_at: string;
  items: Item[];
  next_cursor: string | null;
};
type Review = {
  command: Command;
  version: number;
  warehouse: string;
  summary: string;
  previous: LotControl | undefined;
  required: boolean;
};
function checkedPage(value: View, warehouse: string, sku: string): View {
  const at = instant(value?.checked_at);
  if (
    !value ||
    value.warehouse_id !== warehouse ||
    !Number.isSafeInteger(value.version) ||
    value.version < 0 ||
    at === undefined ||
    !Array.isArray(value.items) ||
    value.items.length > 25 ||
    (value.next_cursor !== null && typeof value.next_cursor !== "string")
  )
    throw new Error(
      "Lot review could not be verified. Refresh before making changes.",
    );
  const identities = new Set<string>();
  for (const item of value.items) {
    const key = JSON.stringify([item.sku, item.lot]);
    if (
      item.sku !== sku ||
      !item.lot ||
      identities.has(key) ||
      item.cursor !== key ||
      typeof item.expiry_required !== "boolean" ||
      ![item.physical, item.reserved, item.unreserved, item.usable].every(
        (v) => Number.isSafeInteger(v) && v >= 0,
      ) ||
      item.reserved > item.physical ||
      item.unreserved !== item.physical - item.reserved ||
      item.usable > item.unreserved ||
      (item.control !== null && !validControl(item.control, item.sku, item.lot))
    )
      throw new Error(
        "Lot identity, custody or control evidence disagrees. Changes are held.",
      );
    const derived = availability(
      {
        products: [
          {
            sku: item.sku,
            name: "",
            category: "",
            reorder_point: 0,
            expiry_required: item.expiry_required,
          },
        ],
        lot_controls: item.control ? [item.control] : [],
      },
      item.sku,
      item.lot,
      at,
    );
    if (
      derived !== item.availability ||
      (derived !== "available" && item.usable !== 0)
    )
      throw new Error(
        "Lot availability disagrees with its original controls. Changes are held.",
      );
    identities.add(key);
  }
  if (
    value.next_cursor !== null &&
    (!value.items.length ||
      value.next_cursor !== value.items[value.items.length - 1].cursor)
  )
    throw new Error("Lot pagination could not be verified.");
  return value;
}
export default function LotControlsPanel({
  state,
  role,
  disabled,
  request,
  onSubmit,
}: {
  state: WarehouseState;
  role: string;
  disabled: boolean;
  request: (path: string) => Promise<any>;
  onSubmit: (command: Command, version: number) => Promise<boolean>;
}) {
  const [open, setOpen] = useState(false),
    [sku, setSku] = useState(state.products[0]?.sku ?? "");
  const [after, setAfter] = useState<string | null>(null),
    [view, setView] = useState<View | null>(null);
  const [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    [notice, setNotice] = useState("");
  const [lot, setLot] = useState(""),
    [mode, setMode] = useState("record_lot_expiry"),
    [expiry, setExpiry] = useState("");
  const [reason, setReason] = useState(""),
    [holdId, setHoldId] = useState(""),
    [clearExpiry, setClearExpiry] = useState(false),
    [required, setRequired] = useState(true);
  const [review, setReview] = useState<Review | null>(null),
    [confirmed, setConfirmed] = useState(false);
  const sequence = useRef(0),
    alive = useRef(true),
    saving = useRef(false);
  const admin = role === "admin",
    canWork = admin || role === "operator";
  const warehouse = state.warehouse.id;
  const load = useCallback(async () => {
    if (!open || !sku) return;
    const generation = ++sequence.current;
    setBusy(true);
    setError("");
    setView(null);
    const params = new URLSearchParams({ sku, limit: "25" });
    if (after) params.set("after", after);
    try {
      const result = checkedPage(
        await request(
          `/v1/warehouses/${encodeURIComponent(warehouse)}/lots?${params}`,
        ),
        warehouse,
        sku,
      );
      if (alive.current && generation === sequence.current) setView(result);
    } catch (e) {
      if (alive.current && generation === sequence.current)
        setError((e as Error).message);
    } finally {
      if (alive.current && generation === sequence.current) setBusy(false);
    }
  }, [open, sku, after, warehouse, request]);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      ++sequence.current;
    };
  }, []);
  useEffect(() => {
    void load();
    return () => {
      ++sequence.current;
    };
  }, [load, state.version]);
  const product = state.products.find((p) => p.sku === sku);
  const control = Array.isArray(state.lot_controls)
    ? state.lot_controls.find((c) => c?.sku === sku && c?.lot === lot)
    : undefined;
  const fresh =
    !!view && view.version === state.version && view.warehouse_id === warehouse;
  const agrees =
    !!product &&
    !!view &&
    view.items.every((item) => {
      const known =
        state.lot_controls?.find(
          (c) => c.sku === item.sku && c.lot === item.lot,
        ) ?? null;
      return (
        !!product.expiry_required === item.expiry_required &&
        sameControl(known, item.control)
      );
    });
  const blocked = disabled || busy || !canWork || !fresh || !agrees;
  const staleReview =
    !!review &&
    (review.warehouse !== warehouse ||
      review.version !== state.version ||
      !fresh);
  const editing = !review && !disabled && !busy;
  const choose = (value: string) => {
    setLot(value);
    setReview(null);
    setConfirmed(false);
    setHoldId("");
    setNotice("");
  };
  const prepare = (event: FormEvent) => {
    event.preventDefault();
    if (blocked || !view || review || !product) return;
    if (
      !reason.trim() ||
      reason.trim().length > 240 ||
      !sku ||
      (mode !== "set_product_expiry_policy" &&
        (!lot.trim() || lot !== lot.trim()))
    ) {
      setError(
        "Enter the exact lot and a reason before reviewing this change.",
      );
      return;
    }
    if (
      !admin &&
      [
        "set_product_expiry_policy",
        "correct_lot_expiry",
        "release_lot_hold",
      ].includes(mode)
    )
      return;
    const command: Command = { type: mode, reason: reason.trim() };
    let summary = "";
    if (mode === "set_product_expiry_policy") {
      command.sku = sku;
      command.required = required;
      if (!!product.expiry_required === required) {
        setError("Choose a different expiry requirement.");
        return;
      }
      summary = `${sku}: ${required ? "require an expiry label on every lot" : "make expiry labels optional"}. Known expiry dates and quarantine holds still apply.`;
    } else if (mode === "release_lot_hold") {
      const hold = control?.holds.find((h) => h.id === holdId);
      if (!hold) {
        setError("Choose the exact active hold to release.");
        return;
      }
      command.hold_id = holdId;
      summary = `${sku} / ${lot}: release only hold ${hold.id} (${hold.reason}). Other holds and expiry still apply.`;
    } else {
      command.sku = sku;
      command.lot = lot;
      if (mode === "quarantine_lot")
        summary = `${sku} / ${lot}: add an independent quarantine hold. Existing reservations and physical units are retained.`;
      else {
        if (
          !(mode === "correct_lot_expiry" && clearExpiry) &&
          instant(expiry) === undefined
        ) {
          setError(
            "Enter an exact RFC3339 expiry with its UTC offset, for example 2027-10-21T17:00:00-07:00. Date-only values are not accepted.",
          );
          return;
        }
        if (mode === "record_lot_expiry" && control?.expiry_recorded) {
          setError(
            "The original expiry is already recorded. A supervisor must correct it explicitly.",
          );
          return;
        }
        if (mode === "correct_lot_expiry" && !control?.expiry_recorded) {
          setError("Record the original expiry before correcting it.");
          return;
        }
        command.expires_at =
          mode === "correct_lot_expiry" && clearExpiry ? null : expiry;
        summary = `${sku} / ${lot}: ${mode === "correct_lot_expiry" ? "correct expiry" : "record original expiry"} to ${command.expires_at === null ? "unknown (missing label rules will apply)" : expiry}. Quarantine holds are retained.`;
      }
    }
    setError("");
    setConfirmed(false);
    setReview({
      command,
      version: view.version,
      warehouse,
      summary,
      previous: control,
      required: !!product.expiry_required,
    });
  };
  const save = async () => {
    if (!review || blocked || staleReview || !confirmed || saving.current)
      return;
    saving.current = true;
    setBusy(true);
    setError("");
    try {
      if (await onSubmit(review.command, review.version)) {
        if (alive.current) {
          setReview(null);
          setConfirmed(false);
          setReason("");
          setNotice("Lot control saved. Physical quantities were not changed.");
        }
      }
    } catch (e) {
      if (alive.current) setError((e as Error).message);
    } finally {
      saving.current = false;
      if (alive.current) setBusy(false);
    }
  };
  return (
    <details
      className="workflow-panel lot-controls-panel"
      onToggle={(e) => setOpen(e.currentTarget.open)}
    >
      <summary>Lot expiry and quarantine</summary>
      <p>
        Physical custody and reservations stay recorded while a lot is
        unavailable. Supervisors correct expiry and release individual holds.
        These controls apply to this facility.
      </p>
      <label className="lot-selector">
        Lot-control SKU
        <select
          aria-label="Lot-control SKU"
          value={sku}
          disabled={disabled || busy || !!review}
          onChange={(e) => {
            setSku(e.target.value);
            setAfter(null);
            choose("");
          }}
        >
          <option value="" disabled>
            Select SKU
          </option>
          {state.products.map((p) => (
            <option key={p.sku} value={p.sku}>
              {p.sku} · {p.name}
            </option>
          ))}
        </select>
      </label>
      <button
        type="button"
        className="btn"
        disabled={disabled || busy || !sku}
        onClick={() => void load()}
      >
        Refresh lot review
      </button>
      {error && (
        <p role="alert" className="workflow-error">
          {error}
        </p>
      )}
      {notice && <p role="status">{notice}</p>}
      {view && (
        <>
          <p>
            Availability checked at {view.checked_at}. Expiry is checked again
            when fulfillment is saved.
          </p>
          {fresh && !agrees && (
            <p role="alert">
              Lot review and warehouse evidence disagree. Refresh both before
              making changes.
            </p>
          )}
          {!fresh && (
            <p role="status">
              Warehouse changed. Refresh the workspace and lot review before
              making a new change.
            </p>
          )}
          <div className="table-scroll">
            <table aria-label="Lot availability">
              <thead>
                <tr>
                  <th>Lot / expiry</th>
                  <th>Availability</th>
                  <th>Physical</th>
                  <th>Reserved</th>
                  <th>Unreserved</th>
                  <th>Usable storage</th>
                  <th>Active holds</th>
                  <th>Review</th>
                </tr>
              </thead>
              <tbody>
                {view.items.map((item) => (
                  <tr key={item.cursor}>
                    <td>
                      <strong>{item.lot}</strong>
                      <span className="subline">
                        {item.control?.expires_at
                          ? localExpiry(
                              item.control.expires_at,
                              state.warehouse.timezone,
                            )
                          : "No expiry recorded"}
                      </span>
                      {item.control?.expires_at && (
                        <span className="subline mono">
                          {item.control.expires_at}
                        </span>
                      )}
                    </td>
                    <td>{availabilityLabel[item.availability]}</td>
                    <td>{item.physical}</td>
                    <td>{item.reserved}</td>
                    <td>{item.unreserved}</td>
                    <td>{item.usable}</td>
                    <td>
                      {item.control?.holds.length || 0}
                      {item.control?.holds.map((h) => (
                        <span key={h.id} className="subline">
                          {h.reason} · {h.actor} · {h.quarantined_at}
                          <br />
                          <code>{h.id}</code>
                        </span>
                      ))}
                    </td>
                    <td>
                      <button
                        type="button"
                        className="mini-btn"
                        disabled={!editing}
                        onClick={() => choose(item.lot)}
                      >
                        Review {item.lot}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {!view.items.length && (
            <p>
              No retained lots for this SKU. You can record a label or
              quarantine before receiving goods.
            </p>
          )}
          <div className="row-actions">
            <button
              type="button"
              className="btn"
              disabled={busy || !after || !!review}
              onClick={() => setAfter(null)}
            >
              First lots
            </button>
            <button
              type="button"
              className="btn"
              disabled={busy || !view.next_cursor || !!review || !fresh}
              onClick={() => setAfter(view.next_cursor)}
            >
              Next lots
            </button>
          </div>
        </>
      )}
      {canWork && (
        <form className="workflow-form" onSubmit={prepare}>
          <label>
            Lot-control action
            <select
              value={mode}
              disabled={!editing}
              onChange={(e) => {
                setMode(e.target.value);
                setClearExpiry(false);
                setHoldId("");
                setNotice("");
              }}
            >
              <option value="record_lot_expiry">Record original expiry</option>
              <option value="quarantine_lot">Quarantine lot</option>
              {admin && (
                <>
                  <option value="correct_lot_expiry">Correct expiry</option>
                  <option value="release_lot_hold">Release exact hold</option>
                  <option value="set_product_expiry_policy">
                    Change required-label policy
                  </option>
                </>
              )}
            </select>
          </label>
          {mode !== "set_product_expiry_policy" && (
            <label>
              Exact lot
              <input
                required
                maxLength={240}
                value={lot}
                disabled={!editing}
                onChange={(e) => {
                  setLot(e.target.value);
                  setHoldId("");
                }}
              />
            </label>
          )}
          {["record_lot_expiry", "correct_lot_expiry"].includes(mode) && (
            <label className="workflow-wide">
              Expiry instant including UTC offset
              <input
                required={!clearExpiry}
                disabled={!editing || clearExpiry}
                value={expiry}
                maxLength={80}
                placeholder="2027-10-21T17:00:00-07:00"
                onChange={(e) => setExpiry(e.target.value)}
              />
              <small>
                Use the exact label evidence and a verified offset. Facility
                timezone: {state.warehouse.timezone}. No date-only conversion is
                inferred.
              </small>
            </label>
          )}
          {mode === "correct_lot_expiry" && (
            <label className="workflow-checkbox workflow-wide">
              <input
                type="checkbox"
                disabled={!editing}
                checked={clearExpiry}
                onChange={(e) => setClearExpiry(e.target.checked)}
              />
              Correct the recorded expiry to unknown
            </label>
          )}
          {mode === "set_product_expiry_policy" && (
            <label className="workflow-checkbox workflow-wide">
              <input
                type="checkbox"
                disabled={!editing}
                checked={required}
                onChange={(e) => setRequired(e.target.checked)}
              />
              Require an expiry label for every lot of this SKU
            </label>
          )}
          {mode === "release_lot_hold" && (
            <label className="workflow-wide">
              Exact active hold
              <select
                aria-label="Exact active hold"
                required
                value={holdId}
                disabled={!editing}
                onChange={(e) => setHoldId(e.target.value)}
              >
                <option value="">Choose original hold</option>
                {control?.holds.map((h) => (
                  <option key={h.id} value={h.id}>
                    {h.id} · {h.reason}
                  </option>
                ))}
              </select>
            </label>
          )}
          <label className="workflow-wide">
            Lot-control reason
            <textarea
              required
              maxLength={240}
              disabled={!editing}
              value={reason}
              onChange={(e) => setReason(e.target.value)}
            />
          </label>
          {!review && (
            <button className="btn" disabled={blocked}>
              Review lot change
            </button>
          )}
        </form>
      )}
      {review && (
        <div
          className="lot-review"
          role="region"
          aria-label="Reviewed lot change"
        >
          <h4>Review lot change</h4>
          <p>{review.summary}</p>
          <p>
            Current expiry: {review.previous?.expires_at ?? "Unknown"} ·{" "}
            {review.previous?.holds.length ?? 0} active{" "}
            {review.previous?.holds.length === 1 ? "hold" : "holds"} · expiry
            label {review.required ? "required" : "optional"}.
          </p>
          <p>Reason: {String(review.command.reason)}</p>
          {staleReview && (
            <p role="alert">
              This reviewed warehouse version changed. Edit and review current
              evidence before saving.
            </p>
          )}
          <label className="workflow-checkbox">
            <input
              type="checkbox"
              checked={confirmed}
              disabled={blocked || staleReview}
              onChange={(e) => setConfirmed(e.target.checked)}
            />
            I checked the original label or investigation evidence for this
            change.
          </label>
          <div className="row-actions">
            <button
              type="button"
              className="btn"
              disabled={disabled || busy}
              onClick={() => {
                setReview(null);
                setConfirmed(false);
              }}
            >
              Edit lot change
            </button>
            <button
              type="button"
              className="btn primary"
              disabled={blocked || staleReview || !confirmed}
              onClick={() => void save()}
            >
              Confirm lot change
            </button>
          </div>
        </div>
      )}
    </details>
  );
}
