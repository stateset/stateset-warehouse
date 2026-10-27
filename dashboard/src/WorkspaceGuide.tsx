import { ModalFrame } from "./CommandDialog";
import { LogOut } from "lucide-react";

/** Help dialog: the dock-to-departure flow, system ownership and safe recovery. */
export default function WorkspaceGuide({
  close,
  logout,
  suspended,
}: {
  close: () => void;
  logout: () => void;
  suspended: boolean;
}) {
  return (
    <ModalFrame
      suspended={suspended}
      title="Your warehouse workspace"
      close={close}
    >
      <div className="detail-body guide">
        <h3>From dock to departure</h3>
        <ol>
          <li>
            <strong>Receiving:</strong> schedule an expected receipt, then
            confirm actual line quantities into a receiving bin.
          </li>
          <li>
            <strong>Putaway:</strong> move received stock into a storage
            location from Inventory.
          </li>
          <li>
            <strong>Fulfillment:</strong> create or accept OMS work,
            allocate stock, follow line-specific picks, and confirm packing.
          </li>
          <li>
            <strong>Handoff:</strong> record the actual carrier pickup with
            a reference. TMS manages transportation afterward.
          </li>
          <li>
            <strong>Reconcile:</strong> review the movement ledger and
            export the independently reconstructed balances.
          </li>
        </ol>
        <h3>Ownership</h3>
        <p>
          Sync is the OMS. Warehouse is the WMS. Freight is the TMS. EDI
          owns trading-partner documents.
        </p>
        <h3>Safe recovery</h3>
        <p>
          If a save response is lost, use “Check original save.” The same
          request ID confirms or retries the original action without
          applying it twice.
        </p>
        <p className="muted-text">
          This version supports whole each units and complete-order
          shipments. Physical counts and operator validation are still
          required before live use.
        </p>
        <button className="btn" onClick={logout}>
          <LogOut size={16} />
          Sign out of workspace
        </button>
      </div>
    </ModalFrame>
  );
}
