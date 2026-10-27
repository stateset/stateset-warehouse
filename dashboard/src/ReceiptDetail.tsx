import { ModalFrame } from "./CommandDialog";
import { Status } from "./WarehouseUi";
import { when } from "./format";
import type { Modal } from "./uiTypes";
import { ArrowRight } from "lucide-react";
import type { Receipt, WarehouseState } from "./types";

/** Receipt review: expected lines, receiving and remainder cancellation. */
export default function ReceiptDetail({
  canWork,
  close,
  disabled,
  fieldOptions,
  receipt,
  s,
  setModal,
  suspended,
}: {
  canWork: boolean;
  close: () => void;
  disabled: boolean;
  fieldOptions: (kind: string) => { value: string; label: string }[];
  receipt: Receipt;
  s: WarehouseState;
  setModal: (next: Modal) => void;
  suspended: boolean;
}) {
  return (
    <ModalFrame
      suspended={suspended}
      title={`${receipt.reference} · ${receipt.supplier}`}
      close={close}
    >
      <div className="detail-body">
        <div className="detail-summary">
          <Status value={receipt.status} />
          <span>Created {when(receipt.created_at)}</span>
        </div>
        {receipt.cancellation && (
          <p className="shipment-proof">
            Unreceived remainder cancelled ·{" "}
            {receipt.cancellation.reason}
            <br />
            {receipt.cancellation.actor} ·{" "}
            {when(receipt.cancellation.cancelled_at)}
          </p>
        )}
        <h3>Expected receipt lines</h3>
        {receipt.return_source && (
          <p className="shipment-proof">
            Customer return · original order{" "}
            {receipt.return_source.order_id}
            <br />
            {receipt.return_source.reason}
          </p>
        )}
        {receipt.lines.map((l) => (
          <div className="receipt-line" key={l.id}>
            <div>
              <strong>
                {s.products.find((p) => p.sku === l.sku)?.name}
              </strong>
              <small>
                {l.sku} · {l.lot}
              </small>
              <span>
                {l.received} received / {l.quantity} expected
              </span>
              {receipt.cancellation && (
                <span>
                  {receipt.cancellation.lines[l.id]?.cancelled ?? 0}{" "}
                  cancelled
                </span>
              )}
            </div>
            {canWork &&
              !receipt.cancellation &&
              receipt.status !== "cancelled" &&
              l.received < l.quantity && (
                <button
                  className="btn primary"
                  disabled={disabled}
                  onClick={() =>
                    setModal({
                      title: "Receive physical goods",
                      subtitle: `Verify ${l.sku}, lot ${l.lot}. Receive into a receiving bin, then move to storage. Allocation also requires an available lot.`,
                      unitSku: l.sku,
                      fields: [
                        {
                          name: "quantity",
                          label: "Received quantity (each)",
                          type: "number",
                          value: l.quantity - l.received,
                          max: l.quantity - l.received,
                        },
                        {
                          name: "location",
                          label: "Receiving location",
                          type: "select",
                          options: fieldOptions("receiving"),
                        },
                      ],
                      submit: "Confirm receipt",
                      build: (d) => ({
                        type: "receive",
                        receipt_id: receipt.id,
                        line_id: l.id,
                        quantity: Number(d.quantity),
                        location: d.location,
                      }),
                    })
                  }
                >
                  Receive goods
                </button>
              )}
          </div>
        ))}
        {canWork &&
          !receipt.cancellation &&
          ["expected", "partial"].includes(receipt.status) &&
          receipt.lines.some(
            (line) => line.received < line.quantity,
          ) && (
            <button
              className="btn danger"
              disabled={disabled}
              onClick={() =>
                setModal({
                  title: "Cancel unreceived remainder",
                  subtitle: `${receipt.reference}: close only goods that have not arrived. Received stock and its history stay intact. Cancelled return quantities become available for a new authorization.`,
                  fields: [
                    {
                      name: "reason",
                      label: "Cancellation reason",
                      value: "",
                      placeholder:
                        "Why the remaining goods will not arrive",
                    },
                  ],
                  submit: "Confirm remainder cancellation",
                  build: (d) => ({
                    type: "cancel_receipt",
                    receipt_id: receipt.id,
                    reason: d.reason,
                  }),
                })
              }
            >
              Cancel unreceived remainder
            </button>
          )}
        <div className="detail-note">
          <ArrowRight size={16} />
          <p>
            Received goods remain in the receiving location until putaway.
            Use Inventory → Move stock to transfer them into storage.
          </p>
        </div>
      </div>
    </ModalFrame>
  );
}
