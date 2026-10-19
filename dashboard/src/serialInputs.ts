import type { SerialInput } from "./types";
export type SerialTarget = {
  target: string[];
  sku: string;
  each_quantity: number;
};
export const serialCommands = new Set([
  "count_stock",
  "move_stock",
  "receive",
  "pick",
  "unpick",
  "create_return",
  "pack_shipment",
  "dispatch_transfer",
  "receive_transfer",
  "record_transfer_loss",
  "reverse_transfer_loss",
]);
export const serialText = (v: unknown): v is string =>
  typeof v === "string" &&
  !!v &&
  v.trim() === v &&
  new TextEncoder().encode(v).length <= 240 &&
  !/[\x00-\x1f\x7f-\x9f]/.test(v);
export function validSerialInputs(
  value: unknown,
): value is SerialInput[] | undefined {
  if (value === undefined) return true;
  if (!Array.isArray(value) || value.length > 100) return false;
  const targets = new Set<string>(),
    identities = new Set<string>();
  let count = 0;
  return value.every((i) => {
    if (
      !i ||
      Object.keys(i).sort().join() !== "serials,sku,target" ||
      !serialText(i.sku) ||
      !Array.isArray(i.target) ||
      i.target.length > 3 ||
      !i.target.every(serialText) ||
      !Array.isArray(i.serials) ||
      !i.serials.every(serialText)
    )
      return false;
    count += i.serials.length;
    const key = JSON.stringify(i.target);
    if (count > 10000 || targets.has(key)) return false;
    targets.add(key);
    return i.serials.every((s: string) => {
      const key = JSON.stringify([i.sku, s]);
      if (identities.has(key)) return false;
      identities.add(key);
      return true;
    });
  });
}
export function validSerialTargets(value: unknown): value is SerialTarget[] {
  if (!Array.isArray(value) || value.length > 100) return false;
  const targets = new Set<string>();
  let count = 0;
  return value.every((i) => {
    if (
      !i ||
      Object.keys(i).sort().join() !== "each_quantity,sku,target" ||
      !serialText(i.sku) ||
      !Array.isArray(i.target) ||
      i.target.length > 3 ||
      !i.target.every(serialText) ||
      !Number.isSafeInteger(i.each_quantity) ||
      i.each_quantity < 0
    )
      return false;
    count += i.each_quantity;
    const key = JSON.stringify(i.target);
    if (count > 10000 || targets.has(key)) return false;
    targets.add(key);
    return true;
  });
}
