export type Product = {
  serial_controlled?: boolean;
  expiry_required?: boolean;
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
  return_source?: {
    order_id: string;
    reason: string;
    line_ids: Record<string, string>;
  };
  cancellation?: {
    reason: string;
    actor: string;
    cancelled_at: string;
    lines: Record<
      string,
      {
        sku: string;
        lot: string;
        quantity: number;
        received: number;
        cancelled: number;
      }
    >;
  };
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
  remainder_cancellation?: {
    reason: string;
    actor: string;
    cancelled_at: string;
    lines: Record<
      string,
      { sku: string; ordered: number; shipped: number; cancelled: number }
    >;
  };
  shipment_mode?: "packages";
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
  receipt_id?: string | null;
  receipt_line_id?: string | null;
  transfer_id?: string | null;
  transfer_line_id?: string | null;
  transfer_event_id?: string | null;
  package_id?: string | null;
  order_line_id?: string | null;
  allocation_location?: string | null;
  shipment_event_id?: string | null;
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
export type LotControl = {
  sku: string;
  lot: string;
  expiry_recorded: boolean;
  expires_at: string | null;
  holds: {
    id: string;
    reason: string;
    actor: string;
    quarantined_at: string;
  }[];
};
export type UnitPack = {
  id: string;
  sku: string;
  code: string;
  label: string;
  each_per_pack: number;
};
export type UnitInput = {
  target: string[];
  sku: string;
  pack_code: string;
  definition_id: string;
  pack_quantity: number;
  each_per_pack: number;
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
  resource_versions?: Record<string, number>;
  availability_checked_at?: string;
  lot_controls?: LotControl[];
  unit_packs?: UnitPack[];
  products: Product[];
  locations: Location[];
  stocks: Stock[];
  receipts: Receipt[];
  orders: Order[];
  movements: Movement[];
  exceptions: Exception[];
  activity: Activity[];
  inventory_baseline?: Stock[];
  receipt_baseline?: Record<string, Record<string, number>>;
};
export type HistoryData = {
  orders: Order;
  receipts: Receipt;
  movements: Movement;
  activity: Activity;
  exceptions: Exception;
};
export type HistoryKind = keyof HistoryData;
export type HistoryPage<T> = {
  items: { cursor: number; data: T }[];
  next_cursor: number | null;
};
export type Session = {
  identity: { tenant: string; actor: string; role: string };
  demo: boolean;
  default_warehouse_id?: string;
  native_integrations?: {
    oms: { configured: boolean; shipment_writeback?: string };
    tms: { configured: boolean };
  };
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
export type SerialInput = { target: string[]; sku: string; serials: string[] };
export type Journal = {
  tenant: string;
  actor: string;
  warehouse: string;
  request: {
    request_id: string;
    expected_version: number;
    command: Command;
    resource_versions?: Record<string, number>;
    unit_inputs?: UnitInput[];
    serial_inputs?: SerialInput[];
  };
};
