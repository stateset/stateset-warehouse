import { useRef, useState } from "react";
import { validUnitInputs } from "./unitInputs";
import type { Command, WarehouseState } from "./types";

type Page = {
  warehouse_id: string;
  sku: string;
  code: string | null;
  version: number;
  view: string;
  items: Record<string, any>[];
  total_records: number;
  next_cursor: string | null;
  page_source_ok: boolean;
};
export default function UnitPacksPanel({
  state,
  role,
  disabled,
  request,
  onSubmit,
}: {
  state: WarehouseState;
  role: string;
  disabled: boolean;
  request: (path: string) => Promise<any>;
  onSubmit: (command: Command, version: number) => Promise<boolean>;
}) {
  const [sku, setSku] = useState(state.products[0]?.sku ?? "");
  const [code, setCode] = useState("CASE");
  const [label, setLabel] = useState("Case");
  const [factor, setFactor] = useState("12");
  const [enabled, setEnabled] = useState(true);
  const [reason, setReason] = useState("");
  const [review, setReview] = useState<{ page: Page; command: Command } | null>(
    null,
  );
  const [page, setPage] = useState<Page | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const generation = useRef(0);
  const invalidate = () => {
    generation.current++;
    setReview(null);
    setPage(null);
    setError("");
  };
  const base = `/v1/warehouses/${encodeURIComponent(state.warehouse.id)}`;
  const checked = async (
    path: string,
    view: string,
    exactCode: string | null,
  ): Promise<Page> => {
    const p: Page = await request(path);
    if (
      p?.warehouse_id !== state.warehouse.id ||
      p.sku !== sku ||
      p.code !== exactCode ||
      p.view !== view ||
      !Number.isSafeInteger(p.version) ||
      p.version !== state.version ||
      !Array.isArray(p.items) ||
      p.items.length > 25 ||
      !Number.isSafeInteger(p.total_records) ||
      p.total_records < p.items.length ||
      p.page_source_ok !== true ||
      p.items.some(
        (i) =>
          i.sku !== sku ||
          i.source_ok !== true ||
          !Number.isSafeInteger(i.version) ||
          i.version < 1 ||
          i.version > p.version ||
          (exactCode !== null && i.code !== exactCode) ||
          !Number.isSafeInteger(i.each_per_pack) ||
          i.each_per_pack < 1 ||
          i.each_per_pack > 1000000 ||
          (view !== "observations"
            ? typeof i.id !== "string" ||
              !i.id ||
              typeof i.enabled !== "boolean"
            : typeof i.event_id !== "string" ||
              !i.event_id ||
              !Number.isSafeInteger(i.ordinal) ||
              i.ordinal < 1 ||
              i.ordinal > 100 ||
              !validUnitInputs([
                {
                  target: i.target,
                  sku: i.sku,
                  pack_code: i.pack_code,
                  definition_id: i.definition_id,
                  pack_quantity: i.pack_quantity,
                  each_per_pack: i.each_per_pack,
                },
              ]) ||
              i.each_quantity !== i.pack_quantity * i.each_per_pack),
      ) ||
      !(p.next_cursor === null || typeof p.next_cursor === "string")
    )
      throw new Error(
        "Pack source or review could not be verified. Refresh before continuing.",
      );
    return p;
  };
  const load = async (view: "history" | "observations", more = false) => {
    const epoch = ++generation.current;
    setBusy(true);
    setError("");
    setReview(null);
    try {
      const q = new URLSearchParams({
        sku,
        limit: "25",
        review: String(state.version),
      });
      if (view === "history") q.set("history", "true");
      if (more && page?.next_cursor) q.set("after", page.next_cursor);
      const p = await checked(
        `${base}/${view === "history" ? "unit-packs" : "unit-observations"}?${q}`,
        view,
        null,
      );
      if (epoch !== generation.current) return;
      const items = more ? [...(page?.items ?? []), ...p.items] : p.items;
      if (
        new Set(
          items.map((i) =>
            view === "history" ? i.id : JSON.stringify([i.event_id, i.ordinal]),
          ),
        ).size !== items.length ||
        items.length > p.total_records ||
        (!p.next_cursor && items.length !== p.total_records)
      )
        throw new Error("Pack evidence is incomplete. Start a fresh review.");
      setPage({ ...p, items });
    } catch (e) {
      if (epoch === generation.current) {
        setPage(null);
        setError((e as Error).message);
      }
    } finally {
      setBusy(false);
    }
  };
  const prepare = async () => {
    const epoch = ++generation.current;
    setBusy(true);
    setError("");
    setReview(null);
    try {
      const q = new URLSearchParams({
        sku,
        code,
        limit: "25",
        review: String(state.version),
      });
      const p = await checked(`${base}/unit-packs?${q}`, "current", code);
      if (
        p.next_cursor !== null ||
        p.items.length !== p.total_records ||
        p.items.length > 1
      )
        throw new Error("Exact pack definition review is incomplete.");
      if (epoch === generation.current)
        setReview({
          page: p,
          command: {
            type: "set_unit_pack",
            sku,
            code,
            label,
            each_per_pack: Number(factor),
            enabled,
            reason,
          },
        });
    } catch (e) {
      if (epoch === generation.current) setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const blocked = disabled || busy;
  return (
    <details className="workflow-panel unit-packs-panel">
      <summary>Pack units</summary>
      <section aria-label="Pack units">
        <p>
          Record whole cases, inner packs or pallets against a reviewed each
          count. Existing stock and earlier observations keep their original
          quantities.
        </p>
        <label>
          Pack product
          <select
            aria-label="Pack product"
            value={sku}
            disabled={blocked}
            onChange={(e) => {
              invalidate();
              setSku(e.target.value);
            }}
          >
            {state.products.map((p) => (
              <option key={p.sku} value={p.sku}>
                {p.name} · {p.sku}
              </option>
            ))}
          </select>
        </label>
        <div className="workflow-actions">
          <button
            className="btn"
            disabled={blocked || !sku}
            onClick={() => void load("history")}
          >
            Review pack history
          </button>
          <button
            className="btn"
            disabled={blocked || !sku}
            onClick={() => void load("observations")}
          >
            Review pack observations
          </button>
        </div>
        {page && (
          <div>
            <p>
              {page.items.length} of {page.total_records} original {page.view}{" "}
              records · version {page.version}
            </p>
            <ul>
              {page.items.map((i) => (
                <li
                  key={String(i.id ?? `${i.event_id}:${i.ordinal}`)}
                  style={{ overflowWrap: "anywhere" }}
                >
                  {String(i.code ?? i.pack_code)} ·{" "}
                  {i.pack_quantity !== undefined
                    ? `${i.pack_quantity} × ${i.each_per_pack} = ${i.each_quantity} each`
                    : `${i.each_per_pack} each · ${i.enabled ? "enabled" : "disabled"}`}{" "}
                  · {String(i.actor)} · original{" "}
                  {String(i.definition_id ?? i.id)}
                </li>
              ))}
            </ul>
            {page.next_cursor && (
              <button
                className="btn"
                disabled={blocked || page.version !== state.version}
                onClick={() =>
                  void load(page.view as "history" | "observations", true)
                }
              >
                More pack evidence
              </button>
            )}
            {page.version !== state.version && (
              <p role="status">
                Warehouse changed. Start a fresh evidence review.
              </p>
            )}
          </div>
        )}
        {role === "admin" && (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              void prepare();
            }}
          >
            <div className="form-grid">
              <label>
                Pack code
                <input
                  value={code}
                  pattern={"[A-Z0-9_\\-]{1,32}"}
                  maxLength={32}
                  required
                  disabled={blocked}
                  onChange={(e) => {
                    invalidate();
                    setCode(e.target.value);
                  }}
                />
              </label>
              <label>
                Pack label
                <input
                  value={label}
                  maxLength={240}
                  required
                  disabled={blocked}
                  onChange={(e) => {
                    invalidate();
                    setLabel(e.target.value);
                  }}
                />
              </label>
              <label>
                Each per pack
                <input
                  type="number"
                  min={1}
                  max={1000000}
                  step={1}
                  value={factor}
                  required
                  disabled={blocked}
                  onChange={(e) => {
                    invalidate();
                    setFactor(e.target.value);
                  }}
                />
              </label>
              <label>
                Pack status
                <select
                  value={String(enabled)}
                  disabled={blocked}
                  onChange={(e) => {
                    invalidate();
                    setEnabled(e.target.value === "true");
                  }}
                >
                  <option value="true">Enabled</option>
                  <option value="false">Disabled</option>
                </select>
              </label>
              <label>
                Pack evidence / reason
                <textarea
                  value={reason}
                  required
                  maxLength={240}
                  disabled={blocked}
                  onChange={(e) => {
                    invalidate();
                    setReason(e.target.value);
                  }}
                />
              </label>
            </div>
            <button
              className="btn"
              type="submit"
              disabled={blocked || !sku || code === "EA"}
            >
              Review pack change
            </button>
          </form>
        )}
        {review && (
          <div>
            <p>
              Original:{" "}
              {review.page.items.length
                ? `${review.page.items[0].each_per_pack} each · ${review.page.items[0].enabled ? "enabled" : "disabled"}`
                : "new pack code"}
              . Proposed: {String(review.command.each_per_pack)} each ·{" "}
              {review.command.enabled ? "enabled" : "disabled"}.{" "}
              {String(review.command.reason)}
            </p>
            <button
              className="btn primary"
              disabled={blocked || review.page.version !== state.version}
              onClick={async () => {
                setBusy(true);
                try {
                  if (await onSubmit(review.command, review.page.version)) {
                    setReview(null);
                    setPage(null);
                  }
                } finally {
                  setBusy(false);
                }
              }}
            >
              Confirm pack definition
            </button>
            {review.page.version !== state.version && (
              <p>Warehouse changed. Review the change again.</p>
            )}
          </div>
        )}
        {error && <p role="alert">{error}</p>}
      </section>
    </details>
  );
}
