import { useCallback, useEffect, useRef, useState } from "react";
import { AlertTriangle, CheckCircle2, Download, RefreshCw } from "lucide-react";
import type { Session } from "./types";
import "./OperationsPanel.css";

type OutboxGroup = {
  destination: string;
  status: string;
  count: number;
  due: number;
  active_leases: number;
  expired_leases: number;
  oldest_age_seconds: number;
  oldest_due_age_seconds: number;
};
type NativeGroup = {
  kind: string;
  status: string;
  count: number;
  active_leases: number;
  expired_leases: number;
  oldest_age_seconds: number;
  oldest_updated_age_seconds: number;
};
type Snapshot = {
  schema_version: number;
  snapshot_at: string;
  outbox: {
    total: number;
    pending: number;
    delivering: number;
    delivered: number;
    dead_letter: number;
    due: number;
    active_leases: number;
    expired_leases: number;
    oldest_pending_age_seconds: number;
    groups: OutboxGroup[];
    receivers: { destination: string; configured: boolean }[];
  };
  native: {
    total: number;
    requires_review: number;
    uncertain: number;
    rejected: number;
    local_conflict: number;
    groups: NativeGroup[];
  };
  warehouses: {
    total: number;
    open_exceptions: number;
    items: { warehouse_id: string; version: number; open_exceptions: number }[];
    next_after: string | null;
  };
};
const destinations = ["oms", "tms"];
const statuses = ["pending", "delivering", "delivered", "dead_letter"];
const nativeKinds = ["oms_import", "tms_transport", "oms_fulfillment"];
const nativeStatuses = [
  "frozen",
  "sending",
  "uncertain",
  "completed",
  "local_conflict",
  "rejected",
];
const count = (value: unknown) =>
  Number.isSafeInteger(value) && (value as number) >= 0;
function validate(value: any): Snapshot {
  const groups = value?.outbox?.groups;
  const native = value?.native?.groups;
  const warehouses = value?.warehouses;
  if (
    value?.schema_version !== 1 ||
    typeof value.snapshot_at !== "string" ||
    !Number.isFinite(Date.parse(value.snapshot_at)) ||
    !Array.isArray(groups) ||
    groups.length !== 8 ||
    !Array.isArray(value.outbox.receivers) ||
    value.outbox.receivers.length !== 2 ||
    !value.outbox.receivers.every(
      (receiver: any) =>
        destinations.includes(receiver.destination) &&
        typeof receiver.configured === "boolean",
    ) ||
    new Set(value.outbox.receivers.map((receiver: any) => receiver.destination))
      .size !== 2 ||
    !Array.isArray(native) ||
    native.length !== 18 ||
    !Array.isArray(warehouses?.items) ||
    warehouses.items.length > 100 ||
    !["total", "open_exceptions"].every((key) => count(warehouses[key])) ||
    !(
      warehouses.next_after === null ||
      typeof warehouses.next_after === "string"
    ) ||
    ![
      "total",
      "pending",
      "delivering",
      "delivered",
      "dead_letter",
      "due",
      "active_leases",
      "expired_leases",
      "oldest_pending_age_seconds",
    ].every((key) => count(value.outbox[key])) ||
    ![
      "total",
      "requires_review",
      "uncertain",
      "rejected",
      "local_conflict",
    ].every((key) => count(value.native[key])) ||
    !groups.every(
      (g: any) =>
        destinations.includes(g.destination) &&
        statuses.includes(g.status) &&
        [
          "count",
          "due",
          "active_leases",
          "expired_leases",
          "oldest_age_seconds",
          "oldest_due_age_seconds",
        ].every((key) => count(g[key])),
    ) ||
    new Set(groups.map((g: OutboxGroup) => g.destination + ":" + g.status))
      .size !== 8 ||
    !native.every(
      (g: any) =>
        nativeKinds.includes(g.kind) &&
        nativeStatuses.includes(g.status) &&
        [
          "count",
          "active_leases",
          "expired_leases",
          "oldest_age_seconds",
          "oldest_updated_age_seconds",
        ].every((key) => count(g[key])),
    ) ||
    new Set(native.map((g: NativeGroup) => g.kind + ":" + g.status)).size !==
      18 ||
    !warehouses.items.every(
      (w: any) =>
        typeof w.warehouse_id === "string" &&
        count(w.version) &&
        count(w.open_exceptions),
    )
  ) {
    throw new Error(
      "The operational snapshot is incomplete. Refresh before relying on these counts.",
    );
  }
  const observed = Date.parse(value.snapshot_at);
  if (observed < Date.now() - 60000 || observed > Date.now() + 30000) {
    throw new Error(
      "The snapshot time is outside the current clock window. Check service clocks and refresh before relying on these counts.",
    );
  }
  return value;
}
function age(seconds: number) {
  if (seconds < 60) return seconds + "s";
  if (seconds < 3600) return Math.floor(seconds / 60) + "m";
  if (seconds < 86400) return Math.floor(seconds / 3600) + "h";
  return Math.floor(seconds / 86400) + "d";
}
const title = (value: string) => value.replaceAll("_", " ");
export default function OperationsPanel({
  session,
  request,
  navigate,
}: {
  session: Session;
  request: (path: string) => Promise<any>;
  navigate: (tab: "integrations" | "exceptions" | "activity") => void;
}) {
  const allowed = ["operator", "admin"].includes(session.identity.role);
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [staleReason, setStaleReason] = useState("");
  const lastSuccessfulRead = useRef<number | null>(null);
  const [cursors, setCursors] = useState<(string | null)[]>([null]);
  const cursorsRef = useRef(cursors);
  const sequence = useRef(0);
  const inFlight = useRef(false);
  const load = useCallback(
    async (next: (string | null)[]) => {
      if (inFlight.current || !allowed) return;
      if (!navigator.onLine) {
        setStaleReason(
          "The browser is offline. Reconnect and refresh operational status.",
        );
        return;
      }
      const current = ++sequence.current;
      inFlight.current = true;
      setBusy(true);
      try {
        const after = next[next.length - 1];
        const value = validate(
          await request(
            "/v1/operations/health?limit=25" +
              (after ? "&after=" + encodeURIComponent(after) : ""),
          ),
        );
        if (current !== sequence.current) return;
        setSnapshot(value);
        lastSuccessfulRead.current = performance.now();
        setStaleReason(
          navigator.onLine
            ? ""
            : "The browser is offline. Reconnect and refresh operational status.",
        );
        setCursors(next);
        cursorsRef.current = next;
        setError("");
      } catch (failure) {
        if (current === sequence.current)
          setError(
            (failure as Error).message || "Operational status is unavailable.",
          );
      } finally {
        if (current === sequence.current) {
          inFlight.current = false;
          setBusy(false);
        }
      }
    },
    [allowed, request],
  );
  useEffect(() => {
    void load([null]);
    const refresh = () => {
      if (document.visibilityState === "visible" && navigator.onLine)
        void load(cursorsRef.current);
    };
    const checkFreshness = () => {
      if (!navigator.onLine) {
        setStaleReason(
          "The browser is offline. Reconnect and refresh operational status.",
        );
      } else if (
        lastSuccessfulRead.current !== null &&
        performance.now() - lastSuccessfulRead.current >= 60000
      ) {
        setStaleReason(
          "The last confirmed operational read is over 60 seconds old. Refresh before relying on these counts.",
        );
      }
    };
    const reconnect = () => {
      checkFreshness();
      refresh();
    };
    const timer = window.setInterval(refresh, 30000);
    const ageTimer = window.setInterval(checkFreshness, 1000);
    window.addEventListener("offline", checkFreshness);
    window.addEventListener("online", reconnect);
    document.addEventListener("visibilitychange", reconnect);
    return () => {
      ++sequence.current;
      inFlight.current = false;
      window.clearInterval(timer);
      window.clearInterval(ageTimer);
      window.removeEventListener("offline", checkFreshness);
      window.removeEventListener("online", reconnect);
      document.removeEventListener("visibilitychange", reconnect);
    };
  }, [load]);
  if (!allowed)
    return (
      <p role="alert">
        An operator or administrator is required to view operational status.
      </p>
    );
  const configured = (destination: string) =>
    snapshot?.outbox.receivers.some(
      (receiver) => receiver.destination === destination && receiver.configured,
    ) ?? false;
  const expiredNative =
    snapshot?.native.groups.reduce(
      (sum, group) => sum + group.expired_leases,
      0,
    ) ?? 0;
  const attention =
    snapshot &&
    (snapshot.native.uncertain > 0 ||
      expiredNative > 0 ||
      snapshot.warehouses.open_exceptions > 0 ||
      snapshot.outbox.groups.some(
        (group) =>
          configured(group.destination) &&
          ((group.status === "dead_letter" && group.count > 0) ||
            group.expired_leases > 0),
      ));
  const stale = !!error || !!staleReason;
  const download = () => {
    if (!snapshot || stale || busy) return;
    if (
      !navigator.onLine ||
      lastSuccessfulRead.current === null ||
      performance.now() - lastSuccessfulRead.current >= 60000
    ) {
      setStaleReason(
        "Refresh operational status before downloading a current snapshot.",
      );
      return;
    }
    const report = {
      kind: "warehouse_operational_snapshot",
      tenant: session.identity.tenant,
      warehouse_page_after: cursors[cursors.length - 1],
      snapshot,
    };
    const url = URL.createObjectURL(
      new Blob([JSON.stringify(report, null, 2)], { type: "application/json" }),
    );
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = "warehouse-operations.json";
    anchor.click();
    window.setTimeout(() => URL.revokeObjectURL(url), 0);
  };
  return (
    <section className="operations-panel" aria-labelledby="operations-title">
      <div className="operations-top">
        <div>
          <h2 id="operations-title">Operational status</h2>
          <p>
            Delivery queues, retained integration outcomes, and open warehouse
            exceptions across this workspace.
          </p>
        </div>
        <div className="operations-actions">
          <button
            className="btn"
            disabled={busy}
            onClick={() => void load(cursorsRef.current)}
          >
            <RefreshCw size={16} className={busy ? "spin" : ""} />
            Refresh status
          </button>
          <button
            className="btn"
            disabled={!snapshot || stale || busy}
            onClick={download}
          >
            <Download size={16} />
            Download snapshot
          </button>
        </div>
      </div>
      {stale && (
        <div className="alert error" role="alert">
          {error || staleReason}{" "}
          {snapshot
            ? "Previously observed counts remain below and may be stale."
            : "No operational snapshot has been confirmed."}
        </div>
      )}
      {!snapshot && !stale && <p role="status">Loading operational status…</p>}
      {snapshot && (
        <>
          <div
            className={
              "operations-status " +
              (stale ? "stale" : attention ? "attention" : "clear")
            }
            role="status"
          >
            {stale || attention ? (
              <AlertTriangle size={19} />
            ) : (
              <CheckCircle2 size={19} />
            )}
            <div>
              <strong>
                {stale
                  ? "Status unavailable — retained snapshot"
                  : attention
                    ? "Operational review needed"
                    : "No active review conditions detected"}
              </strong>
              <span>
                Observed {new Date(snapshot.snapshot_at).toLocaleString()}.
                Queue counts do not establish physical inventory agreement or
                downstream completion.
              </span>
            </div>
          </div>
          <div className="operations-cards">
            <article>
              <span>Uncertain native requests</span>
              <strong>{snapshot.native.uncertain}</strong>
              <small>
                Recover the original actor’s request before starting another.
              </small>
            </article>
            <article>
              <span>Expired delivery leases</span>
              <strong>{snapshot.outbox.expired_leases + expiredNative}</strong>
              <small>A sender stopped without a confirmed outcome.</small>
            </article>
            <article>
              <span>Open warehouse exceptions</span>
              <strong>{snapshot.warehouses.open_exceptions}</strong>
              <button
                className="text-btn"
                onClick={() => navigate("exceptions")}
              >
                Review selected warehouse
              </button>
            </article>
            <article>
              <span>Retained review outcomes</span>
              <strong>
                {snapshot.native.rejected + snapshot.native.local_conflict}
              </strong>
              <small>
                Historical rejections and local conflicts remain audit evidence.
              </small>
            </article>
          </div>
          <div className="operations-section-head">
            <h3>Warehouse event delivery</h3>
            <button
              className="text-btn"
              onClick={() => navigate("integrations")}
            >
              Open integration recovery
            </button>
          </div>
          <p className="operations-note">
            Only configured event receivers send these events. Native OMS/TMS
            adapters use separate contracts. Review original evidence before
            requeueing a dead letter.
          </p>
          {destinations.map((destination) => {
            const groups = snapshot.outbox.groups.filter(
              (g) => g.destination === destination,
            );
            const queued = groups.filter((g) =>
              ["pending", "delivering"].includes(g.status),
            );
            return (
              <article className="operations-receiver" key={destination}>
                <div>
                  <strong>
                    {destination === "oms" ? "OMS events" : "TMS events"}
                  </strong>
                  <span
                    className={
                      "operations-chip " +
                      (configured(destination) ? "configured" : "held")
                    }
                  >
                    {configured(destination)
                      ? "Receiver configured"
                      : "Receiver not configured"}
                  </span>
                </div>
                {!configured(destination) && (
                  <p>
                    Events are retained without external delivery. Configure a
                    compatible receiver before expecting this queue to drain.
                  </p>
                )}
                <dl>
                  <div>
                    <dt>Queued</dt>
                    <dd>{queued.reduce((sum, g) => sum + g.count, 0)}</dd>
                  </div>
                  <div>
                    <dt>Due / expired</dt>
                    <dd>{groups.reduce((sum, g) => sum + g.due, 0)}</dd>
                  </div>
                  <div>
                    <dt>Dead letters</dt>
                    <dd>
                      {groups.find((g) => g.status === "dead_letter")?.count ??
                        0}
                    </dd>
                  </div>
                  <div>
                    <dt>Oldest queued</dt>
                    <dd>
                      {age(
                        Math.max(0, ...queued.map((g) => g.oldest_age_seconds)),
                      )}
                    </dd>
                  </div>
                </dl>
              </article>
            );
          })}
          <h3>Native request journal</h3>
          <p className="operations-note">
            Uncertain requests may already have committed remotely. A verified
            rejection permits a separately reviewed new intent. Completed and
            rejected receipts remain immutable.
          </p>
          <div className="operations-table-wrap">
            <table>
              <caption>Native request outcomes</caption>
              <thead>
                <tr>
                  <th>Operation</th>
                  <th>State</th>
                  <th>Requests</th>
                  <th>Expired leases</th>
                  <th>Oldest state age</th>
                </tr>
              </thead>
              <tbody>
                {snapshot.native.groups
                  .filter((group) => group.count > 0)
                  .map((group) => (
                    <tr key={group.kind + group.status}>
                      <td>{title(group.kind)}</td>
                      <td>{title(group.status)}</td>
                      <td>{group.count}</td>
                      <td>{group.expired_leases}</td>
                      <td>{age(group.oldest_updated_age_seconds)}</td>
                    </tr>
                  ))}
                {snapshot.native.total === 0 && (
                  <tr>
                    <td colSpan={5}>No native requests have been retained.</td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
          <div className="operations-section-head">
            <h3>Warehouse coverage</h3>
            <span>
              {snapshot.warehouses.total} warehouse
              {snapshot.warehouses.total === 1 ? "" : "s"} · page{" "}
              {cursors.length}
            </span>
          </div>
          <div className="operations-table-wrap">
            <table>
              <caption>Warehouse versions and open exceptions</caption>
              <thead>
                <tr>
                  <th>Warehouse</th>
                  <th>Version</th>
                  <th>Open exceptions</th>
                </tr>
              </thead>
              <tbody>
                {snapshot.warehouses.items.map((warehouse) => (
                  <tr key={warehouse.warehouse_id}>
                    <td>{warehouse.warehouse_id}</td>
                    <td>{warehouse.version}</td>
                    <td>{warehouse.open_exceptions}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="operations-actions operations-pagination">
            <button
              className="btn"
              disabled={busy || cursors.length === 1}
              onClick={() => void load(cursors.slice(0, -1))}
            >
              Previous warehouses
            </button>
            <button
              className="btn"
              disabled={busy || !snapshot.warehouses.next_after}
              onClick={() =>
                void load([...cursors, snapshot.warehouses.next_after])
              }
            >
              Next warehouses
            </button>
          </div>
          <p className="operations-note">
            Every refresh or page is a new database snapshot. This panel makes
            no stock changes. Full ledger reconciliation is available in the
            Movement ledger.
          </p>
          {session.identity.role === "admin" && (
            <p className="operations-note">
              Monitoring can scrape <code>/v1/operations/metrics</code> with a
              dedicated administrator credential. Keep the credential in your
              monitoring secret store.
            </p>
          )}
        </>
      )}
    </section>
  );
}
