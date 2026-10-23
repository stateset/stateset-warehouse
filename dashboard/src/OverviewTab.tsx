import { Badge, Metric, Section, label } from "./WarehouseUi";
import { fmt } from "./format";
import type { ReconciliationProof } from "./proofView";
import type { Exception, Order, Product, Stock, WarehouseState } from "./types";
import type { Tab, Totals } from "./uiTypes";
import {
  ArrowDownLeft,
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
  return (
    <>
      <div className="metrics">
        <Metric
          label="Units on hand"
          value={fmt(totals.onHand)}
          icon={Boxes}
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
          note={`${
            s.orders.filter((o) => o.status === "packed").length
          } packed and ready for handoff`}
        />
        <Metric
          label="Needs attention"
          value={fmt(openExceptions.length + lowStock.length)}
          icon={AlertTriangle}
          note={`${openExceptions.length} open issues · ${lowStock.length} low-stock SKUs`}
          attention
        />
      </div>
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
                    setFilter(f.status);
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
              <span className="live-dot" />
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
          <div className="warehouse-map" aria-hidden="true">
            <span className="map-dock" />
            <div>
              {Array.from({ length: 24 }, (_, i) => (
                <i key={i} className={i % 7 === 0 ? "map-empty" : ""} />
              ))}
            </div>
            <span className="map-route" />
            <span className="map-point" />
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
          detail="The next moves on your warehouse floor"
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
            s.orders
              .filter(
                (o) =>
                  !["shipped", "cancelled", "closed_partial"].includes(
                    o.status,
                  ),
              )
              .slice(0, 5),
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
