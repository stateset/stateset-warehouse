import { useState, type FormEvent } from "react";
import { sameVersions } from "./concurrency";
import type { Command, WarehouseState } from "./types";
import "./Workflows.css";

type Request = (
  path: string,
  body?: unknown,
  method?: "GET" | "POST" | "PUT",
) => Promise<any>;
type Submit = (
  command: Command,
  version: number,
  captured?: Record<string, number>,
) => Promise<boolean>;
type Reviewed = {
  command: Command;
  resource_versions?: Record<string, number> | null;
};

const base = (state: WarehouseState) =>
  `/v1/warehouses/${encodeURIComponent(state.warehouse.id)}`;

/** A planned command is still current when its reviewed resources are unchanged. */
export function freshTask(
  state: WarehouseState,
  planVersion: number,
  task: Reviewed,
): boolean {
  const reviewed = task.resource_versions;
  if (reviewed && state.resource_versions) {
    return sameVersions(
      reviewed,
      Object.fromEntries(
        Object.keys(reviewed).map((k) => [k, state.resource_versions![k] ?? 0]),
      ),
    );
  }
  return planVersion === state.version;
}

function zones(state: WarehouseState, kind?: string) {
  return [
    ...new Set(
      state.locations.filter((l) => !kind || l.kind === kind).map((l) => l.zone),
    ),
  ].sort();
}

function Feedback({ error, notice }: { error: string; notice: string }) {
  return (
    <>
      {error && (
        <p role="alert" className="workflow-error">
          {error}
        </p>
      )}
      {notice && <p role="status">{notice}</p>}
    </>
  );
}

// ---------------------------------------------------------------- putaway

type PutawayTask = Reviewed & {
  reason: string;
  zone: string | null;
  serial_labels_required: boolean;
};
type PutawayPlan = {
  warehouse_id: string;
  version: number;
  tasks: PutawayTask[];
  unplaced: { sku: string; lot: string; from: string; quantity: number }[];
};
const reasonText: Record<string, string> = {
  consolidate_same_lot: "Same SKU and lot already here",
  consolidate_same_sku: "Same SKU already here",
  empty_bin_in_requested_zone: "Empty bin in the chosen zone",
  empty_bin_in_sku_zone: "Empty bin near this SKU",
  empty_bin: "Empty bin",
};

export function PutawayPanel({
  state,
  disabled,
  request,
  onSubmit,
}: {
  state: WarehouseState;
  disabled: boolean;
  request: Request;
  onSubmit: Submit;
}) {
  const [zone, setZone] = useState("");
  const [plan, setPlan] = useState<PutawayPlan | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const load = async (event?: FormEvent) => {
    event?.preventDefault();
    if (disabled || busy) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const result: PutawayPlan = await request(`${base(state)}/putaway/plan`, {
        zone: zone || null,
      });
      if (result.warehouse_id !== state.warehouse.id || !Array.isArray(result.tasks))
        throw new Error("Putaway plan could not be verified.");
      setPlan(result);
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const move = async (task: PutawayTask) => {
    if (!plan || busy || disabled || !freshTask(state, plan.version, task)) return;
    setBusy(true);
    setError("");
    try {
      if (await onSubmit(task.command, plan.version, task.resource_versions ?? undefined)) {
        setPlan(null);
        setNotice("Putaway confirmed. Suggest again for the remaining stock.");
      }
    } finally {
      setBusy(false);
    }
  };
  return (
    <details className="workflow-panel">
      <summary>Directed putaway</summary>
      <p>
        Suggests a storage bin for each SKU and lot waiting in receiving: the
        same lot first, then the same SKU, then an empty bin. There is no bin
        capacity model, so confirm the stock physically fits before saving.
      </p>
      <form onSubmit={load} className="workflow-form">
        <label>
          Preferred zone for empty bins
          <select
            aria-label="Preferred putaway zone"
            value={zone}
            onChange={(e) => {
              setZone(e.target.value);
              setPlan(null);
            }}
          >
            <option value="">Any zone</option>
            {zones(state, "storage").map((z) => (
              <option key={z} value={z}>
                {z}
              </option>
            ))}
          </select>
        </label>
        <button className="btn" type="submit" disabled={disabled || busy}>
          Suggest putaway
        </button>
      </form>
      <Feedback error={error} notice={notice} />
      {plan && (
        <div className="workflow-plan">
          {!plan.tasks.length && !plan.unplaced.length && (
            <p>Nothing is waiting in receiving.</p>
          )}
          {plan.unplaced.map((u) => (
            <p key={`${u.from}:${u.sku}:${u.lot}`}>
              {u.quantity} × {u.sku} · {u.lot} in {u.from}: no eligible storage bin
            </p>
          ))}
          {plan.tasks.map((task, i) => (
            <div className="workflow-task" key={i}>
              <span>
                <strong>
                  {String(task.command.sku)} · {String(task.command.lot)}
                </strong>
                <br />
                {String(task.command.from)} → {String(task.command.to)} ·{" "}
                {String(task.command.quantity)} each ·{" "}
                {reasonText[task.reason] ?? task.reason}
                {task.serial_labels_required && " · scan serial labels"}
              </span>
              <button
                type="button"
                className="btn"
                disabled={disabled || busy || !freshTask(state, plan.version, task)}
                onClick={() => void move(task)}
              >
                {freshTask(state, plan.version, task)
                  ? "Confirm physical putaway"
                  : "Stock changed — suggest again"}
              </button>
            </div>
          ))}
        </div>
      )}
    </details>
  );
}

// ---------------------------------------------------------- batch picking

type Pick = Reviewed & {
  tote: number;
  order_id: string;
  line_id: string;
  quantity: number;
};
type Stop = {
  sequence: number;
  zone: string;
  location: string;
  sku: string;
  lot: string;
  total: number;
  serial_labels_required: boolean;
  picks: Pick[];
};
type BatchPlan = {
  warehouse_id: string;
  version: number;
  totes: { tote: number; order_id: string; reference: string; priority: string }[];
  stops: Stop[];
  pick_count: number;
  held_orders: { order_id: string; reason: string }[];
  unavailable: { order_id: string; sku: string; lot: string; reason: string }[];
};

export function BatchPickPanel({
  state,
  disabled,
  request,
  onSubmit,
}: {
  state: WarehouseState;
  disabled: boolean;
  request: Request;
  onSubmit: Submit;
}) {
  const [maxOrders, setMaxOrders] = useState("10");
  const [zone, setZone] = useState("");
  const [plan, setPlan] = useState<BatchPlan | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const fetchPlan = async (orderIds?: string[]) => {
    const result: BatchPlan = await request(`${base(state)}/picking/batch-plan`, {
      order_ids: orderIds ?? [],
      max_orders: orderIds ? null : Number(maxOrders),
      zone: zone || null,
    });
    if (result.warehouse_id !== state.warehouse.id || !Array.isArray(result.stops))
      throw new Error("Batch pick plan could not be verified.");
    return result;
  };
  const load = async (event: FormEvent) => {
    event.preventDefault();
    if (disabled || busy) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      setPlan(await fetchPlan());
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const pick = async (item: Pick) => {
    if (!plan || busy || disabled) return;
    setBusy(true);
    setError("");
    try {
      // Picks at one stop share a resource version, so each pick is sent
      // against the current warehouse review and the batch is re-planned.
      if (await onSubmit(item.command, state.version)) {
        const orders = plan.totes.map((t) => t.order_id);
        const next = await fetchPlan(orders);
        setPlan(next);
        setNotice(
          next.pick_count
            ? `Picked into tote ${item.tote}. ${next.pick_count} picks left in this batch.`
            : "Batch complete: every pick is confirmed.",
        );
      }
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const toteOf = (orderId: string) =>
    plan?.totes.find((t) => t.order_id === orderId);
  return (
    <details className="workflow-panel">
      <summary>Batch picking</summary>
      <p>
        Pick several orders in one walk. Stops follow the bin order and each
        order gets its own tote. Every pick is still the order line's own
        confirmed pick.
      </p>
      <form onSubmit={load} className="workflow-form">
        <label>
          Orders in this batch
          <input
            type="number"
            min={1}
            max={50}
            step={1}
            required
            value={maxOrders}
            onChange={(e) => setMaxOrders(e.target.value)}
          />
        </label>
        <label>
          Pick zone
          <select
            aria-label="Batch pick zone"
            value={zone}
            onChange={(e) => setZone(e.target.value)}
          >
            <option value="">All zones</option>
            {zones(state).map((z) => (
              <option key={z} value={z}>
                {z}
              </option>
            ))}
          </select>
        </label>
        <button className="btn" type="submit" disabled={disabled || busy}>
          Plan batch
        </button>
      </form>
      <Feedback error={error} notice={notice} />
      {plan && (
        <div className="workflow-plan">
          {!plan.totes.length && <p>No orders are ready to pick.</p>}
          {plan.totes.length > 0 && (
            <p>
              {plan.totes
                .map((t) => `Tote ${t.tote}: ${t.reference} (${t.priority})`)
                .join(" · ")}
            </p>
          )}
          {plan.unavailable.map((u, i) => (
            <p key={i}>
              Held: {u.sku} · {u.lot} for order {u.order_id} ({u.reason})
            </p>
          ))}
          {plan.stops.map((stop) => (
            <div key={stop.sequence}>
              <h4>
                Stop {stop.sequence}: {stop.location} · {stop.sku} · {stop.lot} ·{" "}
                {stop.total} each{stop.serial_labels_required && " · scan serial labels"}
              </h4>
              {stop.picks.map((item) => (
                <div className="workflow-task" key={`${item.order_id}:${item.line_id}`}>
                  <span>
                    <strong>Tote {item.tote}</strong> ·{" "}
                    {toteOf(item.order_id)?.reference ?? item.order_id} ·{" "}
                    {item.quantity} each
                  </span>
                  <button
                    type="button"
                    className="btn"
                    disabled={disabled || busy}
                    onClick={() => void pick(item)}
                  >
                    Confirm pick into tote {item.tote}
                  </button>
                </div>
              ))}
            </div>
          ))}
        </div>
      )}
    </details>
  );
}

// ------------------------------------------------------------ cycle counts

type CountTask = {
  class: string;
  interval_days: number;
  sku: string;
  lot: string;
  location: string;
  last_counted_at: string | null;
  overdue_days: number | null;
  reserved_minimum: number;
  serial_labels_required: boolean;
  command: Command;
};
type CountPlan = {
  warehouse_id: string;
  version: number;
  due: number;
  sku_classes: Record<string, number>;
  tasks: CountTask[];
};

export function CycleCountPanel({
  state,
  disabled,
  request,
  onSubmit,
}: {
  state: WarehouseState;
  disabled: boolean;
  request: Request;
  onSubmit: Submit;
}) {
  const [plan, setPlan] = useState<CountPlan | null>(null);
  const [counts, setCounts] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const key = (t: CountTask) => `${t.location}\u0000${t.sku}\u0000${t.lot}`;
  const load = async (event?: FormEvent) => {
    event?.preventDefault();
    if (disabled || busy) return;
    setBusy(true);
    setError("");
    try {
      const result: CountPlan = await request(`${base(state)}/counts/plan`, {});
      if (result.warehouse_id !== state.warehouse.id || !Array.isArray(result.tasks))
        throw new Error("Cycle count plan could not be verified.");
      setPlan(result);
      setCounts({});
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const save = async (task: CountTask) => {
    const raw = counts[key(task)] ?? "";
    const counted = Number(raw);
    if (!plan || busy || disabled || raw === "" || !Number.isSafeInteger(counted) || counted < 0)
      return;
    setBusy(true);
    setError("");
    try {
      if (await onSubmit({ ...task.command, counted }, state.version)) {
        setNotice(`Count saved for ${task.location} · ${task.sku} · ${task.lot}.`);
        setPlan({ ...plan, tasks: plan.tasks.filter((t) => key(t) !== key(task)) });
      }
    } finally {
      setBusy(false);
    }
  };
  return (
    <details className="workflow-panel">
      <summary>Scheduled cycle counts</summary>
      <p>
        Bins are due by how fast their SKU picks: class A every 7 days, B every
        30, C every 90. Counts are blind: enter what you physically see.
      </p>
      <form onSubmit={load} className="workflow-form">
        <button className="btn" type="submit" disabled={disabled || busy}>
          Show counts due
        </button>
      </form>
      <Feedback error={error} notice={notice} />
      {plan && (
        <div className="workflow-plan">
          <p>
            {plan.due} due · SKU classes:{" "}
            {Object.entries(plan.sku_classes)
              .map(([c, n]) => `${c} ${n}`)
              .join(", ")}
          </p>
          {!plan.tasks.length && <p>Every bin is within its count interval.</p>}
          {plan.tasks.map((task) => (
            <div className="workflow-task" key={key(task)}>
              <span>
                <strong>
                  {task.location} · {task.sku} · {task.lot}
                </strong>
                <br />
                Class {task.class} ·{" "}
                {task.last_counted_at
                  ? `${task.overdue_days} days overdue`
                  : "never counted"}
                {task.reserved_minimum > 0 && ` · at least ${task.reserved_minimum} reserved`}
              </span>
              {task.serial_labels_required ? (
                <span>Count serial units from the scanner with their labels.</span>
              ) : (
                <>
                  <input
                    aria-label={`Counted at ${task.location} ${task.sku} ${task.lot}`}
                    type="number"
                    min={0}
                    max={1_000_000}
                    step={1}
                    value={counts[key(task)] ?? ""}
                    onChange={(e) => setCounts({ ...counts, [key(task)]: e.target.value })}
                  />
                  <button
                    type="button"
                    className="btn"
                    disabled={disabled || busy || (counts[key(task)] ?? "") === ""}
                    onClick={() => void save(task)}
                  >
                    Save count
                  </button>
                </>
              )}
            </div>
          ))}
        </div>
      )}
    </details>
  );
}

// ---------------------------------------------------------------- labels

type LabelKind = "location" | "product" | "zone";

export function LabelPrintPanel({
  state,
  request,
}: {
  state: WarehouseState;
  request: Request;
}) {
  const [kind, setKind] = useState<LabelKind>("location");
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const options =
    kind === "product"
      ? state.products.map((p) => p.sku)
      : kind === "zone"
        ? zones(state)
        : state.locations.map((l) => l.code);
  const print = async (event: FormEvent) => {
    event.preventDefault();
    if (busy || !value) return;
    const labels =
      kind === "zone"
        ? state.locations
            .filter((l) => l.zone === value)
            .map((l) => ({ kind: "location", code: l.code }))
        : kind === "product"
          ? [{ kind: "product", sku: value }]
          : [{ kind: "location", code: value }];
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const result = await request(`${base(state)}/labels`, { labels });
      if (result.format !== "zpl" || typeof result.zpl !== "string")
        throw new Error("Label response could not be verified.");
      const url = URL.createObjectURL(new Blob([result.zpl], { type: "text/plain" }));
      const link = document.createElement("a");
      link.href = url;
      link.download = `${state.warehouse.code}-${kind}-${value}.zpl`.replace(/[^\w.-]+/g, "_");
      link.click();
      URL.revokeObjectURL(url);
      setNotice(`${result.count} label(s) downloaded as ZPL for a 203 dpi printer.`);
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <details className="workflow-panel">
      <summary>Print labels</summary>
      <p>
        Downloads ZPL for Zebra-compatible printers. Barcodes encode the exact
        stored value; codes that Code 128 cannot carry are refused, not altered.
      </p>
      <form onSubmit={print} className="workflow-form">
        <label>
          Label type
          <select
            aria-label="Label type"
            value={kind}
            onChange={(e) => {
              setKind(e.target.value as LabelKind);
              setValue("");
            }}
          >
            <option value="location">One bin</option>
            <option value="zone">Every bin in a zone</option>
            <option value="product">Product</option>
          </select>
        </label>
        <label>
          {kind === "product" ? "SKU" : kind === "zone" ? "Zone" : "Bin"}
          <select
            aria-label="Label value"
            required
            value={value}
            onChange={(e) => setValue(e.target.value)}
          >
            <option value="" disabled>
              Select
            </option>
            {options.map((o) => (
              <option key={o} value={o}>
                {o}
              </option>
            ))}
          </select>
        </label>
        <button className="btn" type="submit" disabled={busy || !value}>
          Download ZPL
        </button>
      </form>
      <Feedback error={error} notice={notice} />
    </details>
  );
}

// ---------------------------------------------------------- carrier labels

type CarrierLabel = {
  id: string;
  carrier: string;
  service: string;
  tracking_number: string;
  rate: { amount: string; currency: string } | null;
  label_format: "zpl" | "url";
  label_url: string | null;
  recorded_at: string;
  voided: { at: string; by: string; reason: string } | null;
};

export function CarrierLabelPanel({
  warehouse,
  packageId,
  open,
  canWork,
  request,
}: {
  warehouse: string;
  packageId: string;
  open: boolean;
  canWork: boolean;
  request: Request;
}) {
  const path = `/v1/warehouses/${encodeURIComponent(warehouse)}/packages/${encodeURIComponent(packageId)}/carrier-labels`;
  const [labels, setLabels] = useState<CarrierLabel[] | null>(null);
  const [carrier, setCarrier] = useState("");
  const [service, setService] = useState("");
  const [tracking, setTracking] = useState("");
  const [amount, setAmount] = useState("");
  const [currency, setCurrency] = useState("USD");
  const [labelUrl, setLabelUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const load = async () => {
    setBusy(true);
    setError("");
    try {
      const result = await request(path);
      if (!Array.isArray(result.labels)) throw new Error("Carrier labels could not be verified.");
      setLabels(result.labels);
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const record = async (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await request(path, {
        carrier,
        service,
        tracking_number: tracking,
        rate: amount ? { amount, currency } : null,
        carton: null,
        label: { format: "url", url: labelUrl },
      });
      setTracking("");
      setLabelUrl("");
      await load();
    } catch (failure) {
      setError((failure as Error).message);
      setBusy(false);
    }
  };
  const [voidReason, setVoidReason] = useState("");
  const voidLabel = async (label: CarrierLabel) => {
    const reason = voidReason.trim();
    if (!reason) return;
    setBusy(true);
    setError("");
    try {
      await request(`${path}/${encodeURIComponent(label.id)}/void`, { reason });
      await load();
    } catch (failure) {
      setError((failure as Error).message);
      setBusy(false);
    }
  };
  const active = labels?.find((l) => !l.voided);
  return (
    <details className="workflow-panel" onToggle={(e) => (e.currentTarget.open && !labels ? void load() : undefined)}>
      <summary>Carrier label</summary>
      {error && (
        <p role="alert" className="workflow-error">
          {error}
        </p>
      )}
      {labels?.map((l) => (
        <div className="workflow-task" key={l.id}>
          <span>
            <strong>
              {l.carrier} · {l.service} · {l.tracking_number}
            </strong>
            <br />
            {l.rate ? `${l.rate.amount} ${l.rate.currency} · ` : ""}
            {l.voided ? `Voided: ${l.voided.reason}` : "Active"}
            {l.label_url && (
              <>
                {" · "}
                <a href={l.label_url} target="_blank" rel="noreferrer noopener">
                  Open label
                </a>
              </>
            )}
          </span>
          {!l.voided && canWork && (
            <>
              <input
                aria-label="Void reason"
                placeholder="Void reason"
                maxLength={240}
                value={voidReason}
                onChange={(e) => setVoidReason(e.target.value)}
              />
              <button
                type="button"
                className="btn"
                disabled={busy || !voidReason.trim()}
                onClick={() => void voidLabel(l).then(() => setVoidReason(""))}
              >
                Void label
              </button>
            </>
          )}
        </div>
      ))}
      {labels && !labels.length && <p>No carrier label recorded yet.</p>}
      {labels && open && canWork && !active && (
        <form onSubmit={record} className="workflow-form">
          <label>
            Carrier
            <input required maxLength={120} value={carrier} onChange={(e) => setCarrier(e.target.value)} />
          </label>
          <label>
            Service
            <input required maxLength={120} value={service} onChange={(e) => setService(e.target.value)} />
          </label>
          <label>
            Tracking number
            <input required maxLength={64} pattern="[!-~]+" value={tracking} onChange={(e) => setTracking(e.target.value)} />
          </label>
          <label>
            Rate (optional)
            <input inputMode="decimal" pattern="(0|[1-9][0-9]{0,11})(\.[0-9]{1,8})?" value={amount} onChange={(e) => setAmount(e.target.value)} />
          </label>
          <label>
            Currency
            <input maxLength={3} pattern="[A-Z]{3}" value={currency} onChange={(e) => setCurrency(e.target.value.toUpperCase())} />
          </label>
          <label className="workflow-wide">
            Label link (HTTPS)
            <input required type="url" pattern="https://.*" value={labelUrl} onChange={(e) => setLabelUrl(e.target.value)} />
          </label>
          <button className="btn" type="submit" disabled={busy}>
            Record carrier label
          </button>
        </form>
      )}
    </details>
  );
}

// ------------------------------------------------ saved replenishment rules

type Rule = {
  sku: string;
  destination: string;
  minimum: number;
  target: number;
  source_zone: string | null;
};

export function SavedReplenishment({
  state,
  role,
  disabled,
  request,
  onSubmit,
  draft,
}: {
  state: WarehouseState;
  role: string;
  disabled: boolean;
  request: Request;
  onSubmit: Submit;
  draft: Rule | null;
}) {
  const [rules, setRules] = useState<Rule[] | null>(null);
  const [due, setDue] = useState<{ version: number; tasks: (Reviewed & { quantity: number })[] } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const path = `${base(state)}/replenishment`;
  const run = async (work: () => Promise<void>) => {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await work();
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const loadRules = () =>
    run(async () => {
      const result = await request(`${path}/rules`);
      setRules(result.rules);
    });
  const loadDue = () =>
    run(async () => {
      const result = await request(`${path}/due`);
      if (result.warehouse_id !== state.warehouse.id) throw new Error("Due plan could not be verified.");
      setDue(result);
    });
  const save = (next: Rule[]) =>
    run(async () => {
      await request(`${path}/rules`, { rules: next }, "PUT");
      setRules(next);
      setNotice(`${next.length} saved rule(s).`);
    });
  const move = (task: Reviewed) =>
    run(async () => {
      if (!due || !freshTask(state, due.version, task)) return;
      if (await onSubmit(task.command, due.version, task.resource_versions ?? undefined)) {
        setDue(null);
        setNotice("Move confirmed. Check due work again before another movement.");
      }
    });
  return (
    <div className="workflow-plan">
      <h4>Saved rules</h4>
      <div className="workflow-form">
        <button type="button" className="btn" disabled={busy} onClick={() => void loadRules()}>
          Show saved rules
        </button>
        <button type="button" className="btn" disabled={busy || disabled} onClick={() => void loadDue()}>
          Show replenishment due now
        </button>
        {role === "admin" && draft && rules && (
          <button
            type="button"
            className="btn"
            disabled={busy || disabled}
            onClick={() =>
              void save([
                ...rules.filter((r) => !(r.sku === draft.sku && r.destination === draft.destination)),
                draft,
              ])
            }
          >
            Save the rule above
          </button>
        )}
      </div>
      <Feedback error={error} notice={notice} />
      {rules?.map((r) => (
        <div className="workflow-task" key={`${r.sku}:${r.destination}`}>
          <span>
            {r.sku} → {r.destination}: keep at least {r.minimum}, refill to {r.target}
            {r.source_zone ? ` from ${r.source_zone}` : ""}
          </span>
          {role === "admin" && (
            <button
              type="button"
              className="btn"
              disabled={busy || disabled}
              onClick={() => void save(rules.filter((x) => x !== r))}
            >
              Remove
            </button>
          )}
        </div>
      ))}
      {rules && !rules.length && <p>No saved rules.</p>}
      {due && !due.tasks.length && <p>No replenishment is due.</p>}
      {due?.tasks.map((task, i) => (
        <div className="workflow-task" key={i}>
          <span>
            {String(task.command.sku)} · {String(task.command.lot)}: {String(task.command.from)} →{" "}
            {String(task.command.to)} · {task.quantity} each
          </span>
          <button
            type="button"
            className="btn"
            disabled={disabled || busy || !freshTask(state, due.version, task)}
            onClick={() => void move(task)}
          >
            {freshTask(state, due.version, task) ? "Confirm physical move" : "Stock changed — check again"}
          </button>
        </div>
      ))}
    </div>
  );
}
