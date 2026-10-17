import { useEffect, useRef, useState, type FormEvent } from "react";
import type { Session, WarehouseState } from "./types";

type SourceOrder = {
  id: string;
  orderNumber: string;
  updatedAt: string;
  status: string;
  lineItems: { title: string; sku: string | null; quantity: number }[];
};
type NativeReceipt = {
  found: boolean;
  status?: string;
  request_id?: string;
  last_error?: string;
  attempts?: number;
  in_progress?: boolean;
  response?: unknown;
};
type Pending = {
  warehouse: string;
  path: string;
  body: { request_id: string; [key: string]: unknown };
};
type Props = {
  session: Session;
  state: WarehouseState;
  active: boolean;
  blocked: boolean;
  request: (path: string, body?: unknown) => Promise<any>;
  refresh: (warehouse: string) => Promise<void>;
  onPending: (value: boolean) => void;
};
function restore(key: string): { pending: Pending | null; broken: boolean } {
  try {
    const saved = sessionStorage.getItem(key);
    if (!saved) return { pending: null, broken: false };
    const value = JSON.parse(saved) as Pending;
    if (
      !value.path?.startsWith(`/v1/warehouses/${value.warehouse}/`) ||
      typeof value.body?.request_id !== "string"
    )
      throw new Error();
    return { pending: value, broken: false };
  } catch {
    return { pending: null, broken: true };
  }
}
function download(value: unknown, name: string) {
  const url = URL.createObjectURL(
    new Blob([JSON.stringify(value, null, 2)], { type: "application/json" }),
  );
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  link.click();
  URL.revokeObjectURL(url);
}
export default function NativePanel({
  session,
  state,
  active,
  blocked,
  request,
  refresh,
  onPending,
}: Props) {
  const key = `wms-native:${session.identity.tenant}:${session.identity.actor}`;
  const [restored] = useState(() => restore(key));
  const [pending, setPending] = useState<Pending | null>(restored.pending);
  const broken = restored.broken;
  const mounted = useRef(true);
  const operationInFlight = useRef(false);
  const [receipt, setReceipt] = useState<NativeReceipt | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(
    broken
      ? "Saved integration request is unreadable. Review retained server evidence before repeating any operation."
      : "",
  );
  const [notice, setNotice] = useState("");
  const [orders, setOrders] = useState<SourceOrder[]>([]);
  const [page, setPage] = useState(1);
  const [next, setNext] = useState(false);
  const [selected, setSelected] = useState<SourceOrder | null>(null);
  const [orderId, setOrderId] = useState("");
  const [fulfillmentOrderId, setFulfillmentOrderId] = useState("");
  const [fulfillmentVersion, setFulfillmentVersion] = useState(state.version);
  const [importVersion, setImportVersion] = useState(state.version);
  const [transportVersion, setTransportVersion] = useState(state.version);
  const [preview, setPreview] = useState<unknown>(null);
  const [reviewedPlan, setReviewedPlan] = useState<Record<
    string,
    unknown
  > | null>(null);
  const canWork = ["admin", "operator"].includes(session.identity.role);
  const config = session.native_integrations;
  const conditionalOms = config?.oms.shipment_writeback === "conditional_v1";
  const fulfillmentOrder = state.orders.find(
    (order) => order.id === fulfillmentOrderId,
  );
  const disabled =
    !canWork || session.demo || blocked || busy || !!pending || broken;
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  useEffect(() => {
    onPending(!!pending || broken || busy);
  }, [pending, broken, busy, onPending]);
  function clear() {
    sessionStorage.removeItem(key);
    setPending(null);
  }
  async function send(value: Pending, recovering = false) {
    if (operationInFlight.current) return;
    operationInFlight.current = true;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const result = (await request(value.path, value.body)) as NativeReceipt;
      if (!mounted.current) return;
      setReceipt(result);
      if (result.status === "completed") {
        clear();
        setNotice(
          "Integration saved. Original request and source evidence are retained.",
        );
        setSelected(null);
        setFulfillmentOrderId("");
        setReviewedPlan(null);
        await refresh(value.warehouse);
      } else if (result.status === "rejected") {
        clear();
        setFulfillmentOrderId("");
        setNotice(
          "OMS durably rejected this request without applying fulfillment. Review the current source and shipped order before submitting a new request.",
        );
      } else
        setNotice(
          "Remote acceptance is still uncertain or in progress. Check or retry this original request.",
        );
    } catch (e) {
      if (!mounted.current) return;
      const failure = e as Error & { status?: number };
      // A rejected first attempt is definitive. A rejected recovery attempt
      // says nothing about an earlier uncertain acceptance; keep its journal.
      if (
        !recovering &&
        failure.status &&
        failure.status >= 400 &&
        failure.status < 500 &&
        ![408, 429].includes(failure.status)
      )
        clear();
      setError(
        failure.message +
          (recovering
            ? " Original request retained. Check its recorded outcome before starting another operation."
            : ""),
      );
    } finally {
      operationInFlight.current = false;
      if (mounted.current) setBusy(false);
    }
  }
  async function start(path: string, body: Record<string, unknown>) {
    if (disabled || operationInFlight.current) return;
    const value: Pending = {
      warehouse: state.warehouse.id,
      path,
      body: { ...body, request_id: crypto.randomUUID() },
    };
    try {
      sessionStorage.setItem(key, JSON.stringify(value));
    } catch {
      setError(
        "Session storage is required to recover integration requests safely.",
      );
      return;
    }
    setPending(value);
    await send(value);
  }
  async function check() {
    if (!pending || operationInFlight.current) return;
    operationInFlight.current = true;
    setBusy(true);
    setError("");
    try {
      const value = (await request(
        `/v1/warehouses/${pending.warehouse}/native-requests/${pending.body.request_id}`,
      )) as NativeReceipt;
      if (!mounted.current) return;
      setReceipt(value);
      if (value.status === "completed") {
        clear();
        setNotice("Original integration request confirmed.");
        await refresh(pending.warehouse);
      } else if (value.status === "rejected") {
        clear();
        setFulfillmentOrderId("");
        setNotice(
          "OMS durably rejected the original request without applying fulfillment. Review the current source before starting a new request.",
        );
      } else if (value.status === "local_conflict") {
        clear();
        setError(
          value.last_error ||
            "Import was rejected by current warehouse state. Refresh and review before a new request.",
        );
      } else
        setNotice(
          value.found
            ? "Original intent retained. Retry uses its exact frozen payload."
            : "No committed receipt yet. Retry the original request to establish its outcome.",
        );
    } catch (e) {
      if (mounted.current) setError((e as Error).message);
    } finally {
      operationInFlight.current = false;
      if (mounted.current) setBusy(false);
    }
  }
  async function load(pageNumber: number) {
    setBusy(true);
    setError("");
    try {
      const value = await request(
        `/v1/integrations/oms/orders?page=${pageNumber}`,
      );
      if (!mounted.current) return;
      setOrders(value.items);
      setPage(pageNumber);
      setNext(value.pagination.hasNext);
    } catch (e) {
      if (mounted.current) setError((e as Error).message);
    } finally {
      if (mounted.current) setBusy(false);
    }
  }
  function importOrder(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!selected) return;
    const fields = new FormData(event.currentTarget);
    void start(`/v1/warehouses/${state.warehouse.id}/imports/oms`, {
      expected_version: importVersion,
      oms_order_id: selected.id,
      source_updated_at: selected.updatedAt,
      details: {
        customer: fields.get("customer"),
        destination: fields.get("destination"),
        priority: fields.get("priority"),
      },
    });
  }
  function preparePlan(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const fields = new FormData(event.currentTarget);
    const stop = (prefix: string): Record<string, unknown> => ({
      ...Object.fromEntries(
        [
          "facility",
          "city",
          "state",
          "date",
          "postal_code",
          "country",
          "contact",
          "phone",
          "instructions",
        ].map((field) => [
          field,
          String(fields.get(`${prefix}_${field}`) ?? ""),
        ]),
      ),
      address_lines: String(fields.get(`${prefix}_address`) ?? "")
        .split("\n")
        .filter(Boolean),
    });
    const plan: Record<string, unknown> = Object.fromEntries(
      ["reference", "customer_id", "mode", "equipment", "commodity"].map(
        (field) => [field, fields.get(field)],
      ),
    );
    for (const field of ["weight_lbs", "miles", "buy_cents", "sell_cents"])
      plan[field] = Number(fields.get(field));
    plan.origin = stop("origin");
    plan.destination = stop("destination");
    setReviewedPlan(plan);
  }
  return (
    <section hidden={!active} className="native-panel">
      <div className="section-head">
        <div>
          <h2>Native OMS and TMS workflows</h2>
          <p>
            Import canonical orders, retain source revisions, and create planned
            transportation from packed orders.
          </p>
        </div>
      </div>
      {session.demo && (
        <p className="native-note">
          Native services are disabled in this synthetic workspace. Configure
          tenant connections in a real workspace to use these workflows.
        </p>
      )}
      {!canWork && (
        <p className="native-note">
          A warehouse operator or administrator is required for native
          workflows.
        </p>
      )}
      {error && (
        <div className="alert error" role="alert">
          {error}
        </div>
      )}
      {notice && (
        <div className="alert info" role="status">
          {notice}
        </div>
      )}
      {pending && (
        <div className="native-recovery">
          <strong>Original integration request awaiting confirmation</strong>
          <p className="mono">{pending.body.request_id}</p>
          <p>
            Other warehouse writes are paused until this request has a known
            outcome.
          </p>
          <button className="btn" disabled={busy} onClick={() => void check()}>
            Check original integration request
          </button>
          <button
            className="btn"
            disabled={busy || blocked}
            onClick={() => void send(pending, true)}
          >
            Retry original integration request
          </button>
        </div>
      )}
      {receipt && (
        <p className="native-note">
          Last request: {receipt.status ?? "no committed receipt"} ·{" "}
          {receipt.attempts ?? 0} transport attempts{" "}
          <button
            className="mini-btn"
            onClick={() => download(receipt, "native-request.json")}
          >
            Download evidence
          </button>
        </p>
      )}
      <div className="native-grid">
        <section className="integration-card">
          <h3>Import from OMS</h3>
          <p>
            Source: stateset-sync-server. Customer and destination require
            operator review.
          </p>
          <button
            className="btn"
            disabled={disabled || !config?.oms.configured}
            onClick={() => void load(1)}
          >
            Load OMS orders
          </button>
          <div className="native-orders">
            {orders.map((order) => (
              <button
                key={order.id}
                className="native-source"
                disabled={disabled}
                onClick={() => {
                  setSelected(order);
                  setImportVersion(state.version);
                }}
              >
                <strong>{order.orderNumber}</strong>
                <span>
                  {order.status} · {order.lineItems.length} lines
                </span>
              </button>
            ))}
          </div>
          {!!orders.length && (
            <div className="row-actions">
              <button
                className="mini-btn"
                disabled={disabled || page === 1}
                onClick={() => void load(page - 1)}
              >
                Previous OMS page
              </button>
              <span>Page {page}</span>
              <button
                className="mini-btn"
                disabled={disabled || !next}
                onClick={() => void load(page + 1)}
              >
                Next OMS page
              </button>
            </div>
          )}
          {selected && (
            <form key={selected.id} onSubmit={importOrder}>
              <h4>Review {selected.orderNumber}</h4>
              <p className="subline">Source revision: {selected.updatedAt}</p>
              <ul>
                {selected.lineItems.map((line, index) => (
                  <li key={index}>
                    {line.sku || "Missing SKU"} · {line.quantity} each ·{" "}
                    {line.title}
                  </li>
                ))}
              </ul>
              <label>
                Customer
                <input name="customer" required maxLength={240} />
              </label>
              <label>
                Destination
                <input name="destination" required maxLength={500} />
              </label>
              <label>
                Priority
                <select name="priority">
                  <option value="normal">Normal</option>
                  <option value="high">High</option>
                </select>
              </label>
              <button className="btn primary" disabled={disabled}>
                Import reviewed OMS order
              </button>
            </form>
          )}
        </section>
        <section className="integration-card">
          <h3>Plan transportation</h3>
          <p>
            Source: stateset-freight. The result is a planned load. Actual
            carrier handoff is recorded in fulfillment.
          </p>
          <label>
            Packed order
            <select
              value={orderId}
              disabled={disabled}
              onChange={(e) => {
                setOrderId(e.target.value);
                setTransportVersion(state.version);
                setReviewedPlan(null);
                setPreview(null);
              }}
            >
              <option value="">Choose a packed OMS order</option>
              {state.orders
                .filter(
                  (order) =>
                    order.status === "packed" &&
                    order.source_order_id?.startsWith("oms:"),
                )
                .map((order) => (
                  <option key={order.id} value={order.id}>
                    {order.reference}
                  </option>
                ))}
            </select>
          </label>
          {orderId && (
            <>
              <button
                className="mini-btn"
                disabled={busy}
                onClick={() => {
                  setBusy(true);
                  void request(
                    `/v1/warehouses/${state.warehouse.id}/orders/${orderId}/readiness`,
                  )
                    .then(setPreview)
                    .catch((e) => setError(e.message))
                    .finally(() => setBusy(false));
                }}
              >
                Review warehouse readiness
              </button>
              {preview != null && (
                <details open>
                  <summary>Warehouse readiness</summary>
                  <pre>{JSON.stringify(preview, null, 2)}</pre>
                </details>
              )}
              <form
                key={orderId}
                onSubmit={preparePlan}
                onChange={() => setReviewedPlan(null)}
              >
                <div className="native-fields">
                  {[
                    ["reference", "Transport reference"],
                    ["customer_id", "Freight customer ID"],
                    ["commodity", "Commodity"],
                  ].map(([name, label]) => (
                    <label key={name}>
                      {label}
                      <input name={name} required maxLength={200} />
                    </label>
                  ))}
                  <label>
                    Mode
                    <select name="mode">
                      <option>FTL</option>
                      <option>LTL</option>
                    </select>
                  </label>
                  <label>
                    Equipment
                    <select name="equipment">
                      <option>Dry van</option>
                      <option>Reefer</option>
                      <option>Flatbed</option>
                    </select>
                  </label>
                  {[
                    ["weight_lbs", "Weight (lb)", 1, 80000],
                    ["miles", "Miles", 1, 10000],
                    ["buy_cents", "Buy (USD cents)", 0, 100000000],
                    ["sell_cents", "Sell (USD cents)", 0, 100000000],
                  ].map(([name, label, min, max]) => (
                    <label key={name}>
                      {label}
                      <input
                        name={String(name)}
                        type="number"
                        min={Number(min)}
                        max={Number(max)}
                        step="1"
                        required
                      />
                    </label>
                  ))}
                </div>
                {[
                  ["origin", "Pickup"],
                  ["destination", "Delivery"],
                ].map(([prefix, title]) => (
                  <fieldset key={prefix}>
                    <legend>{title}</legend>
                    <div className="native-fields">
                      {[
                        ["facility", "Facility", 60],
                        ["city", "City", 30],
                        ["state", "State / province code", 2],
                        ["postal_code", "Postal code", 15],
                        ["country", "Country code", 2],
                      ].map(([field, label, max]) => (
                        <label key={field}>
                          {label}
                          <input
                            name={`${prefix}_${field}`}
                            required
                            maxLength={Number(max)}
                          />
                        </label>
                      ))}
                      <label>
                        Date
                        <input name={`${prefix}_date`} type="date" required />
                      </label>
                      <label>
                        Street address (one or two lines)
                        <textarea
                          name={`${prefix}_address`}
                          required
                          rows={2}
                        />
                      </label>
                      {[
                        ["contact", "Contact"],
                        ["phone", "Phone"],
                        ["instructions", "Instructions"],
                      ].map(([field, label]) => (
                        <label key={field}>
                          {label}
                          <input
                            name={`${prefix}_${field}`}
                            maxLength={field === "instructions" ? 1000 : 100}
                          />
                        </label>
                      ))}
                    </div>
                  </fieldset>
                ))}
                <button
                  className="btn"
                  disabled={disabled || !config?.tms.configured}
                >
                  Review transport plan
                </button>
              </form>
              {reviewedPlan && (
                <div className="native-review">
                  <h4>Confirm planned load</h4>
                  <pre>{JSON.stringify(reviewedPlan, null, 2)}</pre>
                  <p>
                    Review facilities, dates, weight, mileage, and prices before
                    creating the load.
                  </p>
                  <button
                    className="btn primary"
                    disabled={disabled}
                    onClick={() =>
                      void start(
                        `/v1/warehouses/${state.warehouse.id}/orders/${orderId}/transport`,
                        {
                          expected_version: transportVersion,
                          plan: reviewedPlan,
                        },
                      )
                    }
                  >
                    Create reviewed planned load
                  </button>
                </div>
              )}
            </>
          )}
        </section>
      </div>
      {conditionalOms && (
        <section className="integration-card">
          <h3>Record the physical handoff in OMS</h3>
          <p>
            The OMS receiver must support conditional fulfillment. The original
            source revision, shipment event, and delivery receipt are retained.
          </p>
          <label>
            Shipped OMS order
            <select
              value={fulfillmentOrderId}
              disabled={disabled}
              onChange={(event) => {
                setFulfillmentOrderId(event.target.value);
                setFulfillmentVersion(state.version);
              }}
            >
              <option value="">Choose a shipped OMS order</option>
              {state.orders
                .filter(
                  (order) =>
                    order.status === "shipped" &&
                    order.source_order_id?.startsWith("oms:"),
                )
                .map((order) => (
                  <option key={order.id} value={order.id}>
                    {order.reference}
                  </option>
                ))}
            </select>
          </label>
          {fulfillmentOrder && (
            <>
              <p>
                <strong>{fulfillmentOrder.carrier}</strong> ·{" "}
                {fulfillmentOrder.handoff_reference}
                <br />
                {fulfillmentOrder.shipped_at}
              </p>
              <p>
                {fulfillmentOrder.lines.length} complete order lines. Confirm
                that these units physically left the warehouse.
              </p>
              <button
                className="btn primary"
                disabled={disabled || fulfillmentVersion !== state.version}
                onClick={() =>
                  void start(
                    `/v1/warehouses/${state.warehouse.id}/orders/${fulfillmentOrder.id}/oms-fulfillment`,
                    { expected_version: fulfillmentVersion },
                  )
                }
              >
                Record verified handoff in OMS
              </button>
              {fulfillmentVersion !== state.version && (
                <p className="native-note">
                  Warehouse changed. Select the shipped order again to review
                  the current record.
                </p>
              )}
            </>
          )}
        </section>
      )}
      <p className="native-note">
        {conditionalOms
          ? "Conditional OMS fulfillment is enabled. Every delivery verifies the receiver contract and uses its original retained request; unsupported receivers remain blocked."
          : "OMS shipment writeback is held because the legacy OMS endpoint cannot protect a concurrent order revision. Shipped order evidence remains available for reconciliation."}
      </p>
    </section>
  );
}
