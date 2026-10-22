import { useEffect, useRef, useState, type ReactNode } from "react";
import { Check, Loader2, Plus, ShieldCheck, X } from "lucide-react";
import type { Command, UnitInput, WarehouseState } from "./types";
import UnitQuantityInput from "./UnitQuantityInput";

type Field = {
  name: string;
  label: string;
  type?: "number" | "select" | "textarea";
  value?: string | number;
  options?: { value: string; label: string }[];
  min?: number;
  max?: number;
  placeholder?: string;
  optional?: boolean;
};
export type FormSpec = {
  title: string;
  subtitle?: string;
  fields: Field[];
  submit: string;
  build: (data: Record<string, string>) => Command;
  lines?: "receipt" | "order";
  unitSku?: string;
};

export function ModalFrame({
  suspended = false,
  title,
  close,
  children,
}: {
  title: string;
  close: () => void;
  suspended?: boolean;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (suspended) return;
    const previous = document.activeElement as HTMLElement | null;
    const el = ref.current;
    (
      el?.querySelector<HTMLElement>("[data-autofocus]") ??
      el?.querySelector<HTMLElement>("input,select,textarea,button")
    )?.focus();
    const listener = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
      if (e.key !== "Tab" || !el) return;
      const all = Array.from(
        el.querySelectorAll<HTMLElement>(
          "button:not(:disabled),input:not(:disabled),select:not(:disabled),textarea:not(:disabled),a[href]",
        ),
      );
      const first = all[0],
        last = all[all.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last?.focus();
      }
      if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first?.focus();
      }
    };
    document.addEventListener("keydown", listener);
    return () => {
      document.removeEventListener("keydown", listener);
      previous?.focus();
    };
  }, [close, suspended]);
  return (
    <div
      className="modal-backdrop"
      style={suspended ? { display: "none" } : undefined}
      aria-hidden={suspended || undefined}
    >
      <div
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        className="modal"
      >
        <div className="modal-head">
          <h2>{title}</h2>
          <button
            className="icon-btn"
            onClick={close}
            aria-label="Close dialog"
          >
            <X size={20} />
          </button>
        </div>
        {children}
      </div>
    </div>
  );
}

export function CommandForm({
  spec,
  state,
  busy,
  submit,
}: {
  spec: FormSpec;
  state: WarehouseState;
  busy: boolean;
  submit: (c: Command, inputs?: UnitInput[]) => Promise<unknown>;
}) {
  const [rootInputs, setRootInputs] = useState<
    Record<string, UnitInput | undefined>
  >({});
  const [quantities, setQuantities] = useState<Record<string, string>>({});
  const skuField = spec.fields.find((f) => f.name === "sku");
  const [selectedSku, setSelectedSku] = useState(
    String(
      spec.unitSku ?? skuField?.value ?? skuField?.options?.[0]?.value ?? "",
    ),
  );
  const [lines, setLines] = useState<
    {
      id: string;
      sku: string;
      quantity: number;
      lot: string;
      input?: UnitInput;
    }[]
  >([
    {
      id: crypto.randomUUID(),
      sku: state.products[0]?.sku ?? "",
      quantity: 1,
      lot: "UNTRACKED",
    },
  ]);
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        const data = Object.fromEntries(
          new FormData(e.currentTarget),
        ) as Record<string, string>;
        const c = spec.build(data);
        if (spec.lines)
          c.lines = lines.map((l) =>
            spec.lines === "receipt"
              ? { id: l.id, sku: l.sku, quantity: l.quantity, lot: l.lot }
              : { id: l.id, sku: l.sku, quantity: l.quantity },
          );
        void submit(
          c,
          spec.lines
            ? lines.flatMap((l) => (l.input ? [l.input] : []))
            : Object.values(rootInputs).filter((i): i is UnitInput => !!i),
        );
      }}
    >
      {spec.subtitle && <p className="form-subtitle">{spec.subtitle}</p>}
      <div className="form-grid">
        {spec.fields.map((f) =>
          selectedSku &&
          f.type === "number" &&
          ["quantity", "counted"].includes(f.name) ? (
            <UnitQuantityInput
              key={`${f.name}:${selectedSku}`}
              packs={state.unit_packs}
              sku={selectedSku}
              target={[]}
              value={quantities[f.name] ?? String(f.value ?? "")}
              onChange={(each, input) => {
                setQuantities((q) => ({ ...q, [f.name]: each }));
                setRootInputs((q) => ({ ...q, [f.name]: input }));
              }}
              label={f.label}
              name={f.name}
              min={f.min ?? 1}
              max={f.max ?? 1000000}
              disabled={busy}
            />
          ) : (
            <label key={f.name} className={f.type === "textarea" ? "full" : ""}>
              {f.label}
              {f.type === "select" ? (
                <select
                  aria-label={f.label}
                  name={f.name}
                  onChange={
                    f.name === "sku"
                      ? (e) => {
                          setSelectedSku(e.target.value);
                          setRootInputs({});
                          setQuantities({ quantity: "" });
                        }
                      : undefined
                  }
                  defaultValue={f.value ?? f.options?.[0]?.value}
                  required={!f.optional}
                >
                  {f.options?.map((o) => (
                    <option value={o.value} key={o.value}>
                      {o.label}
                    </option>
                  ))}
                </select>
              ) : f.type === "textarea" ? (
                <textarea
                  name={f.name}
                  defaultValue={f.value}
                  maxLength={240}
                  required={!f.optional}
                />
              ) : (
                <input
                  name={f.name}
                  type={f.type ?? "text"}
                  defaultValue={f.value}
                  min={f.min ?? 1}
                  max={f.max ?? 1000000}
                  step={f.type === "number" ? 1 : undefined}
                  maxLength={240}
                  placeholder={f.placeholder}
                  required={!f.optional}
                />
              )}
            </label>
          ),
        )}
      </div>
      {spec.lines && (
        <div className="line-editor">
          <div className="line-label">
            Items <span>Quantities in each units</span>
          </div>
          {lines.map((l, i) => (
            <div className="line-row" key={l.id}>
              <select
                aria-label={`SKU line ${i + 1}`}
                value={l.sku}
                onChange={(e) =>
                  setLines(
                    lines.map((x) =>
                      x.id === l.id
                        ? {
                            ...x,
                            sku: e.target.value,
                            quantity: 0,
                            input: undefined,
                          }
                        : x,
                    ),
                  )
                }
                required
              >
                {state.products.map((p) => (
                  <option key={p.sku} value={p.sku}>
                    {p.name} · {p.sku}
                  </option>
                ))}
              </select>
              {spec.lines === "receipt" && (
                <input
                  aria-label={`Lot line ${i + 1}`}
                  value={l.lot}
                  onChange={(e) =>
                    setLines(
                      lines.map((x) =>
                        x.id === l.id ? { ...x, lot: e.target.value } : x,
                      ),
                    )
                  }
                  maxLength={240}
                  required
                />
              )}
              <UnitQuantityInput
                key={`${l.id}:${l.sku}`}
                packs={state.unit_packs}
                sku={l.sku}
                target={[l.id]}
                value={l.quantity ? String(l.quantity) : ""}
                label={`Quantity line ${i + 1}`}
                disabled={busy}
                onChange={(each, input) =>
                  setLines((rows) =>
                    rows.map((x) =>
                      x.id === l.id
                        ? { ...x, quantity: Number(each), input }
                        : x,
                    ),
                  )
                }
              />
              <button
                type="button"
                className="icon-btn"
                aria-label={`Remove line ${i + 1}`}
                disabled={lines.length === 1}
                onClick={() => setLines(lines.filter((x) => x.id !== l.id))}
              >
                <X size={16} />
              </button>
            </div>
          ))}
          <button
            className="text-btn"
            type="button"
            disabled={lines.length >= 100}
            onClick={() =>
              setLines([
                ...lines,
                {
                  id: crypto.randomUUID(),
                  sku: state.products[0]?.sku ?? "",
                  quantity: 1,
                  lot: "UNTRACKED",
                },
              ])
            }
          >
            <Plus size={14} /> Add item
          </button>
        </div>
      )}
      <div className="form-foot">
        <span>
          <ShieldCheck size={14} /> Saved with an audit record
        </span>
        <button disabled={busy} className="btn primary" type="submit">
          {busy ? <Loader2 size={16} className="spin" /> : <Check size={16} />}
          {spec.submit}
        </button>
      </div>
    </form>
  );
}

