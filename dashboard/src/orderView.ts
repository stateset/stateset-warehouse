import type { Order } from "./types";

export const activeOrder = (order: Order) =>
  !["shipped", "cancelled", "closed_partial"].includes(order.status);

export function matchesOrderStatus(order: Order, filter: string) {
  if (filter === "all") return true;
  if (filter === "pick_queue")
    return ["allocated", "picking"].includes(order.status);
  return order.status === filter;
}

export function prioritizeOrders(orders: Order[]) {
  const rank: Record<string, number> = { urgent: 0, high: 1, normal: 2 };
  return [...orders].sort(
    (a, b) =>
      (rank[a.priority] ?? 2) - (rank[b.priority] ?? 2) ||
      a.created_at.localeCompare(b.created_at) ||
      a.reference.localeCompare(b.reference),
  );
}
