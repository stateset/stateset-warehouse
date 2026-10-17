export type Product = {
  sku: string;
  name: string;
  category: string;
  reorder_point: number;
};
export type Location = { code: string; zone: string; kind: string };
export type Stock = {
  sku: string;
  lot: string;
  location: string;
  on_hand: number;
  reserved: number;
};
export type Receipt = {
  id: string;
  reference: string;
  supplier: string;
  status: string;
  lines: {
    id: string;
    sku: string;
    lot: string;
    quantity: number;
    received: number;
  }[];
  created_at: string;
};
export type Allocation = {
  line_id: string;
  sku: string;
  lot: string;
  location: string;
  quantity: number;
  picked: number;
  shipped: number;
  released: number;
};
export type Order = {
  id: string;
  reference: string;
  source_order_id: string | null;
  customer: string;
  destination: string;
  priority: string;
  status: string;
  lines: { id: string; sku: string; quantity: number }[];
  allocations: Allocation[];
  carrier: string;
  handoff_reference: string;
  created_at: string;
  shipped_at: string | null;
};
export type Movement = {
  id: string;
  kind: string;
  sku: string;
  lot: string;
  from: string | null;
  to: string | null;
  quantity: number;
  reference: string;
  reason: string;
  actor: string;
  at: string;
};
export type Exception = {
  id: string;
  order_id: string | null;
  category: string;
  detail: string;
  status: string;
  resolution: string;
  actor: string;
  resolved_by: string | null;
  created_at: string;
};
export type Activity = {
  id: string;
  kind: string;
  reference: string;
  detail: string;
  actor: string;
  at: string;
};
export type WarehouseState = {
  warehouse: {
    id: string;
    code: string;
    name: string;
    city: string;
    timezone: string;
  };
  version: number;
  products: Product[];
  locations: Location[];
  stocks: Stock[];
  receipts: Receipt[];
  orders: Order[];
  movements: Movement[];
  exceptions: Exception[];
  activity: Activity[];
};
export type Session = {
  identity: { tenant: string; actor: string; role: string };
  demo: boolean;
  integrations: {
    id: string;
    name: string;
    role: string;
    configured: boolean;
  }[];
};
export type Outbox = {
  id: string;
  destination: string;
  status: string;
  attempts: number;
  last_error: string;
  event_id: string;
  type: string;
  occurred_at: string;
};
export type Command = { type: string; [key: string]: unknown };
export type Journal = {
  tenant: string;
  actor: string;
  warehouse: string;
  request: { request_id: string; expected_version: number; command: Command };
};
