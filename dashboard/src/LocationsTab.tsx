import { Badge } from "./WarehouseUi";
import { fmt } from "./format";
import type { Tab } from "./uiTypes";
import { ArrowRight, Warehouse } from "lucide-react";
import type { WarehouseState } from "./types";

/** Locations tab: bins by zone and kind. */
export default function LocationsTab({
  matches,
  navigate,
  s,
  setSearch,
}: {
  matches: (...values: (string | undefined | null)[]) => boolean;
  navigate: (next: Tab) => void;
  s: WarehouseState;
  setSearch: (value: string) => void;
}) {
  return (
    <div className="locations-grid">
      {s.locations
        .filter((l) => matches(l.code, l.zone, l.kind))
        .map((l) => {
          const stock = s.stocks.filter((b) => b.location === l.code);
          return (
            <section key={l.code} className="location-card">
              <div>
                <span className={`location-icon ${l.kind}`}>
                  <Warehouse size={23} />
                </span>
                <Badge tone={l.kind === "storage" ? "green" : "blue"}>
                  {l.kind}
                </Badge>
              </div>
              <h2>{l.code}</h2>
              <p>{l.zone}</p>
              <dl>
                <div>
                  <dt>Physical units</dt>
                  <dd>{fmt(stock.reduce((n, b) => n + b.on_hand, 0))}</dd>
                </div>
                <div>
                  <dt>SKUs in stock</dt>
                  <dd>
                    {
                      new Set(
                        stock.filter((b) => b.on_hand > 0).map((b) => b.sku),
                      ).size
                    }
                  </dd>
                </div>
                <div>
                  <dt>Reserved</dt>
                  <dd>{fmt(stock.reduce((n, b) => n + b.reserved, 0))}</dd>
                </div>
              </dl>
              <button
                className="text-btn"
                onClick={() => {
                  navigate("inventory");
                  setSearch(l.code);
                }}
              >
                View stock <ArrowRight size={14} />
              </button>
            </section>
          );
        })}
    </div>
  );
}
