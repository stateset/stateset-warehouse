import { useEffect, useRef, useState } from "react";
import type { SerialInput, Journal } from "./types";
import { serialText, type SerialTarget } from "./serialInputs";
import "./Serials.css";

/** Literal observations only. Never prefill a scan from a custody projection. */
export default function SerialObservationForm({
  targets,
  operation,
  originalRequest,
  onConfirm,
  onCancel,
}: {
  targets: SerialTarget[];
  operation: string;
  originalRequest: Journal["request"];
  onConfirm: (inputs: SerialInput[]) => void;
  onCancel: () => void;
}) {
  const command = originalRequest.command;
  const references = [
    ["Original order", command.order_id],
    ["Original receipt", command.receipt_id],
    ["Original line", command.line_id],
    ["Original transfer", command.transfer_id],
    ["Original loss", command.loss_id],
    [
      "Source bin",
      command.from ?? (operation === "pick" ? command.location : undefined),
    ],
    [
      "Destination bin",
      command.to ??
        (["receive", "receive_transfer", "unpick"].includes(operation)
          ? command.location
          : undefined),
    ],
    ["Counted bin", operation === "count_stock" ? command.location : undefined],
    ["Lot", command.lot],
    ["Destination facility", command.destination_warehouse_id],
  ].filter((entry): entry is [string, string] => typeof entry[1] === "string");
  const [inputs, setInputs] = useState<SerialInput[]>(() =>
    targets.map((t) => ({ target: [...t.target], sku: t.sku, serials: [] })),
  );
  const current = useRef(inputs);
  const [zeros, setZeros] = useState<Set<number>>(new Set());
  const [error, setError] = useState("");
  const [pages, setPages] = useState<Record<number, number>>({});
  const fields = useRef<(HTMLInputElement | null)[]>([]);
  useEffect(() => {
    fields.current.find(Boolean)?.focus();
  }, []);
  const ready = inputs.every(
    (i, n) =>
      i.serials.length === targets[n].each_quantity &&
      (targets[n].each_quantity !== 0 || zeros.has(n)),
  );
  const add = (index: number) => {
    const field = fields.current[index];
    const value = field?.value ?? "";
    if (!serialText(value)) {
      setError(
        "Scan the literal serial: 1–240 UTF-8 bytes, without surrounding spaces or control characters.",
      );
      return;
    }
    const i = current.current[index];
    if (i.serials.length >= targets[index].each_quantity) {
      setError("This original quantity already has its complete serial set.");
      return;
    }
    if (
      current.current.some((g) => g.sku === i.sku && g.serials.includes(value))
    ) {
      setError(
        "This exact serial was already scanned for this SKU. Nothing was saved.",
      );
      if (field) field.value = "";
      return;
    }
    const next = current.current.map((g, n) =>
      n === index ? { ...g, serials: [...g.serials, value] } : g,
    );
    current.current = next;
    setInputs(next);
    setPages((p) => ({
      ...p,
      [index]: Math.floor((next[index].serials.length - 1) / 50),
    }));
    setError("");
    if (field) {
      field.value = "";
      field.focus();
    }
  };
  return (
    <form
      className="serial-observations"
      onSubmit={(e) => {
        e.preventDefault();
        if (!ready) return;
        if (
          new TextEncoder().encode(
            JSON.stringify({
              ...originalRequest,
              serial_inputs: current.current,
            }),
          ).length >
          256 * 1024
        ) {
          setError(
            "The complete original request exceeds one save. Your scanned labels remain here for review; none were removed or saved. Larger complete counts need reviewed count sessions.",
          );
          return;
        }
        onConfirm(structuredClone(current.current));
      }}
    >
      <p>
        Scan each physical unit for this {operation.replaceAll("_", " ")}.
        Labels are case-sensitive and keep their original lot. These labels will
        be saved with the original action.
      </p>
      {references.length > 0 && (
        <dl
          className="serial-action-context"
          aria-label="Original physical action"
        >
          {references.map(([label, value]) => (
            <div key={label}>
              <dt>{label}</dt>
              <dd>{value}</dd>
            </div>
          ))}
        </dl>
      )}
      {targets.map((t, index) => {
        const group = inputs[index],
          page = pages[index] ?? 0;
        const line = Array.isArray(command.lines)
          ? command.lines.find(
              (l: any) =>
                l && (l.id === t.target[0] || l.line_id === t.target[0]),
            )
          : undefined;
        return (
          <fieldset key={JSON.stringify(t.target)}>
            <legend>
              {t.sku} ·{" "}
              {t.target.length ? t.target.join(" / ") : "Physical quantity"}
            </legend>
            {typeof (line?.order_line_id ?? line?.line_id) === "string" && (
              <p>Original order line: {line.order_line_id ?? line.line_id}</p>
            )}
            {typeof line?.location === "string" && (
              <p>
                {operation === "pack_shipment"
                  ? "Original allocation bin"
                  : "Source bin"}
                : {line.location}
              </p>
            )}
            {typeof line?.lot === "string" && <p>Original lot: {line.lot}</p>}
            <p>
              {group.serials.length} of {t.each_quantity} serials observed
            </p>
            {t.each_quantity === 0 ? (
              <label className="serial-zero">
                <input
                  type="checkbox"
                  checked={zeros.has(index)}
                  onChange={(e) =>
                    setZeros((z) => {
                      const n = new Set(z);
                      if (e.target.checked) n.add(index);
                      else n.delete(index);
                      return n;
                    })
                  }
                />
                I physically observed no units for this complete count.
              </label>
            ) : (
              <>
                <label>
                  Scan serial for {t.sku}
                  {t.target.length ? ` · ${t.target.join(" / ")}` : ""}
                  <input
                    data-autofocus
                    ref={(el) => {
                      fields.current[index] = el;
                    }}
                    autoComplete="off"
                    spellCheck={false}
                    autoCapitalize="none"
                    onKeyDown={(e) => {
                      if (e.key === "Enter") {
                        e.preventDefault();
                        add(index);
                      }
                    }}
                  />
                </label>
                <button
                  type="button"
                  className="btn"
                  onClick={() => add(index)}
                >
                  Add serial
                </button>
                <ol start={page * 50 + 1}>
                  {group.serials
                    .slice(page * 50, page * 50 + 50)
                    .map((s, n) => (
                      <li key={s}>
                        <code>{s}</code>
                        <button
                          type="button"
                          aria-label={`Remove serial ${s}`}
                          onClick={() => {
                            const next = current.current.map((g, i) =>
                              i === index
                                ? {
                                    ...g,
                                    serials: g.serials.filter(
                                      (_, k) => k !== page * 50 + n,
                                    ),
                                  }
                                : g,
                            );
                            current.current = next;
                            setInputs(next);
                            setPages((p) => ({
                              ...p,
                              [index]: Math.min(
                                page,
                                Math.max(
                                  0,
                                  Math.ceil(next[index].serials.length / 50) -
                                    1,
                                ),
                              ),
                            }));
                          }}
                        >
                          Remove
                        </button>
                      </li>
                    ))}
                </ol>
                {group.serials.length > 50 && (
                  <div className="serial-pages">
                    <button
                      type="button"
                      disabled={page === 0}
                      onClick={() =>
                        setPages((p) => ({ ...p, [index]: page - 1 }))
                      }
                    >
                      Previous labels
                    </button>
                    <span>
                      Page {page + 1} of {Math.ceil(group.serials.length / 50)}
                    </span>
                    <button
                      type="button"
                      disabled={(page + 1) * 50 >= group.serials.length}
                      onClick={() =>
                        setPages((p) => ({ ...p, [index]: page + 1 }))
                      }
                    >
                      Next labels
                    </button>
                  </div>
                )}
              </>
            )}
          </fieldset>
        );
      })}
      {error && (
        <p role="alert" className="scanner-error">
          {error}
        </p>
      )}
      <p>
        For a count, scan the complete observed set, including a witnessed zero.
        Do not split a full count into partial saves. Confirm only after the
        physical action is complete.
      </p>
      <div className="form-actions">
        <button type="button" className="btn" onClick={onCancel}>
          Cancel serial review
        </button>
        <button className="btn primary" disabled={!ready}>
          Save with observed serials
        </button>
      </div>
    </form>
  );
}
