import { useEffect, useRef, useState, type FormEvent } from "react";
import { Check, ScanLine, RotateCcw } from "lucide-react";
import type {
  Allocation,
  Command,
  Order,
  Receipt,
  WarehouseState,
  UnitInput,
} from "./types";
import "./ScannerPanel.css";
import UnitQuantityInput from "./UnitQuantityInput";
import { reviewedVersions, sameVersions } from "./concurrency";
import { availability, availabilityLabel } from "./lotAvailability";

type Props = {
  at: bigint | undefined;
  state: WarehouseState;
  canWork: boolean;
  disabled: boolean;
  active: boolean;
  request: (path: string, body?: unknown) => Promise<any>;
  /** Uses the shared durable command journal. True means the save was confirmed. */
  onSubmit: (
    command: Command,
    expectedVersion: number,
    versions?: Record<string, number>,
    unitInputs?: UnitInput[],
  ) => Promise<boolean>;
};
type Selection = (
  | { kind: "receive"; record: Receipt; version: number; warehouse: string }
  | { kind: "pick"; record: Order; version: number; warehouse: string }
) & { resourceVersions?: Record<string, number> };
const allocationKey = (allocation: Allocation) =>
  JSON.stringify([allocation.line_id, allocation.location, allocation.lot]);
const steps = ["location", "SKU", "lot"] as const;

/** GS1 element strings: a symbology prefix, bracketed AIs or FNC1 separators. */
export const looksLikeGs1 = (value: string) =>
  /^\](C1|d2|Q3)/.test(value) ||
  value.startsWith("(") ||
  value.includes("\u001d");

export default function ScannerPanel({
  at,
  state,
  canWork,
  disabled,
  active,
  request,
  onSubmit,
}: Props) {
  const [mode, setMode] = useState<"receive" | "pick">("receive");
  const [reference, setReference] = useState("");
  const [selection, setSelection] = useState<Selection | null>(null);
  const [lineKey, setLineKey] = useState("");
  const [receivingBin, setReceivingBin] = useState("");
  const [verified, setVerified] = useState<string[]>([]);
  const [scan, setScan] = useState("");
  const [quantity, setQuantity] = useState("1");
  const [unitInput, setUnitInput] = useState<UnitInput>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [focusSequence, setFocusSequence] = useState(0);
  const scanInput = useRef<HTMLInputElement>(null);
  const quantityInput = useRef<HTMLInputElement>(null);
  const lock = useRef(false);
  const verification = useRef<string[]>([]);
  const gs1Pending = useRef(false);
  const loadSequence = useRef(0);
  const warehouse = state.warehouse.id;
  const blocked = disabled || busy || !canWork;
  const receivingLocations = state.locations.filter(
    (bin) => bin.kind === "receiving",
  );
  const receiptLine =
    selection?.kind === "receive"
      ? selection.record.lines.find((line) => line.id === lineKey)
      : undefined;
  const allocation =
    selection?.kind === "pick"
      ? selection.record.allocations.find(
          (line) => allocationKey(line) === lineKey,
        )
      : undefined;
  const target = receiptLine
    ? {
        sku: receiptLine.sku,
        lot: receiptLine.lot,
        location: receivingBin,
        remaining: receiptLine.quantity - receiptLine.received,
        line: receiptLine.id,
      }
    : allocation
      ? {
          sku: allocation.sku,
          lot: allocation.lot,
          location: allocation.location,
          remaining:
            allocation.quantity - allocation.picked - allocation.released,
          line: allocation.line_id,
        }
      : null;
  const orderBlocked =
    selection?.kind === "pick" &&
    state.exceptions.some(
      (exception) =>
        exception.status === "open" &&
        exception.order_id === selection.record.id,
    );
  const reviewCommand: Command | null =
    selection && target
      ? selection.kind === "receive"
        ? {
            type: "receive",
            receipt_id: selection.record.id,
            line_id: target.line,
            location: target.location,
            quantity: 1,
          }
        : {
            type: "pick",
            order_id: selection.record.id,
            line_id: target.line,
            location: target.location,
            lot: target.lot,
            quantity: 1,
          }
      : null;
  const capturedVersions =
    selection && reviewCommand
      ? reviewedVersions(
          {
            ...state,
            resource_versions: selection.resourceVersions,
            ...(selection.kind === "receive"
              ? { receipts: [selection.record] }
              : { orders: [selection.record] }),
          },
          reviewCommand,
        )
      : undefined;
  const currentVersions =
    capturedVersions && state.resource_versions
      ? Object.fromEntries(
          Object.keys(capturedVersions).map((k) => [
            k,
            state.resource_versions![k] ?? 0,
          ]),
        )
      : undefined;
  const stale =
    !!selection &&
    (selection.warehouse !== warehouse ||
      (capturedVersions
        ? !sameVersions(capturedVersions, currentVersions)
        : selection.version !== state.version));
  const lotStatus = target
    ? availability(state, target.sku, target.lot, at)
    : undefined;
  const lotHeld = selection?.kind === "pick" && lotStatus !== "available";
  const readyToScan =
    !!target &&
    !!target.location &&
    target.remaining > 0 &&
    !stale &&
    !orderBlocked &&
    !lotHeld;
  const validQuantity =
    /^[1-9]\d*$/.test(quantity) &&
    Number.isSafeInteger(Number(quantity)) &&
    !!target &&
    Number(quantity) <= target.remaining;
  const complete =
    !!target &&
    verified.length === steps.length &&
    verified.every(
      (value, index) =>
        value === [target.location, target.sku, target.lot][index],
    );

  const resetScans = () => {
    verification.current = [];
    setVerified([]);
    setScan("");
    setQuantity("1");
    setUnitInput(undefined);
    setError("");
    setNotice("");
    setFocusSequence((value) => value + 1);
  };
  const clearWork = () => {
    ++loadSequence.current;
    setSelection(null);
    setLineKey("");
    setReceivingBin("");
    resetScans();
  };
  useEffect(() => {
    ++loadSequence.current;
    setSelection(null);
    setLineKey("");
    setReference("");
    setReceivingBin("");
    verification.current = [];
    setVerified([]);
    setScan("");
    setError("");
    setNotice("");
  }, [warehouse]);
  useEffect(
    () => () => {
      ++loadSequence.current;
    },
    [],
  );
  useEffect(() => {
    if (!active || blocked || !readyToScan) return;
    if (complete) quantityInput.current?.focus();
    else scanInput.current?.focus();
  }, [active, blocked, readyToScan, complete, verified.length, focusSequence]);

  const loadWork = async (event?: FormEvent) => {
    event?.preventDefault();
    if (lock.current || blocked || !reference) return;
    const candidate =
      mode === "receive"
        ? state.receipts.find((receipt) => receipt.reference === reference)
        : state.orders.find((order) => order.reference === reference);
    if (!candidate) {
      setError(
        "Work reference must exactly match a receipt or order in the current work list.",
      );
      return;
    }
    const generation = ++loadSequence.current;
    const version = state.version;
    lock.current = true;
    setBusy(true);
    setSelection(null);
    resetScans();
    try {
      const record = await request(
        `/v1/warehouses/${encodeURIComponent(warehouse)}/${mode === "receive" ? "receipts" : "orders"}/${encodeURIComponent(candidate.id)}`,
      );
      if (generation !== loadSequence.current) return;
      if (
        record.id !== candidate.id ||
        record.reference !== reference ||
        !Array.isArray(record.lines)
      )
        throw new Error(
          "Canonical work identity did not match. Reload the warehouse before scanning.",
        );
      if (mode === "receive") {
        const receipt = record as Receipt;
        if (receipt.cancellation || receipt.status === "cancelled")
          throw new Error(
            "This receipt remainder was cancelled. Select open receiving work.",
          );
        const line = receipt.lines.find(
          (item) => item.received < item.quantity,
        );
        if (!line)
          throw new Error("This receipt has no remaining quantity to receive.");
        setSelection({
          kind: "receive",
          record: receipt,
          version,
          warehouse,
          resourceVersions: state.resource_versions,
        });
        setLineKey(line.id);
        setReceivingBin(
          receivingLocations.length === 1 ? receivingLocations[0].code : "",
        );
      } else {
        const order = record as Order;
        if (
          !["allocated", "picking", "partially_shipped"].includes(order.status)
        )
          throw new Error(
            "This order must have an active allocation before scanning picks.",
          );
        const line = order.allocations.find(
          (item) => item.picked + item.released < item.quantity,
        );
        if (!line)
          throw new Error("This order has no remaining allocation to pick.");
        setSelection({
          kind: "pick",
          record: order,
          version,
          warehouse,
          resourceVersions: state.resource_versions,
        });
        setLineKey(allocationKey(line));
      }
    } catch (failure) {
      if (generation === loadSequence.current)
        setError((failure as Error).message);
    } finally {
      lock.current = false;
      setBusy(false);
    }
  };

  const clearScan = () => {
    setScan("");
    if (scanInput.current) scanInput.current.value = "";
  };
  // A GS1 label at the product step identifies the product only through
  // its reviewed GTIN, and can verify the lot from the same scan.
  const verifyGs1 = async (value: string) => {
    if (!target || gs1Pending.current) return;
    gs1Pending.current = true;
    try {
      const parsed = await request(
        `/v1/warehouses/${encodeURIComponent(state.warehouse.id)}/scans/parse`,
        { data: value },
      );
      clearScan();
      if (parsed.sku !== target.sku) {
        setError(
          parsed.sku
            ? `GS1 label is ${parsed.sku}. Expected ${target.sku}. Nothing was saved.`
            : "This GS1 label's GTIN is not reviewed for any product here. Scan the SKU barcode or ask an administrator to add the GTIN.",
        );
        setFocusSequence((n) => n + 1);
        return;
      }
      const lotMatches =
        typeof parsed.lot === "string" && parsed.lot === target.lot;
      verification.current = lotMatches
        ? [...verification.current, target.sku, target.lot]
        : [...verification.current, target.sku];
      setVerified(verification.current);
      setError(
        typeof parsed.lot === "string" && !lotMatches
          ? `GS1 label lot is ${parsed.lot}. Expected ${target.lot}. Scan the lot.`
          : "",
      );
    } catch (failure) {
      clearScan();
      setError(`GS1 label could not be read: ${(failure as Error).message}`);
    } finally {
      gs1Pending.current = false;
    }
  };

  const verifyScan = (event: FormEvent) => {
    event.preventDefault();
    if (blocked || !readyToScan || !target || complete || gs1Pending.current)
      return;
    const index = verification.current.length;
    const value = scanInput.current?.value ?? scan;
    if (!value || index >= steps.length) return;
    if (index === 1 && looksLikeGs1(value)) {
      void verifyGs1(value);
      return;
    }
    const expected = [target.location, target.sku, target.lot][index];
    if (value !== expected) {
      setError(
        `Wrong ${steps[index]}. Expected ${expected}. Nothing was saved.`,
      );
      setScan("");
      if (scanInput.current) scanInput.current.value = "";
      setFocusSequence((value) => value + 1);
      return;
    }
    verification.current = [...verification.current, value];
    setVerified(verification.current);
    setScan("");
    if (scanInput.current) scanInput.current.value = "";
    setError("");
  };

  const confirm = async () => {
    if (
      lock.current ||
      blocked ||
      !readyToScan ||
      !target ||
      !selection ||
      !complete ||
      !validQuantity
    )
      return;
    lock.current = true;
    setBusy(true);
    setError("");
    const command: Command =
      selection.kind === "receive"
        ? {
            type: "receive",
            receipt_id: selection.record.id,
            line_id: target.line,
            location: target.location,
            quantity: Number(quantity),
          }
        : {
            type: "pick",
            order_id: selection.record.id,
            line_id: target.line,
            location: target.location,
            lot: target.lot,
            quantity: Number(quantity),
          };
    try {
      const confirmed = await onSubmit(
        command,
        selection.version,
        capturedVersions,
        unitInput ? [unitInput] : undefined,
      );
      if (confirmed) {
        setSelection(null);
        setLineKey("");
        verification.current = [];
        setVerified([]);
        setNotice(
          "Save confirmed. Reload work and scan each identity again before the next action.",
        );
      } else {
        setError(
          "Save was not confirmed. Review the save status and original request recovery before continuing.",
        );
      }
    } catch (failure) {
      setError(
        (failure as Error).message ||
          "Save was not confirmed. Review original request recovery.",
      );
    } finally {
      lock.current = false;
      setBusy(false);
    }
  };

  return (
    <section className="scanner-panel" aria-labelledby="scanner-heading">
      <div className="scanner-heading">
        <ScanLine aria-hidden="true" size={28} />
        <div>
          <h2 id="scanner-heading">Scanner workbench</h2>
          <p>
            Scan the exact bin, SKU, and lot labels. Review the quantity, then
            confirm the physical action.
          </p>
        </div>
      </div>
      <p className="scanner-identity">
        {state.warehouse.code} · Warehouse version {state.version} · Whole units
        (each)
      </p>
      <div
        className="scanner-modes"
        role="group"
        aria-label="Scanner operation"
      >
        <button
          type="button"
          aria-pressed={mode === "receive"}
          disabled={blocked}
          onClick={() => {
            setMode("receive");
            setReference("");
            clearWork();
          }}
        >
          Receive with scanner
        </button>
        <button
          type="button"
          aria-pressed={mode === "pick"}
          disabled={blocked}
          onClick={() => {
            setMode("pick");
            setReference("");
            clearWork();
          }}
        >
          Pick with scanner
        </button>
      </div>
      {!canWork && (
        <p role="status">
          An operator or administrator credential is required to perform scanner
          work.
        </p>
      )}
      <form className="scanner-work-selection" onSubmit={loadWork}>
        <label htmlFor="scanner-reference">
          {mode === "receive" ? "Receipt" : "Order"} reference
          <input
            id="scanner-reference"
            value={reference}
            list="scanner-work-list"
            autoComplete="off"
            autoCapitalize="off"
            spellCheck={false}
            disabled={blocked}
            onChange={(event) => {
              setReference(event.target.value);
              clearWork();
            }}
            placeholder="Scan or choose the exact work reference"
          />
        </label>
        <datalist id="scanner-work-list">
          {(mode === "receive"
            ? state.receipts.filter(
                (receipt) =>
                  ["expected", "partial"].includes(receipt.status) &&
                  !receipt.cancellation &&
                  receipt.lines.some((line) => line.received < line.quantity),
              )
            : state.orders.filter((order) =>
                ["allocated", "picking", "partially_shipped"].includes(
                  order.status,
                ),
              )
          ).map((record) => (
            <option key={record.id} value={record.reference} />
          ))}
        </datalist>
        <button type="submit" disabled={blocked || !reference}>
          {busy ? "Working…" : selection ? "Reload work" : "Load work"}
        </button>
      </form>
      {selection && (
        <div className="scanner-work">
          <div className="scanner-record">
            <strong>{selection.record.reference}</strong>
            <span>Record {selection.record.id}</span>
            <span>Reviewed at warehouse version {selection.version}</span>
          </div>
          {stale && (
            <p className="scanner-error" role="alert">
              Warehouse changed after this work was selected. Reload work and
              verify all labels again.
            </p>
          )}
          {orderBlocked && (
            <p className="scanner-error" role="alert">
              An open exception blocks this order. Resolve the exception before
              picking.
            </p>
          )}
          <label htmlFor="scanner-line">
            {selection.kind === "receive"
              ? "Receipt line"
              : "Reserved allocation"}
            <select
              id="scanner-line"
              value={lineKey}
              disabled={blocked || stale}
              onChange={(event) => {
                setLineKey(event.target.value);
                resetScans();
              }}
            >
              {selection.kind === "receive"
                ? selection.record.lines
                    .filter((line) => line.received < line.quantity)
                    .map((line) => (
                      <option key={line.id} value={line.id}>
                        {line.sku} · {line.lot} ·{" "}
                        {line.quantity - line.received} remaining · line{" "}
                        {line.id}
                      </option>
                    ))
                : selection.record.allocations
                    .filter(
                      (line) => line.picked + line.released < line.quantity,
                    )
                    .map((line) => (
                      <option
                        key={allocationKey(line)}
                        value={allocationKey(line)}
                      >
                        {line.sku} · {line.location} · {line.lot} ·{" "}
                        {line.quantity - line.picked - line.released} remaining
                        · line {line.line_id}
                      </option>
                    ))}
            </select>
          </label>
          {selection.kind === "receive" && (
            <label htmlFor="scanner-receiving-bin">
              Receiving bin
              <select
                id="scanner-receiving-bin"
                value={receivingBin}
                disabled={blocked || stale}
                onChange={(event) => {
                  setReceivingBin(event.target.value);
                  resetScans();
                }}
              >
                <option value="">Choose receiving bin</option>
                {receivingLocations.map((bin) => (
                  <option key={bin.code} value={bin.code}>
                    {bin.code} · {bin.zone}
                  </option>
                ))}
              </select>
            </label>
          )}
          {target && (
            <>
              <dl className="scanner-target">
                <div>
                  <dt>Bin</dt>
                  <dd>{target.location || "Choose receiving bin"}</dd>
                </div>
                <div>
                  <dt>SKU</dt>
                  <dd>{target.sku}</dd>
                </div>
                <div>
                  <dt>Lot</dt>
                  <dd>{target.lot}</dd>
                </div>
                <div>
                  <dt>Remaining</dt>
                  <dd>{target.remaining} each</dd>
                </div>
              </dl>
              {lotStatus && (
                <p role={lotHeld ? "alert" : "status"}>
                  Lot: {availabilityLabel[lotStatus]}.
                  {lotHeld
                    ? " Picking is held. Review Inventory → Lot expiry and quarantine; existing reservations are retained."
                    : selection.kind === "receive" && lotStatus !== "available"
                      ? " Receiving actual custody is still allowed; the lot stays unavailable for fulfillment."
                      : " Expiry is checked again when saving."}
                </p>
              )}
              <ol className="scanner-steps" aria-label="Label verification">
                {steps.map((step, index) => (
                  <li
                    key={step}
                    className={
                      verified.length > index
                        ? "verified"
                        : verified.length === index
                          ? "current"
                          : ""
                    }
                  >
                    {verified.length > index ? (
                      <Check aria-hidden="true" size={18} />
                    ) : (
                      <span>{index + 1}</span>
                    )}
                    {step === "location"
                      ? "Bin"
                      : step === "SKU"
                        ? "SKU"
                        : "Lot"}
                    <span className="scanner-step-state">
                      {verified.length > index ? "Verified" : "Awaiting scan"}
                    </span>
                  </li>
                ))}
              </ol>
              {!complete && (
                <form onSubmit={verifyScan} className="scanner-label-form">
                  <label htmlFor="scanner-label">
                    Scan{" "}
                    {steps[verified.length] === "location"
                      ? "bin"
                      : steps[verified.length]}
                    <input
                      id="scanner-label"
                      ref={scanInput}
                      value={scan}
                      onChange={(event) => setScan(event.target.value)}
                      disabled={blocked || !readyToScan}
                      autoComplete="off"
                      autoCapitalize="off"
                      spellCheck={false}
                      aria-describedby="scanner-input-help"
                    />
                  </label>
                  <p id="scanner-input-help">
                    Keyboard scanner: scan one label and send Enter. Matching is
                    case sensitive.
                  </p>
                  <button
                    type="submit"
                    disabled={blocked || !readyToScan || !scan}
                  >
                    Verify label
                  </button>
                </form>
              )}
              {complete && (
                <div className="scanner-confirm">
                  <p className="scanner-verified" role="status">
                    All three labels verified. No inventory has been saved yet.
                  </p>
                  <UnitQuantityInput
                    key={JSON.stringify([
                      selection.record.id,
                      lineKey,
                      target.sku,
                      target.location,
                      target.lot,
                    ])}
                    packs={state.unit_packs}
                    sku={target.sku}
                    target={[]}
                    value={quantity}
                    onChange={(each, input) => {
                      setQuantity(each);
                      setUnitInput(input);
                    }}
                    label="Physical quantity (each)"
                    max={target.remaining}
                    inputRef={quantityInput}
                    disabled={blocked || stale || !!orderBlocked || lotHeld}
                  />
                  {!validQuantity && (
                    <p className="scanner-error">
                      Enter a whole quantity from 1 to {target.remaining}.
                    </p>
                  )}
                  <p>
                    Confirm only after{" "}
                    {selection.kind === "receive"
                      ? "placing these units in the receiving bin"
                      : "moving these units from the verified bin to staging"}
                    .
                  </p>
                  <button
                    type="button"
                    className="scanner-save"
                    disabled={blocked || !readyToScan || !validQuantity}
                    onClick={() => void confirm()}
                  >
                    {busy
                      ? "Saving…"
                      : selection.kind === "receive"
                        ? "Confirm scanned receipt"
                        : "Confirm scanned pick"}
                  </button>
                </div>
              )}
              <button
                type="button"
                className="scanner-reset"
                disabled={blocked || stale}
                onClick={resetScans}
              >
                <RotateCcw aria-hidden="true" size={16} /> Scan labels again
              </button>
            </>
          )}
        </div>
      )}
      {error && (
        <p className="scanner-error" role="alert">
          {error}
        </p>
      )}
      {notice && (
        <p className="scanner-verified" role="status">
          {notice}
        </p>
      )}
      <p className="scanner-footnote">
        SKU, bin, and lot labels use their warehouse identifiers. Supplier GTIN
        and GS1 aliases require an explicit catalog mapping before use.
      </p>
    </section>
  );
}
