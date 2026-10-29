import { Empty, Section } from "./WarehouseUi";
import { CycleCountPanel, LabelPrintPanel } from "./PlanningPanels";
import { availability, availabilityLabel } from "./lotAvailability";
import { fmt } from "./format";
import type { Product, Session, Stock, WarehouseState } from "./types";
import { Package } from "lucide-react";
import type { RequestJson, SubmitCommand, Totals } from "./uiTypes";
import ReplenishmentPanel from "./ReplenishmentPanel";
import LotControlsPanel from "./LotControlsPanel";
import LotTracePanel from "./LotTracePanel";
import UnitPacksPanel from "./UnitPacksPanel";
import SerialTracePanel from "./SerialTracePanel";

/** Inventory tab: stock by bin with moves, counts, planning and labels. */
export default function InventoryTab({
  admin,
  availabilityTime,
  canWork,
  countStock,
  disabled,
  lowStock,
  matches,
  moveStock,
  clearSearch,
  request,
  s,
  session,
  submit,
  totals,
}: {
  admin: boolean;
  availabilityTime: bigint | undefined;
  canWork: boolean;
  countStock: (stock: Stock) => void;
  disabled: boolean;
  lowStock: Product[];
  matches: (...values: (string | undefined | null)[]) => boolean;
  moveStock: (stock?: Stock) => void;
  clearSearch: () => void;
  request: RequestJson;
  s: WarehouseState;
  session: Session;
  submit: SubmitCommand;
  totals: Totals;
}) {
  const stocks = s.stocks
    .filter((stock) =>
      matches(
        stock.sku,
        stock.location,
        stock.lot,
        s.products.find((product) => product.sku === stock.sku)?.name,
      ),
    )
    .sort(
      (a, b) =>
        a.sku.localeCompare(b.sku) || a.location.localeCompare(b.location),
    );
  return (
    <>
      <div className="inventory-summary">
        <span>
          <i className="legend-dot" />
          {s.products.length} catalog products
        </span>
        <span>{fmt(totals.onHand)} physical units</span>
        <span>{fmt(totals.reserved)} reserved units</span>
        <span>{lowStock.length} below reorder point</span>
      </div>
      <Section
        title="Stock by location"
        detail={`${stocks.length} of ${s.stocks.length} stock positions shown · allocation requires an available lot in storage`}
      >
        <div
          className="table-scroll operational-scroll"
          role="region"
          aria-label="Stock by location"
          tabIndex={0}
        >
          <table className="operational-table stock-table">
            <thead>
              <tr>
                <th>Product</th>
                <th>Location / lot</th>
                <th className="numeric">On hand</th>
                <th className="numeric">Reserved</th>
                <th className="numeric">Unreserved</th>
                <th>Lot availability</th>
                <th>Actions</th>
              </tr>
            </thead>
            <tbody>
              {stocks.map((b) => (
                <tr key={[b.sku, b.lot, b.location].join("|")}>
                  <td>
                    <div className="product-cell">
                      <span className="product-thumb">
                        <Package size={20} />
                      </span>
                      <div>
                        <strong>
                          {s.products.find((p) => p.sku === b.sku)?.name}
                        </strong>
                        <span className="subline mono">{b.sku}</span>
                      </div>
                    </div>
                  </td>
                  <td data-label="Location / lot">
                    <span className="bin-tag">{b.location}</span>
                    <span className="subline mono">{b.lot}</span>
                  </td>
                  <td className="mono strong numeric" data-label="On hand">
                    {fmt(b.on_hand)}
                  </td>
                  <td className="mono numeric" data-label="Reserved">
                    {fmt(b.reserved)}
                  </td>
                  <td className="mono numeric" data-label="Unreserved">
                    {fmt(b.on_hand - b.reserved)}
                  </td>
                  <td data-label="Lot availability">
                    {
                      availabilityLabel[
                        availability(s, b.sku, b.lot, availabilityTime)
                      ]
                    }
                  </td>
                  <td className="stock-actions">
                    <div className="row-actions">
                      {canWork && b.location !== "STAGE-01" && (
                        <button
                          className="mini-btn"
                          disabled={disabled || b.on_hand === b.reserved}
                          onClick={() => moveStock(b)}
                        >
                          Move
                        </button>
                      )}
                      {admin && b.location !== "STAGE-01" && (
                        <button
                          className="mini-btn"
                          disabled={disabled}
                          onClick={() => countStock(b)}
                        >
                          Count
                        </button>
                      )}
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {!stocks.length && s.stocks.length > 0 && (
            <Empty
              title="No matching stock"
              detail="Try a product name, SKU, location, or lot. Your stock remains available when you clear the search."
              action={
                <button className="btn" onClick={clearSearch}>
                  Clear search
                </button>
              }
            />
          )}
          {!s.stocks.length && (
            <Empty
              title="No stock recorded yet"
              detail="Create a product and receipt, then receive goods into the warehouse."
            />
          )}
        </div>
      </Section>
      <section
        className="inventory-tools-section"
        aria-labelledby="inventory-tools-title"
      >
        <div className="section-caption">
          <h2 id="inventory-tools-title">Inventory tools</h2>
          <span>Plan, trace, and maintain your stock</span>
        </div>
        <div className="inventory-tools">
          {canWork && (
            <ReplenishmentPanel
              key={s.warehouse.id}
              state={s}
              role={session.identity.role}
              disabled={disabled}
              request={request}
              onSubmit={submit}
            />
          )}
          {canWork && (
            <CycleCountPanel
              key={`counts-${s.warehouse.id}`}
              state={s}
              disabled={disabled}
              request={request}
              onSubmit={submit}
            />
          )}
          <LabelPrintPanel
            key={`labels-${s.warehouse.id}`}
            state={s}
            request={request}
          />
          <SerialTracePanel
            key={`serial-${s.warehouse.id}`}
            state={s}
            request={request}
          />
          <UnitPacksPanel
            key={s.warehouse.id}
            state={s}
            role={session.identity.role}
            disabled={disabled}
            request={request}
            onSubmit={submit}
          />
          <LotControlsPanel
            key={s.warehouse.id}
            state={s}
            role={session.identity.role}
            disabled={disabled}
            request={request}
            onSubmit={submit}
          />
          <LotTracePanel
            key={`${session.identity.tenant}:${session.identity.actor}`}
            state={s}
            disabled={disabled}
            request={request}
          />
        </div>
      </section>
      <Section
        title="Product catalog"
        detail="Products can exist before their first physical receipt."
      >
        <div className="catalog-grid">
          {s.products
            .filter((p) => matches(p.sku, p.name, p.category))
            .map((p) => (
              <div key={p.sku}>
                <span className="product-thumb">
                  <Package size={21} />
                </span>
                <div>
                  <strong>{p.name}</strong>
                  <small>
                    {p.sku} · {p.category}
                  </small>
                  <small>Reorder below {p.reorder_point} each</small>
                </div>
              </div>
            ))}
        </div>
      </Section>
    </>
  );
}
