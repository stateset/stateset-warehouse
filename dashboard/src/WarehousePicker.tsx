import { useEffect, useRef, useState } from "react";
import { Warehouse } from "lucide-react";
import type { WarehouseState } from "./types";
import "./WarehousePicker.css";

type Facility = WarehouseState["warehouse"];
export default function WarehousePicker({
  current,
  disabled,
  admin,
  request,
  onSelect,
  onCreate,
}: {
  current: Facility;
  disabled: boolean;
  admin: boolean;
  request: (path: string) => Promise<any>;
  onSelect: (warehouse: string) => Promise<boolean>;
  onCreate: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [facilities, setFacilities] = useState<Facility[]>([]);
  const [after, setAfter] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const box = useRef<HTMLDivElement>(null);
  const sequence = useRef(0);
  const alive = useRef(true);
  const inFlight = useRef(false);
  useEffect(() => {
    alive.current = true;
    const outside = (event: MouseEvent) => {
      if (!box.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", outside);
    return () => {
      alive.current = false;
      ++sequence.current;
      document.removeEventListener("mousedown", outside);
    };
  }, []);
  const load = async (cursor: string | null = null) => {
    if (disabled || inFlight.current) return;
    inFlight.current = true;
    const generation = ++sequence.current;
    setBusy(true);
    setError("");
    try {
      const page = await request(
        `/v1/warehouses?limit=50${cursor ? `&after=${encodeURIComponent(cursor)}` : ""}`,
      );
      if (!alive.current || generation !== sequence.current) return;
      if (
        !Array.isArray(page.warehouses) ||
        page.warehouses.some(
          (row: any) => typeof row.warehouse?.id !== "string",
        ) ||
        (page.next_after != null &&
          (typeof page.next_after !== "string" || page.next_after === cursor))
      )
        throw new Error(
          "Warehouse list could not be verified. Refresh the list.",
        );
      setFacilities(
        (previous) =>
          [
            ...new Map([
              ...(cursor ? previous.map((w) => [w.id, w] as const) : []),
              ...page.warehouses.map(
                (row: any) => [row.warehouse.id, row.warehouse] as const,
              ),
            ]).values(),
          ] as Facility[],
      );
      setAfter(page.next_after ?? null);
    } catch (failure) {
      if (alive.current && generation === sequence.current)
        setError((failure as Error).message);
    } finally {
      if (alive.current && generation === sequence.current) {
        setBusy(false);
        inFlight.current = false;
      }
    }
  };
  const all = [
    ...new Map([
      ...facilities.map((w) => [w.id, w] as const),
      [current.id, current] as const,
    ]).values(),
  ];
  return (
    <div
      className="warehouse-picker"
      ref={box}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          setOpen(false);
          e.stopPropagation();
        }
      }}
    >
      <button
        type="button"
        className="icon-btn warehouse-picker-trigger"
        aria-label="Change warehouse"
        aria-describedby="active-warehouse-label"
        aria-expanded={open}
        aria-controls="warehouse-picker-menu"
        title={`${current.code} · ${current.name}`}
        disabled={disabled}
        onClick={() => {
          setOpen(!open);
          if (!open) void load();
        }}
      >
        <Warehouse size={18} />
        <span id="active-warehouse-label">
          {current.code} · {current.city}
        </span>
      </button>
      {open && (
        <div id="warehouse-picker-menu" className="warehouse-picker-menu">
          <label>
            Active warehouse
            <select
              aria-label="Active warehouse"
              value={current.id}
              disabled={disabled || busy}
              onChange={(e) => {
                const id = e.target.value;
                if (id !== current.id)
                  void onSelect(id).then((saved) => {
                    if (saved && alive.current) setOpen(false);
                  });
              }}
            >
              {all.map((w) => (
                <option key={w.id} value={w.id}>
                  {w.code} · {w.name} · {w.city}
                  {all.filter(
                    (other) =>
                      other.code === w.code &&
                      other.name === w.name &&
                      other.city === w.city,
                  ).length > 1
                    ? ` · ${w.id}`
                    : ""}
                </option>
              ))}
            </select>
          </label>
          <div className="warehouse-picker-actions">
            <button
              className="mini-btn"
              type="button"
              disabled={disabled || busy}
              onClick={() => void load()}
            >
              Refresh warehouse list
            </button>
            {after && (
              <button
                className="mini-btn"
                type="button"
                disabled={disabled || busy}
                onClick={() => void load(after)}
              >
                Load more warehouses
              </button>
            )}
            {admin && (
              <button
                className="btn"
                type="button"
                disabled={disabled || busy}
                onClick={() => {
                  setOpen(false);
                  onCreate();
                }}
              >
                New warehouse
              </button>
            )}
          </div>
          {busy && <p role="status">Loading warehouses…</p>}
          {error && <p role="alert">{error}</p>}
        </div>
      )}
    </div>
  );
}
