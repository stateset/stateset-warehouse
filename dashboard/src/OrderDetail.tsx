import { ModalFrame } from "./CommandDialog";
import { Status } from "./WarehouseUi";
import ReturnForm from "./ReturnForm";
import ShipmentsPanel from "./ShipmentsPanel";
import { availability, availabilityLabel } from "./lotAvailability";
import { when } from "./format";
import type { Modal, RequestJson, SubmitCommand } from "./uiTypes";
import {
  AlertTriangle,
  ClipboardCheck,
  Package,
  PackageCheck,
  Truck,
} from "lucide-react";
import type { Allocation, Command, Order, WarehouseState } from "./types";

/** Order review: lines, pick instructions, packages, handoff and recovery. */
export default function OrderDetail({
  at,
  canWork,
  close,
  disabled,
  error,
  order,
  refresh,
  report,
  request,
  s,
  setModal,
  submit,
  suspended,
}: {
  at: bigint | undefined;
  canWork: boolean;
  close: () => void;
  disabled: boolean;
  error: string;
  order: Order;
  refresh: (warehouse: string) => Promise<WarehouseState | undefined>;
  report: (order?: Order) => void;
  request: RequestJson;
  s: WarehouseState;
  setModal: (next: Modal) => void;
  submit: SubmitCommand;
  suspended: boolean;
}) {
  const orderBlocked = s.exceptions.some(
    (issue) => issue.order_id === order.id && issue.status === "open",
  );
  const legacyLotHeld = order.allocations.some(
    (a) =>
      a.quantity > a.shipped + a.released &&
      availability(s, a.sku, a.lot, at) !== "available",
  );
  const action = (command: Command) => {
    void submit(command);
  };
  return (
    <ModalFrame
      suspended={suspended}
      title={`${order.reference} · ${order.customer}`}
      close={close}
    >
      <div className="detail-body">
        <div className="detail-summary">
          <Status value={order.status} />
          <span className={`priority ${order.priority}`}>
            <i />
            {order.priority}
          </span>
          <span>{order.destination}</span>
        </div>
        <div className="detail-reference">
          <span>OMS source</span>
          <code>
            {order.source_order_id ?? "Manual warehouse work"}
          </code>
        </div>
        <h3>Order lines</h3>
        {order.lines.map((l) => (
          <div className="detail-line" key={l.id}>
            <span className="product-thumb">
              <Package size={20} />
            </span>
            <div>
              <strong>
                {s.products.find((p) => p.sku === l.sku)?.name}
              </strong>
              <small>
                {l.sku} · line {l.id.slice(0, 8)}
              </small>
            </div>
            <strong>
              {l.quantity} <small>each</small>
            </strong>
          </div>
        ))}
        {order.allocations.length > 0 && (
          <>
            <h3>Reserved pick instructions</h3>
            {order.allocations.map((a: Allocation, i: number) => (
              <div className="allocation" key={i}>
                <div>
                  <strong>{a.sku}</strong>
                  <small>
                    {a.location} · {a.lot} · line {a.line_id.slice(0, 8)}
                  </small>
                  <span>
                    {a.quantity > a.shipped + a.released
                      ? availabilityLabel[
                          availability(s, a.sku, a.lot, at)
                        ]
                      : "No active reserved units"}
                  </span>
                  <span>
                    {a.picked} picked / {a.quantity} reserved
                    {a.released > 0 && ` · ${a.released} released`}
                  </span>
                </div>
                {canWork &&
                  ["allocated", "picking", "partially_shipped"].includes(
                    order.status,
                  ) &&
                  a.quantity > a.picked + a.released && (
                    <button
                      className="btn"
                      disabled={
                        disabled ||
                        orderBlocked ||
                        availability(s, a.sku, a.lot, at) !==
                          "available"
                      }
                      onClick={() =>
                        setModal({
                          title: "Confirm physical pick",
                          subtitle: `Verify ${a.sku} at ${a.location}, lot ${a.lot}. The units move into STAGE-01 and remain reserved.`,
                          unitSku: a.sku,
                          fields: [
                            {
                              name: "quantity",
                              label: "Picked quantity (each)",
                              type: "number",
                              value: a.quantity - a.picked - a.released,
                              max: a.quantity - a.picked - a.released,
                            },
                          ],
                          submit: "Confirm pick",
                          build: (d) => ({
                            type: "pick",
                            order_id: order.id,
                            line_id: a.line_id,
                            location: a.location,
                            lot: a.lot,
                            quantity: Number(d.quantity),
                          }),
                        })
                      }
                    >
                      Pick units
                    </button>
                  )}
                {canWork &&
                  [
                    "picking",
                    "picked",
                    "packed",
                    "partially_shipped",
                  ].includes(order.status) &&
                  a.picked > a.shipped && (
                    <button
                      className="btn"
                      disabled={disabled}
                      onClick={() =>
                        setModal({
                          title: "Return staged stock",
                          subtitle: `Physically return ${a.sku}, lot ${a.lot}, from STAGE-01 to ${a.location}. Only line ${a.line_id} is affected. Packing must be confirmed again after recovery.`,
                          unitSku: a.sku,
                          fields: [
                            {
                              name: "quantity",
                              label: "Returned quantity (each)",
                              type: "number",
                              value: a.picked - a.shipped,
                              max: a.picked - a.shipped,
                            },
                            {
                              name: "reason",
                              label: "Return evidence / reason",
                              type: "textarea",
                            },
                          ],
                          submit: "Confirm physical return",
                          build: (data) => ({
                            type: "unpick",
                            order_id: order.id,
                            line_id: a.line_id,
                            location: a.location,
                            lot: a.lot,
                            quantity: Number(data.quantity),
                            reason: data.reason,
                          }),
                        })
                      }
                    >
                      Return staged units
                    </button>
                  )}
              </div>
            ))}
          </>
        )}
        {s.exceptions
          .filter(
            (e) => e.order_id === order.id && e.status === "open",
          )
          .map((e) => (
            <div className="alert warning" key={e.id}>
              <AlertTriangle size={17} />
              <p>
                <strong>{e.category}</strong>
                <br />
                {e.detail}
              </p>
            </div>
          ))}
        {order.status === "shipped" && (
          <div className="shipment-proof">
            <Truck size={22} />
            <div>
              <strong>Physical handoff recorded</strong>
              <p>
                {order.carrier
                  ? `${order.carrier} · ${order.handoff_reference}`
                  : "Multiple handoffs — review each shipment package below"}
              </p>
              <small>
                {order.shipped_at && when(order.shipped_at)}
              </small>
            </div>
          </div>
        )}
        {["shipped", "partially_shipped", "closed_partial"].includes(
          order.status,
        ) &&
          canWork && (
            <ReturnForm
              packs={s.unit_packs}
              warehouse={s.warehouse.id}
              orderId={order.id}
              disabled={disabled}
              request={request}
              onSubmit={submit}
            />
          )}
        {(order.shipment_mode === "packages" ||
          !["shipped", "cancelled", "closed_partial"].includes(
            order.status,
          )) && (
          <ShipmentsPanel
            warehouse={s.warehouse.id}
            orderId={order.id}
            disabled={disabled}
            forwardBlocked={orderBlocked}
            state={s}
            at={at}
            canWork={canWork}
            request={request}
            refreshWarehouse={refresh}
            onSubmit={submit}
          />
        )}
        {legacyLotHeld && (
          <p role="status" className="alert warning">
            One or more reserved lots are unavailable. Review Inventory →
            Lot expiry and quarantine. Physical unpick, unpack and
            reservation release remain available.
          </p>
        )}
        <div className="detail-actions">
          {canWork && (
            <>
              {!order.shipment_mode &&
                order.status === "pending" && (
                  <button
                    className="btn primary"
                    disabled={disabled || orderBlocked}
                    onClick={() =>
                      action({
                        type: "allocate",
                        order_id: order.id,
                      })
                    }
                  >
                    <ClipboardCheck size={16} />
                    Allocate stock
                  </button>
                )}
              {!order.shipment_mode &&
                order.status === "allocated" &&
                order.allocations.every(
                  (allocation) => allocation.picked === 0,
                ) && (
                  <button
                    className="btn"
                    disabled={disabled}
                    onClick={() =>
                      setModal({
                        title: "Release allocation for recovery",
                        subtitle:
                          "All staged stock has been returned. Release this order's reservations so a supervisor can recount the physical stock, then allocate the order again after the issue is resolved.",
                        fields: [
                          {
                            name: "reason",
                            label: "Allocation release reason",
                            type: "textarea",
                          },
                        ],
                        submit: "Release reservations",
                        build: (data) => ({
                          type: "reset_allocation",
                          order_id: order.id,
                          reason: data.reason,
                        }),
                      })
                    }
                  >
                    Release allocation
                  </button>
                )}
              {!order.shipment_mode &&
                order.status === "picked" && (
                  <button
                    className="btn primary"
                    disabled={disabled || orderBlocked || legacyLotHeld}
                    onClick={() =>
                      action({ type: "pack", order_id: order.id })
                    }
                  >
                    <PackageCheck size={16} />
                    Confirm packed
                  </button>
                )}
              {!order.shipment_mode &&
                order.status === "packed" && (
                  <button
                    className="btn primary"
                    disabled={disabled || orderBlocked || legacyLotHeld}
                    onClick={() =>
                      setModal({
                        title: "Confirm carrier handoff",
                        subtitle:
                          "Record the actual physical departure. This removes staged units from warehouse stock. TMS owns transportation after this handoff.",
                        fields: [
                          { name: "carrier", label: "Carrier" },
                          {
                            name: "handoff_reference",
                            label: "Carrier pickup / tracking reference",
                          },
                        ],
                        submit: "Confirm physical handoff",
                        build: (d) => ({
                          type: "ship",
                          order_id: order.id,
                          ...d,
                        }),
                      })
                    }
                  >
                    <Truck size={16} />
                    Record handoff
                  </button>
                )}
              {!["shipped", "cancelled", "closed_partial"].includes(
                order.status,
              ) && (
                <button
                  className="btn"
                  disabled={disabled}
                  onClick={() => report(order)}
                >
                  Report exception
                </button>
              )}
              {["pending", "allocated"].includes(order.status) && (
                <button
                  className="text-btn danger"
                  disabled={disabled}
                  onClick={() =>
                    setModal({
                      title: "Cancel warehouse fulfillment",
                      subtitle:
                        "Releases all reservations for this order. Available only before physical picking begins.",
                      fields: [
                        {
                          name: "reason",
                          label: "Cancellation reason",
                          type: "textarea",
                        },
                      ],
                      submit: "Cancel fulfillment",
                      build: (d) => ({
                        type: "cancel_order",
                        order_id: order.id,
                        ...d,
                      }),
                    })
                  }
                >
                  Cancel fulfillment
                </button>
              )}
            </>
          )}
        </div>
      </div>
      {error && (
        <div role="alert" className="modal-error">
          {error}
        </div>
      )}
    </ModalFrame>
  );
}
