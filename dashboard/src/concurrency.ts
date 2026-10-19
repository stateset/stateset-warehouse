import type { Command, WarehouseState } from "./types";

export const resourceKey = (...parts: string[]) => JSON.stringify(parts);
const stockKey = (sku: string, lot: string, location: string) =>
  resourceKey("stock", sku, lot, location);

/** Exact dependencies must match the server's command contract. */
export function reviewedVersions(
  state: WarehouseState,
  command: Command,
): Record<string, number> | undefined {
  if (!state.resource_versions) return undefined;
  const keys = new Set<string>();
  const text = (name: string) => String(command[name]);
  switch (command.type) {
    case "move_stock":
      keys.add(stockKey(text("sku"), text("lot"), text("from")));
      keys.add(stockKey(text("sku"), text("lot"), text("to")));
      break;
    case "count_stock":
      keys.add(stockKey(text("sku"), text("lot"), text("location")));
      break;
    case "receive": {
      const receipt = state.receipts.find((r) => r.id === command.receipt_id);
      const line = receipt?.lines.find((l) => l.id === command.line_id);
      if (!line) return undefined;
      keys.add(resourceKey("receipt", text("receipt_id")));
      keys.add(stockKey(line.sku, line.lot, text("location")));
      break;
    }
    case "allocate":
    case "pack":
    case "ship":
    case "cancel_order":
    case "reset_allocation":
    case "pick":
    case "unpick": {
      const order = state.orders.find((o) => o.id === command.order_id);
      if (!order) return undefined;
      keys.add(resourceKey("order", order.id));
      if (["pick", "pack", "ship"].includes(command.type)) {
        order.allocations.forEach((a) => {
          if (
            command.type !== "pick" ||
            (a.line_id === command.line_id &&
              a.location === command.location &&
              a.lot === command.lot)
          ) {
            keys.add(resourceKey("lot", a.sku, a.lot));
            keys.add(resourceKey("product", a.sku));
          }
        });
      }
      if (command.type === "allocate") {
        order.lines.forEach((l) => keys.add(resourceKey("inventory", l.sku)));
      } else if (command.type === "pick" || command.type === "unpick") {
        const a = order.allocations.find(
          (a) =>
            a.line_id === command.line_id &&
            a.location === command.location &&
            a.lot === command.lot,
        );
        if (!a) return undefined;
        keys.add(stockKey(a.sku, a.lot, a.location));
        keys.add(stockKey(a.sku, a.lot, "STAGE-01"));
      } else if (command.type !== "pack") {
        order.allocations.forEach((a) => {
          keys.add(stockKey(a.sku, a.lot, a.location));
          keys.add(stockKey(a.sku, a.lot, "STAGE-01"));
        });
      }
      break;
    }
    default:
      return undefined;
  }
  for (const k of [...keys]) {
    const parts: string[] = JSON.parse(k);
    if (parts[0] === "stock") {
      keys.add(resourceKey("product", parts[1]));
      keys.add(resourceKey("location", parts[3]));
    }
  }
  if (keys.size > 2000) return undefined;
  const versions = Object.fromEntries(
    [...keys].sort().map((k) => [k, state.resource_versions![k] ?? 0]),
  );
  // Long SKU, lot and bin labels can exceed the HTTP body limit before the
  // key-count ceiling. Keep room for the command and use the strict fence.
  if (
    new TextEncoder().encode(JSON.stringify(versions)).byteLength >
    128 * 1024
  )
    return undefined;
  return versions;
}

export function sameVersions(a: unknown, b: unknown): boolean {
  if (
    !a ||
    !b ||
    typeof a !== "object" ||
    typeof b !== "object" ||
    Array.isArray(a) ||
    Array.isArray(b)
  )
    return false;
  const left = a as Record<string, unknown>,
    right = b as Record<string, unknown>;
  return (
    Object.keys(left).length === Object.keys(right).length &&
    Object.entries(left).every(
      ([k, v]) =>
        Number.isSafeInteger(v) &&
        (v as number) >= 0 &&
        Object.hasOwn(right, k) &&
        right[k] === v,
    )
  );
}
