import { useState, type FormEvent } from "react";
import { sameVersions } from "./concurrency";
import type { Command, WarehouseState } from "./types";
import "./Workflows.css";
type Task = {
  command: Command;
  resource_versions: Record<string, number>;
  quantity: number;
};
type Plan = {
  warehouse_id: string;
  version: number;
  tasks: Task[];
  shortages: {
    sku: string;
    destination: string;
    available: number;
    planned: number;
    unfilled: number;
  }[];
};
export default function ReplenishmentPanel({
  state,
  disabled,
  request,
  onSubmit,
}: {
  state: WarehouseState;
  disabled: boolean;
  request: (path: string, body?: unknown) => Promise<any>;
  onSubmit: (
    command: Command,
    version: number,
    captured?: Record<string, number>,
  ) => Promise<boolean>;
}) {
  const [sku, setSku] = useState(state.products[0]?.sku ?? "");
  const [destination, setDestination] = useState(
    state.locations.find((l) => l.kind === "storage")?.code ?? "",
  );
  const [zone, setZone] = useState("");
  const [minimum, setMinimum] = useState("5");
  const [target, setTarget] = useState("20");
  const [plan, setPlan] = useState<Plan | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const reset = () => {
    setPlan(null);
    setNotice("");
  };
  const load = async (event: FormEvent) => {
    event.preventDefault();
    if (disabled || busy) return;
    setBusy(true);
    setError("");
    reset();
    try {
      const result: Plan = await request(
        `/v1/warehouses/${encodeURIComponent(state.warehouse.id)}/replenishment/plan`,
        {
          rules: [
            {
              sku,
              destination,
              minimum: Number(minimum),
              target: Number(target),
              source_zone: zone || null,
            },
          ],
        },
      );
      if (
        result.warehouse_id !== state.warehouse.id ||
        !Number.isSafeInteger(result.version) ||
        !Array.isArray(result.tasks) ||
        !Array.isArray(result.shortages)
      )
        throw new Error("Replenishment plan could not be verified.");
      setPlan(result);
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const fresh = (task: Task) =>
    plan?.warehouse_id === state.warehouse.id &&
    (state.resource_versions
      ? sameVersions(
          task.resource_versions,
          Object.fromEntries(
            Object.keys(task.resource_versions).map((k) => [
              k,
              state.resource_versions![k] ?? 0,
            ]),
          ),
        )
      : plan?.version === state.version);
  const move = async (task: Task) => {
    if (!plan || busy || disabled || !fresh(task)) return;
    setBusy(true);
    setError("");
    try {
      if (await onSubmit(task.command, plan.version, task.resource_versions)) {
        setPlan(null);
        setNotice(
          "Move confirmed. Replan against current balances before another movement.",
        );
      }
    } finally {
      setBusy(false);
    }
  };
  return (
    <details className="workflow-panel">
      <summary>Pick-face replenishment</summary>
      <p>
        Plan from unreserved storage stock in available lots. Receiving and
        staging are excluded. Confirm the physical bin, SKU, lot and quantity
        before saving each movement.
      </p>
      <form onSubmit={load} className="workflow-form">
        <label>
          Replenishment SKU
          <select
            aria-label="Replenishment SKU"
            required
            value={sku}
            onChange={(e) => {
              setSku(e.target.value);
              reset();
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
        <label>
          Pick-face destination
          <select
            aria-label="Pick-face destination"
            required
            value={destination}
            onChange={(e) => {
              setDestination(e.target.value);
              reset();
            }}
          >
            <option value="" disabled>
              Select storage bin
            </option>
            {state.locations
              .filter((l) => l.kind === "storage")
              .map((l) => (
                <option key={l.code} value={l.code}>
                  {l.code}
                </option>
              ))}
          </select>
        </label>
        <label>
          Reserve source zone
          <select
            aria-label="Reserve source zone"
            value={zone}
            onChange={(e) => {
              setZone(e.target.value);
              reset();
            }}
          >
            <option value="">All eligible storage bins</option>
            {[
              ...new Set(
                state.locations
                  .filter((l) => l.kind === "storage")
                  .map((l) => l.zone),
              ),
            ]
              .sort()
              .map((z) => (
                <option key={z} value={z}>
                  {z}
                </option>
              ))}
          </select>
        </label>
        <label>
          Minimum available (each)
          <input
            type="number"
            required
            min={1}
            max={1_000_000}
            step={1}
            value={minimum}
            onChange={(e) => {
              setMinimum(e.target.value);
              reset();
            }}
          />
        </label>
        <label>
          Target available (each)
          <input
            type="number"
            required
            min={Number(minimum) || 1}
            max={1_000_000}
            step={1}
            value={target}
            onChange={(e) => {
              setTarget(e.target.value);
              reset();
            }}
          />
        </label>
        <button className="btn" type="submit" disabled={disabled || busy}>
          Plan replenishment
        </button>
      </form>
      {error && (
        <p role="alert" className="workflow-error">
          {error}
        </p>
      )}
      {notice && <p role="status">{notice}</p>}
      {plan && (
        <div className="workflow-plan">
          {!plan.shortages.length && (
            <p>The pick face meets its minimum; no movement is needed.</p>
          )}
          {plan.shortages.map((s) => (
            <p key={`${s.sku}:${s.destination}`}>
              {s.available} available · {s.planned} planned · {s.unfilled} still
              unavailable
            </p>
          ))}
          {plan.tasks.map((task, index) => (
            <div className="workflow-task" key={index}>
              <span>
                <strong>
                  {String(task.command.sku)} · {String(task.command.lot)}
                </strong>
                <br />
                {String(task.command.from)} → {String(task.command.to)} ·{" "}
                {task.quantity} each
              </span>
              <button
                type="button"
                className="btn"
                disabled={disabled || busy || !fresh(task)}
                onClick={() => void move(task)}
              >
                {fresh(task)
                  ? "Confirm physical move"
                  : "Stock changed — replan"}
              </button>
            </div>
          ))}
        </div>
      )}
    </details>
  );
}
