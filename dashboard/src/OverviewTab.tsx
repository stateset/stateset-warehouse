import { Badge, Metric, Section, label } from "./WarehouseUi";
import { fmt } from "./format";
import type { ReconciliationProof } from "./proofView";
import type { Exception, Order, Product, Stock, WarehouseState } from "./types";
import type { Tab, Totals } from "./uiTypes";
import { activeOrder, prioritizeOrders } from "./orderView";
import {
  ArrowDownLeft,
  ArrowDownToLine,
  ArrowRight,
  ArrowUpRight,
  Box,
  Boxes,
  CheckCheck,
  ChevronRight,
  ClipboardCheck,
  MapPin,
  Package,
  PackageCheck,
  Truck,
  AlertTriangle,
} from "lucide-react";

/** Overview tab: today's work, stock health and reconciliation at a glance. */
export default function OverviewTab({
  availableStorage,
  lowStock,
  navigate,
  openExceptions,
  orderTable,
  reconciliation,
  s,
  setFilter,
  setSearch,
  totals,
}: {
  availableStorage: Stock[];
  lowStock: Product[];
  navigate: (next: Tab) => void;
  openExceptions: Exception[];
  orderTable: (orders: Order[]) => JSX.Element;
  reconciliation: ReconciliationProof | null;
  s: WarehouseState;
  setFilter: (value: string) => void;
  setSearch: (value: string) => void;
  totals: Totals;
}) {
  const floor = [
    { kind: "receiving", title: "Receiving", icon: ArrowDownToLine },
    { kind: "storage", title: "Storage", icon: Boxes },
    { kind: "staging", title: "Staging", icon: Truck },
  ].map((area) => ({
    ...area,
    units: s.stocks
      .filter((stock) =>
        s.locations.some(
          (location) =>
            location.code === stock.location && location.kind === area.kind,
        ),
      )
      .reduce((sum, stock) => sum + stock.on_hand, 0),
  }));
  const packed = s.orders.filter((order) => order.status === "packed").length;
  const pending = s.orders.filter((order) => order.status === "pending").length;
  const receivingUnits = floor.find((area) => area.kind === "receiving")!.units;
  const openView = (next: Tab, status = "all") => {
    navigate(next);
    setFilter(status);
  };
  const nextActions = [
    ...(!reconciliation?.ok
      ? [
          {
            title: "Review inventory balances",
            detail: "Check the ledger before continuing work.",
            icon: AlertTriangle,
            tone: "danger",
            action: () => openView("activity"),
          },
        ]
      : []),
    ...(openExceptions.length
      ? [
          {
            title: `Resolve ${openExceptions.length} open ${openExceptions.length === 1 ? "issue" : "issues"}`,
            detail: "Review exceptions holding up warehouse work.",
            icon: AlertTriangle,
            tone: "warn",
            action: () => openView("exceptions", "open"),
          },
        ]
      : []),
    ...(packed
      ? [
          {
            title: `Review ${packed} packed ${packed === 1 ? "order" : "orders"}`,
            detail: "Confirm readiness for the carrier handoff.",
            icon: Truck,
            tone: "green",
            action: () => openView("fulfillment", "packed"),
          },
        ]
      : []),
    ...(totals.receiving
      ? [
          {
            title: `${totals.receiving} inbound ${totals.receiving === 1 ? "receipt" : "receipts"} open`,
            detail: "Receive arrivals and plan putaway.",
            icon: ArrowDownToLine,
            tone: "blue",
            action: () => openView("receiving", "inbound_queue"),
          },
        ]
      : []),
    ...(receivingUnits > 0
      ? [
          {
            title: `${fmt(receivingUnits)} units at receiving`,
            detail: "Review received goods and plan putaway.",
            icon: Boxes,
            tone: "blue",
            action: () => openView("receiving"),
          },
        ]
      : []),
    ...(pending
      ? [
          {
            title: `Review ${pending} unallocated ${pending === 1 ? "order" : "orders"}`,
            detail: "Check available stock and allocate work.",
            icon: ClipboardCheck,
            tone: "blue",
            action: () => openView("fulfillment", "pending"),
          },
        ]
      : []),
  ].slice(0, 3);
  if (!nextActions.length && totals.active > 0) {
    nextActions.push({
      title: `Review ${totals.active} active ${totals.active === 1 ? "order" : "orders"}`,
      detail: "Continue picking, packing, or partial shipments.",
      icon: PackageCheck,
      tone: "blue",
      action: () => openView("fulfillment"),
    });
  }
  return (
    <>
      <div className="metrics">
        <Metric
          label="Units on hand"
          value={fmt(totals.onHand)}
          icon={Boxes}
          tone="blue"
          note={`${s.products.length} SKUs across ${s.locations.length} locations`}
        />
        <Metric
          label="Available in storage"
          value={fmt(
            availableStorage.reduce((n, b) => n + b.on_hand - b.reserved, 0),
          )}
          icon={Package}
          note={`${fmt(totals.reserved)} units reserved across the floor`}
        />
        <Metric
          label="Orders in progress"
          value={fmt(totals.active)}
          icon={PackageCheck}
          tone="purple"
          note={`${
            s.orders.filter((o) => o.status === "packed").length
          } packed · review before carrier handoff`}
        />
        <Metric
          label="Needs attention"
          value={fmt(openExceptions.length + lowStock.length)}
          icon={AlertTriangle}
          note={`${openExceptions.length} open issues · ${lowStock.length} low-stock SKUs`}
          attention={openExceptions.length + lowStock.length > 0}
        />
      </div>
      <section className="next-actions" aria-labelledby="next-actions-title">
        <div className="section-caption">
          <h2 id="next-actions-title">Your next moves</h2>
          <span>Review issues first, then move work forward</span>
        </div>
        {nextActions.length ? (
          <div className="next-actions-grid">
            {nextActions.map((item, index) => (
              <button
                className={`next-action ${item.tone}`}
                key={item.title}
                onClick={item.action}
              >
                <span className="next-action-icon">
                  <item.icon size={19} />
                </span>
                <span className="next-action-copy">
                  <small>0{index + 1}</small>
                  <strong>{item.title}</strong>
                  <span>{item.detail}</span>
                </span>
                <ArrowRight size={17} className="next-action-arrow" />
              </button>
            ))}
          </div>
        ) : (
          <div className="next-actions-clear">
            <CheckCheck size={20} />
            <span>No pending orders, receipts, or open issues.</span>
            <button className="text-btn" onClick={() => navigate("inventory")}>
              Review inventory <ArrowRight size={15} />
            </button>
          </div>
        )}
      </section>
      <div className="overview-grid">
        <Section
          title="Fulfillment flow"
          detail="Current work across the warehouse"
          action={
            <button
              className="text-btn"
              onClick={() => navigate("fulfillment")}
            >
              View orders <ArrowUpRight size={15} />
            </button>
          }
        >
          <div className="flow">
            {[
              {
                status: "pending",
                title: "To allocate",
                icon: ClipboardCheck,
              },
              { status: "allocated", title: "To pick", icon: Box },
              {
                status: "picked",
                title: "To pack",
                icon: PackageCheck,
              },
              { status: "packed", title: "To hand off", icon: Truck },
            ].map((f, i) => (
              <div className="flow-step" key={f.status}>
                <button
                  onClick={() => {
                    navigate("fulfillment");
                    setFilter(
                      f.status === "allocated" ? "pick_queue" : f.status,
                    );
                  }}
                >
                  <span className={`flow-icon step-${i}`}>
                    <f.icon size={20} />
                  </span>
                  <strong>
                    {fmt(
                      s.orders.filter((o) =>
                        f.status === "allocated"
                          ? ["allocated", "picking"].includes(o.status)
                          : o.status === f.status,
                      ).length,
                    )}
                  </strong>
                  <span>{f.title}</span>
                </button>
                {i < 3 && <ChevronRight className="flow-arrow" size={18} />}
              </div>
            ))}
          </div>
          <div className="flow-footer">
            <span>
              <span className="live-dot" />
              From reservation to physical handoff
            </span>
            <span>
              {s.orders.filter((o) => o.status === "shipped").length} completed
            </span>
          </div>
        </Section>
        <section className="warehouse-card">
          <div className="warehouse-card-top">
            <span className="warehouse-code">{s.warehouse.code}</span>
            <Badge tone={reconciliation?.ok ? "green" : "red"}>
              <i />
              {reconciliation?.ok
                ? "Operational balances checked"
                : "Balances require review"}
            </Badge>
          </div>
          <h2>{s.warehouse.name}</h2>
          <p>
            <MapPin size={14} />
            {s.warehouse.city}
          </p>
          <div
            className="floor-distribution"
            aria-label="Physical units by warehouse area"
          >
            {floor.map((area) => (
              <div className={`floor-area ${area.kind}`} key={area.kind}>
                <div>
                  <span>
                    <area.icon size={14} />
                    {area.title}
                  </span>
                  <strong>
                    {fmt(area.units)} <small>units</small>
                  </strong>
                </div>
                <div className="floor-track" aria-hidden="true">
                  <span
                    style={{
                      width: `${totals.onHand > 0 ? Math.min(100, (area.units / totals.onHand) * 100) : 0}%`,
                    }}
                  />
                </div>
              </div>
            ))}
          </div>
          <div className="warehouse-card-foot">
            <span>
              {s.locations.filter((l) => l.kind === "storage").length} storage
              bins
            </span>
            <button className="text-btn" onClick={() => navigate("locations")}>
              Explore locations <ArrowUpRight size={14} />
            </button>
          </div>
        </section>
      </div>
      <div className="overview-grid bottom-grid">
        <Section
          title="Active orders"
          detail="Urgent and high-priority work first · oldest first within each priority"
          action={
            <button
              className="text-btn"
              onClick={() => navigate("fulfillment")}
            >
              All orders <ArrowUpRight size={15} />
            </button>
          }
        >
          {orderTable(
            prioritizeOrders(s.orders.filter(activeOrder)).slice(0, 5),
          )}
        </Section>
        <div className="right-stack">
          <Section
            title="Attention queue"
            action={
              <span className="count-pill">
                {openExceptions.length + lowStock.length}
              </span>
            }
          >
            <div className="attention-list">
              {openExceptions.slice(0, 2).map((e) => (
                <button key={e.id} onClick={() => navigate("exceptions")}>
                  <span className="attention-icon">
                    <AlertTriangle size={17} />
                  </span>
                  <div>
                    <strong>{e.category}</strong>
                    <p>{e.detail}</p>
                  </div>
                  <ChevronRight size={16} />
                </button>
              ))}
              {lowStock.slice(0, 2).map((p) => (
                <button
                  key={p.sku}
                  onClick={() => {
                    navigate("inventory");
                    setSearch(p.sku);
                  }}
                >
                  <span className="attention-icon muted">
                    <Boxes size={17} />
                  </span>
                  <div>
                    <strong>{p.name}</strong>
                    <p>Below reorder point of {p.reorder_point} units</p>
                  </div>
                  <ChevronRight size={16} />
                </button>
              ))}
              {!openExceptions.length && !lowStock.length && (
                <div className="all-clear">
                  <CheckCheck size={22} />
                  No open issues. Your floor is clear.
                </div>
              )}
            </div>
          </Section>
          <Section title="Latest movements">
            <div className="recent-movements">
              {s.movements
                .slice(-3)
                .reverse()
                .map((m) => (
                  <div key={m.id}>
                    <span className={`movement-icon ${m.kind}`}>
                      <ArrowDownLeft size={16} />
                    </span>
                    <div>
                      <strong>{m.sku}</strong>
                      <small>
                        {label(m.kind)} · {m.to ?? "Carrier handoff"}
                      </small>
                    </div>
                    <b>
                      {fmt(m.quantity)}
                      <small>units</small>
                    </b>
                  </div>
                ))}
              {!s.movements.length && (
                <p className="muted-text">
                  Your first stock movement will appear here.
                </p>
              )}
            </div>
          </Section>
        </div>
      </div>
    </>
  );
}
