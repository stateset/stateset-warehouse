import { useEffect, useRef, useState, type RefObject } from "react";
import type { UnitInput, UnitPack } from "./types";

/** A captured definition stays captured until the operator explicitly changes units. */
export default function UnitQuantityInput({
  packs = [],
  sku,
  target,
  value,
  onChange,
  label,
  name,
  min = 1,
  max = 1000000,
  disabled,
  inputRef,
}: {
  packs?: UnitPack[];
  sku: string;
  target: string[];
  value: string;
  onChange: (each: string, observation?: UnitInput) => void;
  label: string;
  name?: string;
  min?: number;
  max?: number;
  disabled?: boolean;
  inputRef?: RefObject<HTMLInputElement>;
}) {
  const [chosen, setChosen] = useState<UnitPack | null>(null);
  const [count, setCount] = useState("");
  const ownRef = useRef<HTMLInputElement>(null);
  const field = inputRef ?? ownRef;
  const callback = useRef(onChange);
  callback.current = onChange;
  const options = packs.filter((p) => p.sku === sku);
  const stale =
    !!chosen &&
    (chosen.sku !== sku ||
      !options.some(
        (p) =>
          p.id === chosen.id &&
          p.code === chosen.code &&
          p.each_per_pack === chosen.each_per_pack,
      ));
  useEffect(() => {
    field.current?.setCustomValidity(
      stale
        ? "Pack definition changed. Review and explicitly choose units again."
        : "",
    );
    if (stale) callback.current("", undefined);
  }, [stale, field]);
  const changeCount = (raw: string, pack: UnitPack) => {
    setCount(raw);
    const n = Number(raw),
      each = n * pack.each_per_pack;
    const valid =
      /^\d+$/.test(raw) &&
      Number.isSafeInteger(n) &&
      n >= (min === 0 ? 0 : 1) &&
      n <= 1000000 &&
      Number.isSafeInteger(each) &&
      each >= min &&
      each <= max &&
      !stale;
    callback.current(
      valid ? String(each) : "",
      valid
        ? {
            target: [...target],
            sku,
            pack_code: pack.code,
            definition_id: pack.id,
            pack_quantity: n,
            each_per_pack: pack.each_per_pack,
          }
        : undefined,
    );
  };
  return (
    <div className="unit-quantity">
      {(options.length > 0 || chosen) && (
        <label>
          Units for {label}
          <select
            aria-label={`Units for ${label}`}
            disabled={disabled}
            value={chosen?.id ?? "EA"}
            onChange={(e) => {
              const pack = options.find((p) => p.id === e.target.value) ?? null;
              setChosen(pack);
              setCount("");
              callback.current("", undefined);
              field.current?.setCustomValidity("");
            }}
          >
            <option value="EA">Each</option>
            {stale && (
              <option value={chosen!.id}>
                Original {chosen!.code} · review required
              </option>
            )}
            {options.map((p) => (
              <option key={p.id} value={p.id}>
                {p.label} · {p.each_per_pack} each
              </option>
            ))}
          </select>
        </label>
      )}
      <label>
        {chosen ? `${label} (${chosen.code} packs)` : label}
        <input
          ref={field}
          aria-label={chosen ? `${label} (${chosen.code} packs)` : label}
          name={chosen ? undefined : name}
          type="number"
          step="1"
          min={chosen ? (min === 0 ? 0 : 1) : min}
          max={
            chosen
              ? Math.min(1000000, Math.floor(max / chosen.each_per_pack))
              : max
          }
          value={chosen ? count : value}
          disabled={disabled}
          required
          onChange={(e) =>
            chosen
              ? changeCount(e.target.value, chosen)
              : onChange(e.target.value)
          }
        />
      </label>
      {chosen && (
        <>
          <input type="hidden" name={name} value={value} />
          <small>
            {stale
              ? "Pack definition changed. Choose units again before saving."
              : value !== ""
                ? `${count} × ${chosen.each_per_pack} = ${value} each`
                : "Enter the observed whole pack count."}
          </small>
        </>
      )}
    </div>
  );
}
