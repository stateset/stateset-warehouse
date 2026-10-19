import { useEffect, useMemo, useState } from "react";
import type { LotControl, WarehouseState } from "./types";
export type Availability =
  | "available"
  | "quarantined"
  | "expired"
  | "expiry_missing"
  | "invalid"
  | "unverified";
export const availabilityLabel: Record<Availability, string> = {
  available: "Available",
  quarantined: "Quarantined",
  expired: "Expired",
  expiry_missing: "Expiry label required",
  invalid: "Lot evidence needs repair",
  unverified: "Refresh expiry review",
};
/** Exact offset-bearing RFC3339 instants, at the database's microsecond precision. */
export function instant(value: unknown, precision = 6): bigint | undefined {
  if (typeof value !== "string") return undefined;
  const m =
    /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.(\d{1,9}))?(?:Z|([+-])(\d{2}):(\d{2}))$/.exec(
      value,
    );
  if (!m || (m[7]?.length ?? 0) > precision) return undefined;
  const [year, month, day, hour, minute, second] = m.slice(1, 7).map(Number);
  const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  if (
    year < 1 ||
    month < 1 ||
    month > 12 ||
    day < 1 ||
    day > days[month - 1] ||
    hour > 23 ||
    minute > 59 ||
    second > 59 ||
    Number(m[9] ?? 0) > 23 ||
    Number(m[10] ?? 0) > 59
  )
    return undefined;
  const milliseconds = Date.parse(value);
  if (
    !Number.isFinite(milliseconds) ||
    new Date(milliseconds).getUTCFullYear() < 1 ||
    new Date(milliseconds).getUTCFullYear() > 9999
  )
    return undefined;
  const fraction = (m[7] ?? "").padEnd(6, "0");
  return BigInt(milliseconds) * 1000n + BigInt(fraction.slice(3, 6));
}
const text = (s: unknown): s is string =>
  typeof s === "string" &&
  !!s &&
  s === s.trim() &&
  new TextEncoder().encode(s).length <= 240 &&
  !/[\u0000-\u001f\u007f-\u009f]/.test(s);
export function validControl(
  value: unknown,
  sku: string,
  lot: string,
): value is LotControl {
  if (!value || typeof value !== "object") return false;
  const c = value as LotControl;
  return (
    c.sku === sku &&
    c.lot === lot &&
    typeof c.expiry_recorded === "boolean" &&
    (c.expires_at === null ||
      (c.expiry_recorded && instant(c.expires_at) !== undefined)) &&
    Array.isArray(c.holds) &&
    new Set(c.holds.map((h) => h?.id)).size === c.holds.length &&
    c.holds.every(
      (h) =>
        h &&
        text(h.id) &&
        text(h.reason) &&
        text(h.actor) &&
        instant(h.quarantined_at, 9) !== undefined,
    )
  );
}
export function availability(
  state: Pick<WarehouseState, "products" | "lot_controls">,
  sku: string,
  lot: string,
  at?: bigint,
): Availability {
  const product = state.products.find((p) => p.sku === sku);
  if (
    !product ||
    (product.expiry_required !== undefined &&
      typeof product.expiry_required !== "boolean")
  )
    return "invalid";
  if (
    state.lot_controls !== undefined &&
    (!Array.isArray(state.lot_controls) || state.lot_controls.some((c) => !c))
  )
    return "invalid";
  const controls =
    state.lot_controls?.filter((c) => c.sku === sku && c.lot === lot) ?? [];
  if (controls.length > 1 || controls.some((c) => !validControl(c, sku, lot)))
    return "invalid";
  const control = controls[0];
  if (control?.holds.length) return "quarantined";
  if (control?.expires_at) {
    const expiry = instant(control.expires_at);
    if (expiry === undefined) return "invalid";
    return at === undefined
      ? "unverified"
      : at >= expiry
        ? "expired"
        : "available";
  }
  return product.expiry_required ? "expiry_missing" : "available";
}
/** Lot controls compare by identity and values, independent of JSON key order. */
export function sameControl(
  a: LotControl | null | undefined,
  b: LotControl | null | undefined,
): boolean {
  if (!a || !b) return !a && !b;
  if (!validControl(a, a.sku, a.lot) || !validControl(b, a.sku, a.lot))
    return false;
  const fields = (c: LotControl) => [
    c.sku,
    c.lot,
    c.expiry_recorded,
    c.expires_at,
    c.holds
      .map((h) => [h.id, h.reason, h.actor, h.quarantined_at])
      .sort((x, y) => x[0].localeCompare(y[0])),
  ];
  return JSON.stringify(fields(a)) === JSON.stringify(fields(b));
}
/** Use the server's clock and elapsed monotonic time, never the device's UTC clock. */
export function useAvailabilityTime(
  state: WarehouseState | null | undefined,
): bigint | undefined {
  const clock = useMemo(
    () => ({
      at: instant(state?.availability_checked_at),
      received: performance.now(),
    }),
    [state?.availability_checked_at],
  );
  const [tick, setTick] = useState(0);
  const at =
    clock.at === undefined
      ? undefined
      : clock.at +
        BigInt(Math.ceil((performance.now() - clock.received) * 1000));
  useEffect(() => {
    const update = () => setTick((n) => n + 1);
    const future = (
      Array.isArray(state?.lot_controls) ? state.lot_controls : []
    )
      .map((c) => instant(c?.expires_at))
      .filter(
        (v): v is bigint => v !== undefined && at !== undefined && v > at,
      );
    const next = future.reduce<bigint | undefined>(
      (a, b) => (a === undefined || b < a ? b : a),
      undefined,
    );
    const timer =
      next !== undefined && at !== undefined
        ? window.setTimeout(
            update,
            Math.min(2147483647, Math.max(1, Number((next - at) / 1000n) + 1)),
          )
        : undefined;
    window.addEventListener("focus", update);
    document.addEventListener("visibilitychange", update);
    return () => {
      window.clearTimeout(timer);
      window.removeEventListener("focus", update);
      document.removeEventListener("visibilitychange", update);
    };
  }, [clock, state?.lot_controls, tick]);
  return at;
}
export function localExpiry(value: string, timezone: string): string {
  if (instant(value) === undefined) return "Invalid expiry";
  try {
    return new Intl.DateTimeFormat(undefined, {
      timeZone: timezone,
      dateStyle: "medium",
      timeStyle: "long",
    }).format(new Date(value));
  } catch {
    return value;
  }
}
