import { CommandForm, ModalFrame, type FormSpec } from "./CommandDialog";
import { ApiError, provesStaleCommandDidNotCommit } from "./commandRecovery";
import { Badge, Empty, Metric, Section, Status, label } from "./WarehouseUi";
import type { WarehouseAuth } from "./Auth";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type FormEvent,
} from "react";
import NativePanel from "./NativePanel";
import ScannerPanel from "./ScannerPanel";
import OperationsPanel from "./OperationsPanel";
import ReplenishmentPanel from "./ReplenishmentPanel";
import LotControlsPanel from "./LotControlsPanel";
import LotTracePanel from "./LotTracePanel";
import {
  availability,
  availabilityLabel,
  useAvailabilityTime,
} from "./lotAvailability";
import ReturnForm from "./ReturnForm";
import ShipmentsPanel from "./ShipmentsPanel";
import TransfersPanel from "./TransfersPanel";
import WarehousePicker from "./WarehousePicker";
import { reviewedVersions } from "./concurrency";
import {
  Activity as ActivityIcon,
  ArrowDownLeft,
  ArrowDownToLine,
  ArrowRight,
  ArrowUpRight,
  Box,
  Boxes,
  CheckCheck,
  ChevronDown,
  ChevronRight,
  CircleHelp,
  ClipboardCheck,
  Clock3,
  Download,
  ExternalLink,
  Filter,
  LayoutDashboard,
  Link2,
  Loader2,
  LogOut,
  MapPin,
  MoreHorizontal,
  Package,
  PackageCheck,
  Plus,
  RefreshCw,
  Search,
  ScanLine,
  Settings2,
  ShieldCheck,
  Truck,
  Warehouse,
  X,
  AlertTriangle,
} from "lucide-react";
import type {
  Allocation,
  Activity,
  Command,
  Journal,
  HistoryData,
  HistoryKind,
  HistoryPage,
  Order,
  Outbox,
  Receipt,
  Session,
  Stock,
  WarehouseState,
  UnitInput,
} from "./types";

import UnitPacksPanel from "./UnitPacksPanel";
import { validUnitInputs } from "./unitInputs";
import {
  serialCommands,
  validSerialInputs,
  validSerialTargets,
  type SerialTarget,
} from "./serialInputs";
import SerialObservationForm from "./SerialObservationForm";
import SerialTracePanel from "./SerialTracePanel";
import type { SerialInput } from "./types";

type Tab =
  | "overview"
  | "inventory"
  | "receiving"
  | "fulfillment"
  | "locations"
  | "exceptions"
  | "activity"
  | "integrations"
  | "scanner"
  | "operations"
  | "transfers";
type Modal =
  | FormSpec
  | { order: string }
  | { receipt: string }
  | { help: true }
  | null;
type HistoryState = {
  [K in HistoryKind]?: {
    items: HistoryData[K][];
    nextCursor: number | null;
    loading: boolean;
  };
};
type ReconciliationProof = {
  ok: boolean;
  rows: Record<string, unknown>[];
  receipts?: Record<string, unknown>[];
  shipments?: Record<string, unknown>[];
  transfers?: Record<string, unknown>[];
  packages?: Record<string, unknown>[];
  version: number;
  method?: string;
  units?: {
    ok: boolean;
    definition_changes: number;
    quantity_observations: number;
    method: string;
  };
  lot_controls?: {
    ok: boolean;
    baseline_version: number;
    policy_changes: number;
    eligibility_decisions: number;
  };
};
function mergeRecords<T extends { id: string }>(
  current: T[],
  older: T[] = [],
): T[] {
  const records = new Map(older.map((item) => [item.id, item]));
  for (const item of current) records.set(item.id, item);
  return [...records.values()];
}
const nav: { id: Tab; title: string; icon: typeof Box }[] = [
  { id: "overview", title: "Overview", icon: LayoutDashboard },
  { id: "receiving", title: "Receiving", icon: ArrowDownToLine },
  { id: "inventory", title: "Inventory", icon: Boxes },
  { id: "fulfillment", title: "Fulfillment", icon: PackageCheck },
  { id: "scanner", title: "Scanner", icon: ScanLine },
  { id: "transfers", title: "Facility transfers", icon: Truck },
  { id: "locations", title: "Locations", icon: MapPin },
  { id: "exceptions", title: "Exceptions", icon: AlertTriangle },
  { id: "activity", title: "Movement ledger", icon: ActivityIcon },
  { id: "integrations", title: "Integrations", icon: Link2 },
  { id: "operations", title: "Operations", icon: ShieldCheck },
];
const fmt = (n: number) => new Intl.NumberFormat("en-US").format(n);
const when = (s: string) =>
  new Date(s).toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
const opts = (items: string[]) =>
  items.map((value) => ({ value, label: value }));
export default function App({ auth }: { auth?: WarehouseAuth }) {
  const [token, setToken] = useState(() =>
    auth ? "workos" : (sessionStorage.getItem("wms-token") ?? ""),
  );
  const [session, setSession] = useState<Session | null>(null);
  const [state, setState] = useState<WarehouseState | null>(null);
  const availabilityTime = useAvailabilityTime(state);
  const [tab, setTab] = useState<Tab>(
    () =>
      (nav.some((n) => n.id === location.hash.slice(1))
        ? location.hash.slice(1)
        : "overview") as Tab,
  );
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState("all");
  const [modal, rawSetModal] = useState<Modal>(null);
  const modalVersion = useRef<number | null>(null);
  const setModal = (next: Modal) => {
    modalVersion.current = next ? (state?.version ?? null) : null;
    if (next && ("order" in next || "receipt" in next)) setDetail(null);
    setError("");
    rawSetModal(next);
  };
  const [error, setError] = useState("");
  const [toast, setToast] = useState("");
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const [switchingWarehouse, setSwitchingWarehouse] = useState(false);
  const [journal, setJournal] = useState<Journal | null>(null);
  const [serialPreparing, setSerialPreparing] = useState(false);
  const [serialReview, setSerialReview] = useState<{
    id: string;
    targets: SerialTarget[];
    operation: string;
    originalRequest: Journal["request"];
  } | null>(null);
  const serialDecision = useRef<
    ((inputs: SerialInput[] | null) => void) | null
  >(null);
  const completeSerialReview = useCallback((inputs: SerialInput[] | null) => {
    const decide = serialDecision.current;
    serialDecision.current = null;
    setSerialReview(null);
    decide?.(inputs);
  }, []);
  useEffect(
    () => () => {
      serialDecision.current?.(null);
    },
    [],
  );
  const commandInFlight = useRef(false);
  const pendingCommand = useRef<Journal | null>(null);
  pendingCommand.current = journal;
  const [recoveryBlocked, setRecoveryBlocked] = useState(false);
  const [nativeBlocked, setNativeBlocked] = useState(false);
  const [outbox, setOutbox] = useState<Outbox[]>([]);
  const [history, setHistory] = useState<HistoryState>({});
  const [detail, setDetail] = useState<{
    key: string;
    version: number;
    data: Order | Receipt;
  } | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [reconciliation, setReconciliation] =
    useState<ReconciliationProof | null>(null);
  const [fullReconciliation, setFullReconciliation] = useState<{
    warehouse: string;
    proof: ReconciliationProof;
  } | null>(null);
  const [fullReconciliationBusy, setFullReconciliationBusy] = useState(false);
  const [fullReconciliationError, setFullReconciliationError] = useState("");
  const reconciliationSequence = useRef(0);
  const epoch = useRef(0);
  const refreshSequence = useRef(0);
  const controllers = useRef(new Set<AbortController>());
  const closeModal = useCallback(() => {
    modalVersion.current = null;
    rawSetModal(null);
  }, []);
  const request = useCallback(
    async (path: string, body?: unknown) => {
      const controller = new AbortController();
      controllers.current.add(controller);
      const timeout = setTimeout(() => controller.abort(), 20000);
      let cancelCredential: (() => void) | undefined;
      try {
        const credential = auth
          ? await new Promise<string>((resolve, reject) => {
              cancelCredential = () =>
                reject(
                  new Error(
                    "Sign-in was interrupted. Check the original save after signing in again.",
                  ),
                );
              controller.signal.addEventListener("abort", cancelCredential, {
                once: true,
              });
              void auth.getAccessToken().then(resolve, reject);
            })
          : token;
        controller.signal.throwIfAborted();
        const response = await fetch(path, {
          signal: controller.signal,
          method: body ? "POST" : "GET",
          headers: {
            Authorization: `Bearer ${credential}`,
            ...(body ? { "Content-Type": "application/json" } : {}),
          },
          body: body ? JSON.stringify(body) : undefined,
        });
        const text = await response.text();
        let data: any;
        try {
          data = JSON.parse(text);
        } catch {
          throw new ApiError(
            response.status >= 400 ? response.status : 502,
            "The API returned an unreadable response.",
          );
        }
        if (!response.ok)
          throw new ApiError(
            response.status,
            data.error ?? text ?? `HTTP ${response.status}`,
            data.command_outcome,
          );
        return data;
      } finally {
        if (cancelCredential)
          controller.signal.removeEventListener("abort", cancelCredential);
        clearTimeout(timeout);
        controllers.current.delete(controller);
      }
    },
    [token, auth],
  );
  const refresh = useCallback(
    async (warehouse: string) => {
      const current = epoch.current;
      const sequence = ++refreshSequence.current;
      const [s, o] = await Promise.all([
        request(`/v1/warehouses/${encodeURIComponent(warehouse)}`),
        request(`/v1/warehouses/${encodeURIComponent(warehouse)}/outbox`),
      ]);
      if (s.warehouse?.id !== warehouse)
        throw new Error(
          "Warehouse response did not match the selected facility.",
        );
      if (current === epoch.current && sequence === refreshSequence.current) {
        setState(s);
        setOutbox(o.items);
        setReconciliation(s.reconciliation);
        return s as WarehouseState;
      }
    },
    [request],
  );
  const enterWarehouse = async (warehouse: string): Promise<boolean> => {
    if (!session) return false;
    try {
      const owner = `${session.identity.tenant}:${session.identity.actor}`;
      // Completed callbacks may cross facilities too. Use durable journals and
      // the immediate command ref, not rendered flags that can lag a clear.
      if (
        pendingCommand.current ||
        sessionStorage.getItem(`wms-pending:${owner}`) ||
        sessionStorage.getItem(`wms-native:${owner}`)
      ) {
        setError(
          "Resolve the remaining original warehouse or integration request before changing facilities.",
        );
        return false;
      }
    } catch {
      setError(
        "Browser recovery storage is unavailable. Restore access before changing warehouses.",
      );
      return false;
    }
    const current = ++epoch.current;
    ++refreshSequence.current;
    ++reconciliationSequence.current;
    for (const controller of controllers.current) controller.abort();
    commandInFlight.current = false;
    closeModal();
    setDetail(null);
    setDetailLoading(false);
    setHistory({});
    setFullReconciliation(null);
    setFullReconciliationBusy(false);
    setFullReconciliationError("");
    setSearch("");
    setFilter("all");
    setTab("overview");
    location.hash = "overview";
    setError("");
    setBusy(false);
    setLoading(true);
    setSwitchingWarehouse(true);
    try {
      const opened = await refresh(warehouse);
      if (current !== epoch.current || !opened) return false;
      try {
        sessionStorage.setItem(
          `wms-warehouse:${session.identity.tenant}:${session.identity.actor}`,
          warehouse,
        );
      } catch {
        setError(
          "Warehouse opened, but this browser could not remember the selection for reload.",
        );
      }
      setToast(`Opened ${opened.warehouse.name}.`);
      return true;
    } catch (failure) {
      if (current === epoch.current)
        setError(`Could not open warehouse: ${(failure as Error).message}`);
      return false;
    } finally {
      if (current === epoch.current) {
        setSwitchingWarehouse(false);
        setLoading(false);
        setBusy(false);
      }
    }
  };
  const refreshCompleted = async (warehouse: string) => {
    if (state?.warehouse.id === warehouse) await refresh(warehouse);
    else await enterWarehouse(warehouse);
  };
  const switchWarehouse = async (warehouse: string): Promise<boolean> => {
    if (
      !session ||
      busy ||
      loading ||
      journal ||
      recoveryBlocked ||
      nativeBlocked ||
      commandInFlight.current ||
      pendingCommand.current
    )
      return false;
    try {
      // Check the durable native journal synchronously as well as its rendered
      // busy state: a second click must not race a just-started native intent.
      if (
        sessionStorage.getItem(
          `wms-native:${session.identity.tenant}:${session.identity.actor}`,
        )
      )
        return false;
    } catch {
      setError(
        "Browser recovery storage is unavailable. Restore access before changing warehouses.",
      );
      return false;
    }
    if (warehouse === state?.warehouse.id) return true;
    return enterWarehouse(warehouse);
  };
  const runFullReconciliation = useCallback(
    async (warehouse: string) => {
      const current = epoch.current;
      const sequence = ++reconciliationSequence.current;
      setFullReconciliationBusy(true);
      setFullReconciliationError("");
      try {
        const proof: ReconciliationProof = await request(
          `/v1/warehouses/${encodeURIComponent(warehouse)}/reconciliation`,
        );
        if (
          current === epoch.current &&
          sequence === reconciliationSequence.current
        )
          setFullReconciliation({ warehouse, proof });
      } catch (e) {
        if (
          current === epoch.current &&
          sequence === reconciliationSequence.current
        )
          setFullReconciliationError((e as Error).message);
      } finally {
        if (
          current === epoch.current &&
          sequence === reconciliationSequence.current
        )
          setFullReconciliationBusy(false);
      }
    },
    [request],
  );
  useEffect(() => {
    if (tab === "activity" && state?.warehouse.id)
      void runFullReconciliation(state.warehouse.id);
  }, [tab, state?.warehouse.id, state?.version, runFullReconciliation]);
  const detailKey =
    modal && "order" in modal
      ? `orders/${encodeURIComponent(modal.order)}`
      : modal && "receipt" in modal
        ? `receipts/${encodeURIComponent(modal.receipt)}`
        : null;
  useEffect(() => {
    if (!detailKey || !state?.warehouse.id) return;
    let cancelled = false;
    const version = state.version;
    setDetail(null);
    setDetailLoading(true);
    setError("");
    void request(
      `/v1/warehouses/${encodeURIComponent(state.warehouse.id)}/${detailKey}`,
    )
      .then((data) => {
        if (!cancelled) {
          modalVersion.current = version;
          setDetail({
            key: detailKey,
            version,
            data: data.order ?? data.receipt ?? data,
          });
        }
      })
      .catch((e) => {
        if (!cancelled) setError((e as Error).message);
      })
      .finally(() => {
        if (!cancelled) setDetailLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [detailKey, state?.warehouse.id, state?.version, request]);
  useEffect(() => {
    if (!token) return;
    const current = ++epoch.current;
    setLoading(true);
    setError("");
    void (async () => {
      try {
        const s: Session = await request("/v1/session");
        const list = await request("/v1/warehouses");
        if (current !== epoch.current) return;
        setSession(s);
        if (!auth) sessionStorage.setItem("wms-token", token);
        const selectionKey = `wms-warehouse:${s.identity.tenant}:${s.identity.actor}`;
        const remembered = sessionStorage.getItem(selectionKey);
        let preferred = remembered;
        const nativeSaved = sessionStorage.getItem(
          `wms-native:${s.identity.tenant}:${s.identity.actor}`,
        );
        setNativeBlocked(!!nativeSaved);
        if (nativeSaved) {
          try {
            const native = JSON.parse(nativeSaved);
            if (typeof native.warehouse === "string")
              preferred = native.warehouse;
          } catch {
            /* The native panel preserves and explains the unreadable journal. */
          }
        }
        const saved = sessionStorage.getItem(
          `wms-pending:${s.identity.tenant}:${s.identity.actor}`,
        );
        if (saved) {
          try {
            const parsed = JSON.parse(saved);
            if (
              parsed.tenant !== s.identity.tenant ||
              parsed.actor !== s.identity.actor ||
              typeof parsed.warehouse !== "string" ||
              typeof parsed.request?.request_id !== "string" ||
              !Number.isSafeInteger(parsed.request?.expected_version) ||
              typeof parsed.request?.command?.type !== "string" ||
              !validUnitInputs(parsed.request?.unit_inputs) ||
              !validSerialInputs(parsed.request?.serial_inputs)
            )
              throw new Error("Invalid saved request");
            setJournal(parsed);
            preferred = parsed.warehouse;
          } catch {
            setRecoveryBlocked(true);
            setError(
              "The saved request is unreadable. Do not repeat the physical action; review the ledger.",
            );
          }
        }
        // Directory order is not the login default. The server's bootstrap
        // facility may be outside the first page; retained work takes priority.
        const listedDefault = list.warehouses.find(
          (row: { warehouse: { id: string } }) =>
            row.warehouse.id === "wh-main" || row.warehouse.id === "wh-pdx",
        )?.warehouse.id;
        const candidates = [
          ...new Set([
            preferred,
            remembered,
            s.default_warehouse_id,
            listedDefault,
            list.warehouses[0]?.warehouse.id,
          ]),
        ].filter((id): id is string => typeof id === "string" && !!id);
        let selected: string | undefined;
        for (const warehouse of candidates) {
          try {
            await refresh(warehouse);
            selected = warehouse;
            break;
          } catch (failure) {
            // A retained creation can be lost before its header arrives. Keep
            // its journal while falling back to an existing selected facility.
            if ((failure as ApiError).status !== 404) throw failure;
          }
        }
        if (!selected)
          throw new Error("No accessible warehouse could be opened.");
        if (current === epoch.current)
          sessionStorage.setItem(selectionKey, selected);
      } catch (e) {
        if (current === epoch.current) setError((e as Error).message);
      } finally {
        if (current === epoch.current) setLoading(false);
      }
    })();
    return () => {
      ++epoch.current;
      for (const c of controllers.current) c.abort();
    };
  }, [token, request, refresh, auth]);
  useEffect(() => {
    if (!toast) return;
    const timer = setTimeout(() => setToast(""), 4500);
    return () => clearTimeout(timer);
  }, [toast]);
  useEffect(() => {
    const followHash = () => {
      const requested = location.hash.slice(1);
      setTab(
        nav.some((entry) => entry.id === requested)
          ? (requested as Tab)
          : "overview",
      );
      setSearch("");
      setFilter("all");
    };
    window.addEventListener("hashchange", followHash);
    return () => window.removeEventListener("hashchange", followHash);
  }, []);
  const navigate = (next: Tab) => {
    setTab(next);
    setSearch("");
    setFilter("all");
    location.hash = next;
  };
  const logout = () => {
    ++epoch.current;
    commandInFlight.current = false;
    pendingCommand.current = null;
    completeSerialReview(null);
    setSerialPreparing(false);
    for (const c of controllers.current) c.abort();
    sessionStorage.removeItem("wms-token");
    setToken("");
    setSession(null);
    setState(null);
    setFullReconciliation(null);
    setFullReconciliationBusy(false);
    setFullReconciliationError("");
    setHistory({});
    setDetail(null);
    setJournal(null);
    setRecoveryBlocked(false);
    setNativeBlocked(false);
    setModal(null);
    setError("");
    setBusy(false);
    setLoading(false);
    setSwitchingWarehouse(false);
    auth?.signOut();
  };
  const journalKey = session
    ? `wms-pending:${session.identity.tenant}:${session.identity.actor}`
    : "";
  const clearJournal = () => {
    sessionStorage.removeItem(journalKey);
    pendingCommand.current = null;
    setJournal(null);
  };
  const finish = async (j: Journal, recovering = false): Promise<boolean> => {
    const current = epoch.current;
    setBusy(true);
    setError("");
    try {
      const result = await request(
        `/v1/warehouses/${j.warehouse}/commands`,
        j.request,
      );
      if (current !== epoch.current) return false;
      clearJournal();
      setModal(null);
      setToast(
        result.replayed
          ? "Original save confirmed. No action was repeated."
          : j.request.command.type === "create_warehouse"
            ? "Warehouse created. Audit record saved."
            : "Warehouse updated. Audit record saved.",
      );
      try {
        await refreshCompleted(j.warehouse);
      } catch {
        setError(
          "Save confirmed, but the refreshed view is unavailable. Use Refresh; do not repeat the action.",
        );
      }
      return true;
    } catch (e) {
      if (current !== epoch.current) return false;
      const err = e as ApiError;
      if (recovering && provesStaleCommandDidNotCommit(err, j)) {
        clearJournal();
        setModal(null);
        let refreshed = true;
        try {
          await refresh(j.warehouse);
        } catch {
          refreshed = false;
        }
        if (current === epoch.current)
          setError(
            "The original save was not committed because the warehouse advanced. Review current work and physical stock before starting a new save." +
              (refreshed
                ? ""
                : " The refreshed view is unavailable; use Refresh before continuing."),
          );
        return false;
      }
      if (
        !recovering &&
        err.status >= 400 &&
        err.status < 500 &&
        ![408, 429].includes(err.status)
      ) {
        clearJournal();
        if (err.status === 409) {
          try {
            await refresh(j.warehouse);
          } catch {
            /* Preserve original error. */
          }
        }
      }
      setError(
        (err.message ||
          "Save response unavailable. Check the original request before continuing.") +
          (err.status === 409
            ? " Close and reopen the dialog to review current stock before trying again."
            : ""),
      );
      return false;
    } finally {
      if (current === epoch.current) setBusy(false);
    }
  };
  const submit = async (
    command: Command,
    expectedVersion?: number,
    capturedVersions?: Record<string, number>,
    unitInputs?: UnitInput[],
  ): Promise<boolean> => {
    if (
      !state ||
      !session ||
      journal ||
      busy ||
      loading ||
      recoveryBlocked ||
      nativeBlocked ||
      commandInFlight.current ||
      pendingCommand.current
    )
      return false;
    commandInFlight.current = true;
    const current = epoch.current;
    const creating = command.type === "create_warehouse";
    const j: Journal = {
      tenant: session.identity.tenant,
      actor: session.identity.actor,
      warehouse: creating ? `wh-${crypto.randomUUID()}` : state.warehouse.id,
      request: {
        request_id: crypto.randomUUID(),
        expected_version: creating
          ? 0
          : (expectedVersion ?? modalVersion.current ?? state.version),
        ...(!creating &&
        (expectedVersion ?? modalVersion.current ?? state.version) ===
          state.version
          ? {
              resource_versions:
                capturedVersions ?? reviewedVersions(state, command),
            }
          : !creating && capturedVersions
            ? { resource_versions: capturedVersions }
            : {}),
        command,
        ...(unitInputs?.length
          ? { unit_inputs: structuredClone(unitInputs) }
          : {}),
      },
    };
    try {
      if (
        serialCommands.has(command.type) &&
        state.products.some((p) => p.serial_controlled === true)
      ) {
        setSerialPreparing(true);
        const plan = await request(
          `/v1/warehouses/${encodeURIComponent(j.warehouse)}/serial-observation-plan`,
          { plan_id: j.request.request_id, command: j.request.command },
        );
        if (current !== epoch.current) return false;
        if (
          plan.schema_version !== 1 ||
          plan.plan_id !== j.request.request_id ||
          plan.warehouse_id !== j.warehouse ||
          plan.command_type !== command.type ||
          !validSerialTargets(plan.targets) ||
          !Number.isSafeInteger(plan.required_labels) ||
          plan.required_labels !==
            plan.targets.reduce(
              (n: number, t: SerialTarget) => n + t.each_quantity,
              0,
            )
        ) {
          throw new Error(
            "Serial observation scope did not match the original action. Nothing was saved.",
          );
        }
        if (plan.targets.length) {
          const decision = new Promise<SerialInput[] | null>((resolve) => {
            serialDecision.current = resolve;
          });
          setSerialReview({
            id: j.request.request_id,
            targets: plan.targets,
            operation: command.type,
            originalRequest: structuredClone(j.request),
          });
          setSerialPreparing(false);
          const inputs = await decision;
          if (current !== epoch.current || !inputs) {
            if (current === epoch.current) commandInFlight.current = false;
            return false;
          }
          if (
            !validSerialInputs(inputs) ||
            inputs.length !== plan.targets.length ||
            inputs.some(
              (i, n) =>
                JSON.stringify(i.target) !==
                  JSON.stringify(plan.targets[n].target) ||
                i.sku !== plan.targets[n].sku ||
                i.serials.length !== plan.targets[n].each_quantity,
            )
          )
            throw new Error(
              "The complete original serial observation is required.",
            );
          j.request.serial_inputs = structuredClone(inputs);
        }
      }
      if (
        new TextEncoder().encode(JSON.stringify(j.request)).length >
        256 * 1024
      )
        throw new Error(
          "The complete request exceeds one save. No identities were removed or saved. Large complete counts require reviewed count sessions.",
        );
    } catch (failure) {
      if (current === epoch.current) setError((failure as Error).message);
      if (current === epoch.current) commandInFlight.current = false;
      return false;
    } finally {
      if (current === epoch.current) setSerialPreparing(false);
    }
    if (current !== epoch.current) return false;
    try {
      sessionStorage.setItem(journalKey, JSON.stringify(j));
    } catch {
      setError(
        "The browser could not preserve this request for recovery. Enable session storage before saving.",
      );
      commandInFlight.current = false;
      return false;
    }
    pendingCommand.current = j;
    setJournal(j);
    try {
      return await finish(j);
    } finally {
      if (current === epoch.current) commandInFlight.current = false;
    }
  };
  const recover = async () => {
    if (!journal || commandInFlight.current) return;
    commandInFlight.current = true;
    const current = epoch.current;
    setBusy(true);
    setError("");
    try {
      const r = await request(
        `/v1/warehouses/${journal.warehouse}/commands/${journal.request.request_id}`,
      );
      if (current !== epoch.current) return;
      if (r.found) {
        const w = journal.warehouse;
        clearJournal();
        setModal(null);
        setToast("Original save confirmed.");
        await refreshCompleted(w);
      } else {
        await finish(journal, true);
      }
    } catch (e) {
      if (current === epoch.current) setError((e as Error).message);
    } finally {
      if (current === epoch.current) {
        commandInFlight.current = false;
        setBusy(false);
      }
    }
  };

  if ((!session || !state) && auth)
    return (
      <main className="login-form">
        <h1>Open your warehouse</h1>
        <p role={error ? "alert" : "status"}>
          {error || "Loading your authorized workspace…"}
        </p>
        <button className="btn" onClick={logout}>
          Sign out
        </button>
      </main>
    );
  if (!session || !state)
    return (
      <div className="login-screen">
        <div className="login-art">
          <div className="brand">
            <span className="brand-mark">
              <Boxes size={24} />
            </span>
            stateset<span className="brand-period">.</span>
          </div>
          <div>
            <span className="eyebrow">WAREHOUSE OPERATIONS</span>
            <h1>
              Every unit.
              <br />
              Every movement.
              <br />
              <em>Accounted for.</em>
            </h1>
            <p>
              A clear view of your warehouse, from the receiving dock to the
              final handoff.
            </p>
          </div>
          <div className="login-grid" aria-hidden="true">
            {Array.from({ length: 35 }, (_, i) => (
              <span key={i} className={i % 8 === 0 ? "lit" : ""} />
            ))}
          </div>
        </div>
        <form
          className="login-form"
          onSubmit={(e: FormEvent<HTMLFormElement>) => {
            e.preventDefault();
            setToken(String(new FormData(e.currentTarget).get("token")));
          }}
        >
          <div className="icon-tile">
            <Warehouse size={26} />
          </div>
          <h2>Open your warehouse</h2>
          <p>Connect with your organization’s warehouse API credential.</p>
          <label>
            API credential
            <input
              type="password"
              name="token"
              autoComplete="off"
              placeholder="Enter your credential"
              required
            />
          </label>
          <button className="btn primary" disabled={loading}>
            {loading ? (
              <Loader2 size={16} className="spin" />
            ) : (
              <ArrowRight size={16} />
            )}
            Connect to workspace
          </button>
          <div className="login-divider">or explore locally</div>
          <button
            type="button"
            className="btn"
            disabled={loading}
            onClick={() => {
              if (token === "demo-warehouse") {
                setToken("");
                setTimeout(() => setToken("demo-warehouse"), 0);
              } else setToken("demo-warehouse");
            }}
          >
            Open synthetic demo <ArrowUpRight size={16} />
          </button>
          <small>
            Demo access requires the API to run with WMS_DEMO=true. All
            warehouse data in the demo is synthetic.
          </small>
          {error && (
            <div role="alert" className="alert error">
              {error}
            </div>
          )}
        </form>
      </div>
    );

  const admin = session.identity.role === "admin";
  const canWork = ["admin", "operator"].includes(session.identity.role);
  const disabled =
    busy ||
    loading ||
    !!journal ||
    recoveryBlocked ||
    nativeBlocked ||
    serialPreparing ||
    !!serialReview;
  if (switchingWarehouse)
    return (
      <main className="warehouse-loading" role="status">
        <Loader2 className="spin" />
        <p>Loading warehouse…</p>
        <button className="btn" onClick={logout}>
          Sign out
        </button>
      </main>
    );
  const s = {
    ...state,
    orders: mergeRecords(state.orders, history.orders?.items),
    receipts: mergeRecords(state.receipts, history.receipts?.items).sort(
      (a, b) => a.created_at.localeCompare(b.created_at),
    ),
    movements: mergeRecords(state.movements, history.movements?.items).sort(
      (a, b) => a.at.localeCompare(b.at),
    ),
    exceptions: mergeRecords(state.exceptions, history.exceptions?.items),
    activity: mergeRecords(state.activity, history.activity?.items).sort(
      (a, b) => a.at.localeCompare(b.at),
    ),
  };
  const fullProof =
    fullReconciliation?.warehouse === s.warehouse.id
      ? fullReconciliation.proof
      : null;
  const fullProofReady =
    !!fullProof &&
    fullProof.version === s.version &&
    !fullReconciliationBusy &&
    !fullReconciliationError;
  const qualityProof = fullProof?.lot_controls;
  const qualityProofRequired =
    !!s.lot_controls?.length || s.products.some((p) => p.expiry_required);
  const qualityProofValid =
    !!qualityProof &&
    typeof qualityProof.ok === "boolean" &&
    [
      qualityProof.baseline_version,
      qualityProof.policy_changes,
      qualityProof.eligibility_decisions,
    ].every((v) => Number.isSafeInteger(v) && v >= 0) &&
    qualityProof.baseline_version <= (fullProof?.version ?? -1);
  const unitProof = fullProof?.units;
  const unitProofRequired = !!s.unit_packs?.length;
  const unitProofValid =
    !!unitProof &&
    typeof unitProof.ok === "boolean" &&
    [unitProof.definition_changes, unitProof.quantity_observations].every(
      (n) => Number.isSafeInteger(n) && n >= 0,
    ) &&
    typeof unitProof.method === "string";
  const fullProofOk =
    fullProofReady &&
    fullProof.ok &&
    ((unitProof === undefined && !unitProofRequired) ||
      (unitProofValid && unitProof?.ok)) &&
    ((qualityProof === undefined && !qualityProofRequired) ||
      (qualityProofValid && qualityProof?.ok));
  const shipmentDifferences =
    fullProofReady && Array.isArray(fullProof.shipments)
      ? fullProof.shipments.filter((proof) => proof?.ok !== true)
      : [];
  const transferDifferences =
    fullProofReady && Array.isArray(fullProof.transfers)
      ? fullProof.transfers.filter((p) => p?.ok !== true)
      : [];
  const packageDifferences =
    fullProofReady && Array.isArray(fullProof.packages)
      ? fullProof.packages.filter((p) => p?.ok !== true)
      : [];
  const loadHistory = async <K extends HistoryKind>(kind: K) => {
    const previous = history[kind];
    if (previous?.loading || (previous && previous.nextCursor === null)) return;
    const currentEpoch = epoch.current;
    setHistory((value) => ({
      ...value,
      [kind]: {
        items: previous?.items ?? [],
        nextCursor: previous?.nextCursor ?? null,
        loading: true,
      },
    }));
    try {
      const before = previous?.nextCursor;
      const page: HistoryPage<HistoryData[K]> = await request(
        `/v1/warehouses/${encodeURIComponent(
          s.warehouse.id,
        )}/history/${kind}?limit=50${before == null ? "" : `&before=${before}`}`,
      );
      if (currentEpoch !== epoch.current) return;
      setHistory((value) => ({
        ...value,
        [kind]: {
          items: mergeRecords(
            page.items.map((item) => item.data),
            previous?.items,
          ),
          nextCursor: page.next_cursor,
          loading: false,
        },
      }));
    } catch (e) {
      if (currentEpoch !== epoch.current) return;
      setError((e as Error).message);
      setHistory((value) => ({ ...value, [kind]: previous }));
    }
  };
  const historyControl = (kind: HistoryKind) => (
    <div className="detail-actions">
      {history[kind]?.nextCursor === null && !history[kind]?.loading ? (
        <span>All {kind === "activity" ? "audit records" : kind} loaded</span>
      ) : (
        <button
          className="btn"
          disabled={history[kind]?.loading}
          onClick={() => void loadHistory(kind)}
        >
          {history[kind]?.loading ? (
            <Loader2 size={16} className="spin" />
          ) : (
            <Clock3 size={16} />
          )}
          {history[kind] ? "Load older" : "Browse"}{" "}
          {kind === "activity" ? "audit records" : kind} history
        </button>
      )}
    </div>
  );
  const totals = {
    onHand: s.stocks.reduce((n, x) => n + x.on_hand, 0),
    reserved: s.stocks.reduce((n, x) => n + x.reserved, 0),
    receiving: s.receipts.filter((r) =>
      ["expected", "partial"].includes(r.status),
    ).length,
    active: s.orders.filter(
      (o) => !["shipped", "cancelled", "closed_partial"].includes(o.status),
    ).length,
  };
  const openExceptions = s.exceptions.filter((e) => e.status === "open");
  const availableStorage = s.stocks.filter(
    (b) =>
      s.locations.some((l) => l.code === b.location && l.kind === "storage") &&
      availability(s, b.sku, b.lot, availabilityTime) === "available",
  );
  const lowStock = s.products.filter(
    (p) =>
      availableStorage
        .filter((b) => b.sku === p.sku)
        .reduce((n, b) => n + b.on_hand - b.reserved, 0) < p.reorder_point,
  );
  const matches = (...values: (string | undefined | null)[]) =>
    values.join(" ").toLowerCase().includes(search.toLowerCase());
  const fieldOptions = (kind: string) =>
    opts(s.locations.filter((l) => l.kind === kind).map((l) => l.code));
  const createOrder = () =>
    setModal({
      title: "New fulfillment order",
      subtitle:
        "Create warehouse execution work. Commercial order ownership remains in the OMS.",
      fields: [
        { name: "reference", label: "Order reference", placeholder: "SO-1047" },
        {
          name: "source_order_id",
          label: "OMS order ID (optional)",
          optional: true,
        },
        {
          name: "customer",
          label: "Customer",
          placeholder: "Customer or account name",
        },
        {
          name: "destination",
          label: "Destination",
          placeholder: "City, region",
        },
        {
          name: "priority",
          label: "Priority",
          type: "select",
          options: opts(["normal", "high", "urgent"]),
        },
      ],
      lines: "order",
      submit: "Create order",
      build: (d) => ({
        type: "create_order",
        ...d,
        source_order_id: d.source_order_id || null,
      }),
    });
  const createReceipt = () =>
    setModal({
      title: "Schedule inbound receipt",
      subtitle: "Register expected quantities before receiving physical goods.",
      fields: [
        {
          name: "reference",
          label: "Receipt reference",
          placeholder: "RCV-208",
        },
        { name: "supplier", label: "Supplier", placeholder: "Supplier name" },
      ],
      lines: "receipt",
      submit: "Create receipt",
      build: (d) => ({ type: "create_receipt", ...d }),
    });
  const moveStock = (stock?: Stock) =>
    setModal({
      title: "Put away / move stock",
      subtitle:
        "Move unreserved goods into storage. Physical stock stays in the warehouse.",
      fields: [
        {
          name: "sku",
          label: "SKU",
          type: "select",
          options: s.products.map((p) => ({
            value: p.sku,
            label: `${p.name} · ${p.sku}`,
          })),
          value: stock?.sku,
        },
        { name: "lot", label: "Lot", value: stock?.lot ?? "UNTRACKED" },
        {
          name: "from",
          label: "From location",
          type: "select",
          options: opts(
            s.locations.filter((l) => l.kind !== "staging").map((l) => l.code),
          ),
          value: stock?.location,
        },
        {
          name: "to",
          label: "To storage location",
          type: "select",
          options: fieldOptions("storage"),
        },
        {
          name: "quantity",
          label: "Quantity (each)",
          type: "number",
          value: stock ? stock.on_hand - stock.reserved : 1,
        },
      ],
      submit: "Confirm movement",
      build: (d) => ({
        type: "move_stock",
        ...d,
        quantity: Number(d.quantity),
      }),
    });
  const countStock = (stock: Stock) =>
    setModal({
      title: "Record physical count",
      unitSku: stock.sku,
      subtitle: `Review current bin stock before confirming. ${stock.reserved} units are reserved and cannot be written off.`,
      fields: [
        {
          name: "counted",
          label: `Counted each units · ${stock.sku} / ${stock.location}`,
          type: "number",
          value: stock.on_hand,
          min: 0,
        },
        { name: "reason", label: "Count evidence / reason", type: "textarea" },
      ],
      submit: "Apply count adjustment",
      build: (d) => ({
        type: "count_stock",
        sku: stock.sku,
        lot: stock.lot,
        location: stock.location,
        counted: Number(d.counted),
        reason: d.reason,
      }),
    });
  const report = (order?: Order) =>
    setModal({
      title: "Report warehouse exception",
      subtitle:
        "Order-linked exceptions block allocation, picking, packing and handoff until resolved.",
      fields: [
        {
          name: "category",
          label: "Category",
          type: "select",
          options: opts([
            "Stock shortage",
            "Damaged goods",
            "Location mismatch",
            "Packing discrepancy",
            "Other",
          ]),
        },
        { name: "detail", label: "Observed issue", type: "textarea" },
      ],
      submit: "Open exception",
      build: (d) => ({
        type: "report_exception",
        order_id: order?.id ?? null,
        ...d,
      }),
    });
  const csv = (filename: string, rows: Record<string, unknown>[]) => {
    const keys = [...new Set(rows.flatMap((row) => Object.keys(row)))];
    const cell = (v: unknown) => {
      const text = String(v ?? "");
      return (
        '"' +
        (typeof v === "string" && /^(?:\s*[=+\-@]|[\t\r])/.test(text)
          ? "'"
          : "") +
        text.replaceAll('"', '""') +
        '"'
      );
    };
    const blob = new Blob(
      [
        "\uFEFF" +
          [keys, ...rows.map((r) => keys.map((k) => r[k]))]
            .map((row) => row.map(cell).join(","))
            .join("\r\n"),
      ],
      { type: "text/csv;charset=utf-8" },
    );
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    a.click();
    URL.revokeObjectURL(url);
  };
  const action = (c: Command) => {
    void submit(c);
  };
  const orderTable = (orders: Order[]) => (
    <div className="table-scroll">
      <table>
        <thead>
          <tr>
            <th>Order / customer</th>
            <th>Destination</th>
            <th>Units</th>
            <th>Status</th>
            <th>Priority</th>
            <th aria-label="Open order" />
          </tr>
        </thead>
        <tbody>
          {orders.map((o) => (
            <tr key={o.id}>
              <td>
                <button
                  className="table-link"
                  onClick={() => setModal({ order: o.id })}
                >
                  {o.reference}
                </button>
                <span className="subline">{o.customer}</span>
              </td>
              <td>{o.destination}</td>
              <td className="mono">
                {fmt(o.lines.reduce((n, l) => n + l.quantity, 0))}
              </td>
              <td>
                <Status value={o.status} />
              </td>
              <td>
                <span className={`priority ${o.priority}`}>
                  <i />
                  {o.priority}
                </span>
              </td>
              <td>
                <button
                  className="icon-btn"
                  aria-label={`Open ${o.reference}`}
                  onClick={() => setModal({ order: o.id })}
                >
                  <ChevronRight size={17} />
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {!orders.length && (
        <Empty
          title="No matching orders"
          detail="New fulfillment work will appear here."
        />
      )}
    </div>
  );
  const selectedOrder =
    modal &&
    "order" in modal &&
    detail?.key === detailKey &&
    detail.version === s.version
      ? (detail.data as Order)
      : undefined;
  const selectedReceipt =
    modal &&
    "receipt" in modal &&
    detail?.key === detailKey &&
    detail.version === s.version
      ? (detail.data as Receipt)
      : undefined;
  const orderBlocked =
    !!selectedOrder &&
    s.exceptions.some(
      (issue) => issue.order_id === selectedOrder.id && issue.status === "open",
    );
  const legacyLotHeld =
    !!selectedOrder &&
    selectedOrder.allocations.some(
      (a) =>
        a.quantity > a.shipped + a.released &&
        availability(s, a.sku, a.lot, availabilityTime) !== "available",
    );

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <a
          className="brand"
          href="#overview"
          onClick={(e) => {
            e.preventDefault();
            navigate("overview");
          }}
        >
          <span className="brand-mark">
            <Boxes size={22} />
          </span>
          stateset<span className="brand-period">.</span>
        </a>
        <div className="product-label">WAREHOUSE</div>
        <div className="workspace-card">
          <span className="workspace-icon">
            <Warehouse size={19} />
          </span>
          <div>
            <strong>{s.warehouse.code}</strong>
            <small>{s.warehouse.city}</small>
          </div>
          <span className="live-dot" />
        </div>
        <div className="nav-label">WORKSPACE</div>
        <nav>
          {nav
            .filter((n) => n.id !== "operations" || canWork)
            .map((n) => (
              <button
                key={n.id}
                aria-label={n.title}
                title={n.title}
                className={tab === n.id ? "active" : ""}
                onClick={() => navigate(n.id)}
              >
                <n.icon size={18} />
                <span>{n.title}</span>
                {n.id === "exceptions" && openExceptions.length > 0 && (
                  <b>{openExceptions.length}</b>
                )}
              </button>
            ))}
        </nav>
        <div className="sidebar-bottom">
          <div className="system-note">
            <span className="live-dot" />
            <span>
              {session.demo
                ? "Synthetic demo workspace"
                : "Connected to warehouse"}
            </span>
          </div>
          <button className="help-btn" onClick={() => setModal({ help: true })}>
            <CircleHelp size={17} />
            Workspace guide
            <ArrowUpRight size={15} />
          </button>
          <div className="profile">
            <div className="avatar">
              {session.identity.actor
                .split(":")
                .pop()
                ?.slice(0, 2)
                .toUpperCase()}
            </div>
            <div>
              <strong>
                {session.demo ? "Demo operator" : session.identity.actor}
              </strong>
              <small>
                {session.identity.role} · {session.identity.tenant}
              </small>
            </div>
            <button className="icon-btn" onClick={logout} aria-label="Sign out">
              <LogOut size={16} />
            </button>
          </div>
        </div>
      </aside>
      <div className="main-shell">
        <header className="topbar">
          <div className="breadcrumb">
            Workspace <ChevronRight size={14} />
            <span>{nav.find((n) => n.id === tab)?.title}</span>
          </div>
          <div className="topbar-actions">
            <WarehousePicker
              key={`${session.identity.tenant}:${session.identity.actor}:${s.warehouse.id}`}
              current={s.warehouse}
              disabled={disabled}
              admin={admin}
              request={request}
              onSelect={switchWarehouse}
              onCreate={() =>
                setModal({
                  title: "Create warehouse",
                  subtitle:
                    "Create an empty facility with receiving and staging bins. Inventory is recorded through receipts or physical counts.",
                  submit: "Create warehouse",
                  fields: [
                    { name: "code", label: "Warehouse code" },
                    { name: "name", label: "Warehouse name" },
                    { name: "city", label: "City" },
                    {
                      name: "timezone",
                      label: "IANA timezone",
                      value: s.warehouse.timezone,
                    },
                  ],
                  build: (data) => ({ type: "create_warehouse", ...data }),
                })
              }
            />
            <span className="environment">
              <span className="live-dot" />
              {session.demo ? "Demo environment" : "Live workspace"}
            </span>
            <span className="topbar-separator" />
            <button
              className="icon-btn"
              disabled={busy || loading}
              aria-label="Refresh warehouse"
              onClick={() => {
                setLoading(true);
                void refresh(s.warehouse.id)
                  .catch((e) => setError(e.message))
                  .finally(() => setLoading(false));
              }}
            >
              <RefreshCw size={17} className={loading ? "spin" : ""} />
            </button>
            <button
              className="avatar small"
              aria-label="Account and workspace guide"
              title="Account and workspace guide"
              onClick={() => setModal({ help: true })}
            >
              WH
            </button>
          </div>
        </header>
        <main>
          {nativeBlocked && tab !== "integrations" && (
            <div className="alert info" role="status">
              An integration request needs confirmation. Open Integrations to
              check or retry the original request.
              <button
                className="mini-btn"
                onClick={() => setTab("integrations")}
              >
                Open integrations
              </button>
            </div>
          )}
          {session.demo && (
            <div className="demo-banner">
              <span>
                <span className="demo-dot" />
                SYNTHETIC DATA
              </span>{" "}
              A working warehouse, ready to explore. All saves persist in the
              local demo database.
            </div>
          )}
          {error && (
            <div className="alert error" role="alert">
              <AlertTriangle size={17} />
              <span>{error}</span>
              <button
                className="icon-btn"
                onClick={() => setError("")}
                aria-label="Dismiss error"
              >
                <X size={16} />
              </button>
            </div>
          )}
          {recoveryBlocked && (
            <div className="alert warning" role="alert">
              <AlertTriangle size={18} />
              <p>
                The retained save is unreadable. Writes remain paused. Preserve
                the browser recovery data and reconcile the original action
                against command receipts and the movement ledger before clearing
                it.
              </p>
            </div>
          )}
          {journal && (
            <div className="alert warning">
              <Clock3 size={18} />
              <div>
                <strong>
                  {busy
                    ? "Saving the original request…"
                    : "A save needs confirmation"}
                </strong>
                <p>
                  Warehouse {journal.warehouse} · Request{" "}
                  {journal.request.request_id.slice(0, 8)} ·{" "}
                  {label(journal.request.command.type)}. Further writes are
                  paused.
                </p>
                {journal.request.serial_inputs?.map((input, i) => (
                  <details
                    key={`serial-${i}`}
                    className="serial-observation-summary"
                  >
                    <summary>
                      Original serial observation · {input.sku} ·{" "}
                      {input.serials.length} labels ·{" "}
                      {input.target.length
                        ? input.target.join(" / ")
                        : "physical quantity"}
                    </summary>
                    <textarea
                      aria-label={`Original serials for ${input.sku} group ${i + 1}`}
                      readOnly
                      value={input.serials.join("\n")}
                    />
                    <p>
                      {input.serials.length === 0
                        ? "Original witnessed empty count."
                        : "Literal labels retained with the original request. Do not rescan or replace them during recovery."}
                    </p>
                  </details>
                ))}
                {journal.request.unit_inputs?.map((input, i) => (
                  <p key={i} style={{ overflowWrap: "anywhere" }}>
                    Original pack observation · {input.sku} ·{" "}
                    {input.pack_quantity} {input.pack_code} ×{" "}
                    {input.each_per_pack} ={" "}
                    {input.pack_quantity * input.each_per_pack} each ·
                    definition {input.definition_id} · target{" "}
                    {input.target.length
                      ? input.target.join(" / ")
                      : "physical quantity"}
                  </p>
                ))}
                {[
                  "record_lot_expiry",
                  "correct_lot_expiry",
                  "quarantine_lot",
                  "release_lot_hold",
                  "set_product_expiry_policy",
                ].includes(journal.request.command.type) && (
                  <p>
                    {journal.request.command.sku
                      ? `SKU ${String(journal.request.command.sku)} · `
                      : ""}
                    {journal.request.command.lot
                      ? `Lot ${String(journal.request.command.lot)} · `
                      : ""}
                    {journal.request.command.hold_id
                      ? `Hold ${String(journal.request.command.hold_id)} · `
                      : ""}
                    {String(journal.request.command.reason)}
                  </p>
                )}
                {["record_transfer_loss", "reverse_transfer_loss"].includes(
                  journal.request.command.type,
                ) && (
                  <p style={{ overflowWrap: "anywhere" }}>
                    {journal.request.command.transfer_id
                      ? `Transfer ${String(journal.request.command.transfer_id)} · `
                      : ""}
                    {journal.request.command.line_id
                      ? `Line ${String(journal.request.command.line_id)} · `
                      : ""}
                    {journal.request.command.loss_id
                      ? `Original loss ${String(journal.request.command.loss_id)} · `
                      : ""}
                    {String(journal.request.command.quantity)} each ·{" "}
                    {journal.request.command.incident_reference
                      ? `Incident ${String(journal.request.command.incident_reference)} · `
                      : ""}
                    {String(journal.request.command.reason)}
                  </p>
                )}
              </div>
              <button
                className="btn"
                disabled={busy}
                onClick={() => void recover()}
              >
                Check original save
              </button>
            </div>
          )}
          <div className="page-heading">
            <div>
              <div className="eyebrow">
                {s.warehouse.code} <span>/</span>{" "}
                {session.demo ? "DEMO WORKSPACE" : "WAREHOUSE EXECUTION"}
              </div>
              <h1>
                {tab === "overview"
                  ? "Warehouse overview"
                  : nav.find((n) => n.id === tab)?.title}
              </h1>
              <p>
                {
                  {
                    overview:
                      "A clear picture of your floor. Keep every movement in sync.",
                    inventory:
                      "Know what you have, where it is, and what’s ready to move.",
                    receiving:
                      "From expected delivery to goods received and put away.",
                    fulfillment:
                      "Move every order from reservation to carrier handoff.",
                    locations:
                      "Your receiving docks, storage bins, and outbound staging.",
                    exceptions:
                      "Resolve the issues holding warehouse work back.",
                    activity:
                      "Every physical movement, with a reference and a responsible actor.",
                    integrations:
                      "Keep warehouse execution connected to the rest of StateSet.",
                    scanner:
                      "Verify the location, SKU, and lot before confirming physical work.",
                    transfers:
                      "Track physical dispatch, goods in transit and destination arrivals.",
                    operations:
                      "Review delivery health, uncertain requests, and warehouse exceptions.",
                  }[tab]
                }
              </p>
            </div>
            <div className="heading-actions">
              {tab === "overview" && (
                <button
                  className="btn"
                  onClick={() => csv("warehouse-inventory.csv", s.stocks)}
                >
                  <Download size={16} />
                  Export inventory
                </button>
              )}
              {canWork && ["overview", "fulfillment"].includes(tab) && (
                <button
                  className="btn primary"
                  disabled={disabled || !s.products.length}
                  onClick={createOrder}
                >
                  <Plus size={17} />
                  New order
                </button>
              )}
              {canWork && tab === "receiving" && (
                <button
                  className="btn primary"
                  disabled={disabled || !s.products.length}
                  onClick={createReceipt}
                >
                  <Plus size={17} />
                  New receipt
                </button>
              )}
              {canWork && tab === "inventory" && (
                <button
                  className="btn primary"
                  disabled={
                    disabled ||
                    !s.products.length ||
                    !fieldOptions("storage").length
                  }
                  onClick={() => moveStock()}
                >
                  <ArrowRight size={17} />
                  Move stock
                </button>
              )}
              {canWork && tab === "exceptions" && (
                <button
                  className="btn primary"
                  disabled={disabled}
                  onClick={() => report()}
                >
                  <Plus size={17} />
                  Report issue
                </button>
              )}
              {admin && tab === "locations" && (
                <button
                  className="btn primary"
                  disabled={disabled}
                  onClick={() =>
                    setModal({
                      title: "Add warehouse location",
                      fields: [
                        {
                          name: "code",
                          label: "Location code",
                          placeholder: "A-03-01",
                        },
                        { name: "zone", label: "Zone", placeholder: "Apparel" },
                        {
                          name: "kind",
                          label: "Location type",
                          type: "select",
                          options: opts(["storage", "receiving"]),
                        },
                      ],
                      submit: "Create location",
                      build: (d) => ({ type: "create_location", ...d }),
                    })
                  }
                >
                  <Plus size={17} />
                  Add location
                </button>
              )}
            </div>
          </div>

          {tab === "overview" && (
            <>
              <div className="metrics">
                <Metric
                  label="Units on hand"
                  value={fmt(totals.onHand)}
                  icon={Boxes}
                  note={`${s.products.length} SKUs across ${s.locations.length} locations`}
                />
                <Metric
                  label="Available in storage"
                  value={fmt(
                    availableStorage.reduce(
                      (n, b) => n + b.on_hand - b.reserved,
                      0,
                    ),
                  )}
                  icon={Package}
                  note={`${fmt(
                    totals.reserved,
                  )} units reserved across the floor`}
                />
                <Metric
                  label="Orders in progress"
                  value={fmt(totals.active)}
                  icon={PackageCheck}
                  note={`${
                    s.orders.filter((o) => o.status === "packed").length
                  } packed and ready for handoff`}
                />
                <Metric
                  label="Needs attention"
                  value={fmt(openExceptions.length + lowStock.length)}
                  icon={AlertTriangle}
                  note={`${openExceptions.length} open issues · ${lowStock.length} low-stock SKUs`}
                  attention
                />
              </div>
              <div className="overview-grid">
                <Section
                  title="Fulfillment flow"
                  detail="Current work across the warehouse"
                  action={
                    <button
                      className="text-btn"
                      onClick={() => navigate("fulfillment")}
                    >
                      View orders <ArrowUpRight size={15} />
                    </button>
                  }
                >
                  <div className="flow">
                    {[
                      {
                        status: "pending",
                        title: "To allocate",
                        icon: ClipboardCheck,
                      },
                      { status: "allocated", title: "To pick", icon: Box },
                      {
                        status: "picked",
                        title: "To pack",
                        icon: PackageCheck,
                      },
                      { status: "packed", title: "To hand off", icon: Truck },
                    ].map((f, i) => (
                      <div className="flow-step" key={f.status}>
                        <button
                          onClick={() => {
                            navigate("fulfillment");
                            setFilter(f.status);
                          }}
                        >
                          <span className={`flow-icon step-${i}`}>
                            <f.icon size={20} />
                          </span>
                          <strong>
                            {fmt(
                              s.orders.filter((o) =>
                                f.status === "allocated"
                                  ? ["allocated", "picking"].includes(o.status)
                                  : o.status === f.status,
                              ).length,
                            )}
                          </strong>
                          <span>{f.title}</span>
                        </button>
                        {i < 3 && (
                          <ChevronRight className="flow-arrow" size={18} />
                        )}
                      </div>
                    ))}
                  </div>
                  <div className="flow-footer">
                    <span>
                      <span className="live-dot" />
                      From reservation to physical handoff
                    </span>
                    <span>
                      {s.orders.filter((o) => o.status === "shipped").length}{" "}
                      completed
                    </span>
                  </div>
                </Section>
                <section className="warehouse-card">
                  <div className="warehouse-card-top">
                    <span className="warehouse-code">{s.warehouse.code}</span>
                    <Badge tone={reconciliation?.ok ? "green" : "red"}>
                      <span className="live-dot" />
                      {reconciliation?.ok
                        ? "Operational balances checked"
                        : "Balances require review"}
                    </Badge>
                  </div>
                  <h2>{s.warehouse.name}</h2>
                  <p>
                    <MapPin size={14} />
                    {s.warehouse.city}
                  </p>
                  <div className="warehouse-map" aria-hidden="true">
                    <span className="map-dock" />
                    <div>
                      {Array.from({ length: 24 }, (_, i) => (
                        <i key={i} className={i % 7 === 0 ? "map-empty" : ""} />
                      ))}
                    </div>
                    <span className="map-route" />
                    <span className="map-point" />
                  </div>
                  <div className="warehouse-card-foot">
                    <span>
                      {s.locations.filter((l) => l.kind === "storage").length}{" "}
                      storage bins
                    </span>
                    <button
                      className="text-btn"
                      onClick={() => navigate("locations")}
                    >
                      Explore locations <ArrowUpRight size={14} />
                    </button>
                  </div>
                </section>
              </div>
              <div className="overview-grid bottom-grid">
                <Section
                  title="Active orders"
                  detail="The next moves on your warehouse floor"
                  action={
                    <button
                      className="text-btn"
                      onClick={() => navigate("fulfillment")}
                    >
                      All orders <ArrowUpRight size={15} />
                    </button>
                  }
                >
                  {orderTable(
                    s.orders
                      .filter(
                        (o) =>
                          !["shipped", "cancelled", "closed_partial"].includes(
                            o.status,
                          ),
                      )
                      .slice(0, 5),
                  )}
                </Section>
                <div className="right-stack">
                  <Section
                    title="Attention queue"
                    action={
                      <span className="count-pill">
                        {openExceptions.length + lowStock.length}
                      </span>
                    }
                  >
                    <div className="attention-list">
                      {openExceptions.slice(0, 2).map((e) => (
                        <button
                          key={e.id}
                          onClick={() => navigate("exceptions")}
                        >
                          <span className="attention-icon">
                            <AlertTriangle size={17} />
                          </span>
                          <div>
                            <strong>{e.category}</strong>
                            <p>{e.detail}</p>
                          </div>
                          <ChevronRight size={16} />
                        </button>
                      ))}
                      {lowStock.slice(0, 2).map((p) => (
                        <button
                          key={p.sku}
                          onClick={() => {
                            navigate("inventory");
                            setSearch(p.sku);
                          }}
                        >
                          <span className="attention-icon muted">
                            <Boxes size={17} />
                          </span>
                          <div>
                            <strong>{p.name}</strong>
                            <p>
                              Below reorder point of {p.reorder_point} units
                            </p>
                          </div>
                          <ChevronRight size={16} />
                        </button>
                      ))}
                      {!openExceptions.length && !lowStock.length && (
                        <div className="all-clear">
                          <CheckCheck size={22} />
                          No open issues. Your floor is clear.
                        </div>
                      )}
                    </div>
                  </Section>
                  <Section title="Latest movements">
                    <div className="recent-movements">
                      {s.movements
                        .slice(-3)
                        .reverse()
                        .map((m) => (
                          <div key={m.id}>
                            <span className={`movement-icon ${m.kind}`}>
                              <ArrowDownLeft size={16} />
                            </span>
                            <div>
                              <strong>{m.sku}</strong>
                              <small>
                                {label(m.kind)} · {m.to ?? "Carrier handoff"}
                              </small>
                            </div>
                            <b>
                              {fmt(m.quantity)}
                              <small>units</small>
                            </b>
                          </div>
                        ))}
                      {!s.movements.length && (
                        <p className="muted-text">
                          Your first stock movement will appear here.
                        </p>
                      )}
                    </div>
                  </Section>
                </div>
              </div>
            </>
          )}

          {tab !== "overview" && (
            <>
              {tab !== "operations" && tab !== "transfers" && (
                <div className="list-toolbar">
                  <div className="search-field">
                    <Search size={17} />
                    <input
                      aria-label="Search current view"
                      placeholder={`Search ${
                        tab === "activity" ? "movements" : tab
                      }…`}
                      value={search}
                      onChange={(e) => setSearch(e.target.value)}
                    />
                    {search && (
                      <button
                        className="icon-btn"
                        aria-label="Clear search"
                        onClick={() => setSearch("")}
                      >
                        <X size={14} />
                      </button>
                    )}
                  </div>
                  {[
                    "receiving",
                    "fulfillment",
                    "exceptions",
                    "activity",
                  ].includes(tab) && (
                    <div className="filter-control">
                      <Filter size={15} />
                      <select
                        aria-label="Filter by status or type"
                        value={filter}
                        onChange={(e) => setFilter(e.target.value)}
                      >
                        {opts([
                          "all",
                          ...(tab === "fulfillment"
                            ? [
                                "pending",
                                "allocated",
                                "picking",
                                "partially_shipped",
                                "closed_partial",
                                "picked",
                                "packed",
                                "shipped",
                                "cancelled",
                              ]
                            : tab === "receiving"
                              ? ["expected", "partial", "received", "cancelled"]
                              : tab === "exceptions"
                                ? ["open", "resolved"]
                                : [
                                    "receipt",
                                    "putaway",
                                    "pick",
                                    "unpick",
                                    "shipment",
                                    "count",
                                  ]),
                        ]).map((o) => (
                          <option value={o.value} key={o.value}>
                            {o.value === "all"
                              ? "All " +
                                (tab === "activity" ? "types" : "statuses")
                              : label(o.label)}
                          </option>
                        ))}
                      </select>
                    </div>
                  )}
                  <span className="toolbar-spacer" />
                  {tab === "locations" && admin && (
                    <button
                      className="btn"
                      disabled={disabled}
                      onClick={() =>
                        setModal({
                          title: "Configure warehouse facility",
                          subtitle:
                            "Use the physical facility identity. Location and stock history keep their existing warehouse ID.",
                          fields: [
                            {
                              name: "code",
                              label: "Facility code",
                              value: s.warehouse.code,
                            },
                            {
                              name: "name",
                              label: "Facility name",
                              value: s.warehouse.name,
                            },
                            {
                              name: "city",
                              label: "City / region",
                              value: s.warehouse.city,
                            },
                            {
                              name: "timezone",
                              label: "IANA timezone",
                              value: s.warehouse.timezone,
                            },
                          ],
                          submit: "Save facility",
                          build: (d) => ({ type: "update_warehouse", ...d }),
                        })
                      }
                    >
                      <Settings2 size={16} />
                      Configure facility
                    </button>
                  )}
                  {tab === "inventory" && admin && (
                    <button
                      className="btn"
                      disabled={disabled}
                      onClick={() =>
                        setModal({
                          title: "Add product",
                          subtitle:
                            "Choose serial tracking when this SKU is first introduced. The choice is shared by all facilities and cannot relabel existing inventory.",
                          fields: [
                            {
                              name: "sku",
                              label: "SKU",
                              placeholder: "SS-NEW-001",
                            },
                            { name: "name", label: "Product name" },
                            {
                              name: "serial_controlled",
                              label: "Track individual serials",
                              type: "select",
                              value: "false",
                              options: [
                                {
                                  value: "false",
                                  label: "Whole each quantities",
                                },
                                {
                                  value: "true",
                                  label: "Require each unit's literal serial",
                                },
                              ],
                            },
                            {
                              name: "category",
                              label: "Category",
                              value: "General",
                            },
                            {
                              name: "reorder_point",
                              label: "Reorder point (each)",
                              type: "number",
                              min: 0,
                              value: 20,
                            },
                          ],
                          submit: "Create product",
                          build: (d) => ({
                            type: "create_product",
                            ...d,
                            serial_controlled:
                              d.serial_controlled === "true" ? true : undefined,
                            reorder_point: Number(d.reorder_point),
                          }),
                        })
                      }
                    >
                      <Plus size={16} />
                      Add product
                    </button>
                  )}
                  {["inventory", "activity"].includes(tab) && (
                    <button
                      className="btn"
                      onClick={() =>
                        csv(
                          `${tab}.csv`,
                          tab === "inventory" ? s.stocks : s.movements,
                        )
                      }
                    >
                      <Download size={16} />
                      Export CSV
                    </button>
                  )}
                </div>
              )}
              {tab === "inventory" && (
                <>
                  {canWork && (
                    <ReplenishmentPanel
                      key={s.warehouse.id}
                      state={s}
                      disabled={disabled}
                      request={request}
                      onSubmit={submit}
                    />
                  )}
                  <SerialTracePanel
                    key={`serial-${s.warehouse.id}`}
                    state={s}
                    request={request}
                  />
                  <UnitPacksPanel
                    key={s.warehouse.id}
                    state={s}
                    role={session.identity.role}
                    disabled={disabled}
                    request={request}
                    onSubmit={submit}
                  />
                  <LotControlsPanel
                    key={s.warehouse.id}
                    state={s}
                    role={session.identity.role}
                    disabled={disabled}
                    request={request}
                    onSubmit={submit}
                  />
                  <LotTracePanel
                    key={`${session.identity.tenant}:${session.identity.actor}`}
                    state={s}
                    disabled={disabled}
                    request={request}
                  />
                  <div className="inventory-summary">
                    <span>
                      <i className="legend-dot" />
                      {s.products.length} catalog products
                    </span>
                    <span>{fmt(totals.onHand)} physical units</span>
                    <span>{fmt(totals.reserved)} reserved units</span>
                    <span>{lowStock.length} below reorder point</span>
                  </div>
                  <Section
                    title="Stock by location"
                    detail="Physical on-hand includes receiving and staged goods. Allocation requires unreserved storage stock with an available lot."
                  >
                    <div className="table-scroll">
                      <table>
                        <thead>
                          <tr>
                            <th>Product</th>
                            <th>Location / lot</th>
                            <th>On hand</th>
                            <th>Reserved</th>
                            <th>Unreserved</th>
                            <th>Lot availability</th>
                            <th>Actions</th>
                          </tr>
                        </thead>
                        <tbody>
                          {s.stocks
                            .filter((b) =>
                              matches(
                                b.sku,
                                b.location,
                                b.lot,
                                s.products.find((p) => p.sku === b.sku)?.name,
                              ),
                            )
                            .sort(
                              (a, b) =>
                                a.sku.localeCompare(b.sku) ||
                                a.location.localeCompare(b.location),
                            )
                            .map((b) => (
                              <tr key={[b.sku, b.lot, b.location].join("|")}>
                                <td>
                                  <div className="product-cell">
                                    <span className="product-thumb">
                                      <Package size={20} />
                                    </span>
                                    <div>
                                      <strong>
                                        {
                                          s.products.find(
                                            (p) => p.sku === b.sku,
                                          )?.name
                                        }
                                      </strong>
                                      <span className="subline mono">
                                        {b.sku}
                                      </span>
                                    </div>
                                  </div>
                                </td>
                                <td>
                                  <span className="bin-tag">{b.location}</span>
                                  <span className="subline mono">{b.lot}</span>
                                </td>
                                <td className="mono strong">
                                  {fmt(b.on_hand)}
                                </td>
                                <td className="mono">{fmt(b.reserved)}</td>
                                <td className="mono">
                                  {fmt(b.on_hand - b.reserved)}
                                </td>
                                <td>
                                  {
                                    availabilityLabel[
                                      availability(
                                        s,
                                        b.sku,
                                        b.lot,
                                        availabilityTime,
                                      )
                                    ]
                                  }
                                </td>
                                <td>
                                  <div className="row-actions">
                                    {canWork && b.location !== "STAGE-01" && (
                                      <button
                                        className="mini-btn"
                                        disabled={
                                          disabled || b.on_hand === b.reserved
                                        }
                                        onClick={() => moveStock(b)}
                                      >
                                        Move
                                      </button>
                                    )}
                                    {admin && b.location !== "STAGE-01" && (
                                      <button
                                        className="mini-btn"
                                        disabled={disabled}
                                        onClick={() => countStock(b)}
                                      >
                                        Count
                                      </button>
                                    )}
                                  </div>
                                </td>
                              </tr>
                            ))}
                        </tbody>
                      </table>
                      {!s.stocks.length && (
                        <Empty
                          title="No stock recorded yet"
                          detail="Create a product and receipt, then receive goods into the warehouse."
                        />
                      )}
                    </div>
                  </Section>
                  <Section
                    title="Product catalog"
                    detail="Products can exist before their first physical receipt."
                  >
                    <div className="catalog-grid">
                      {s.products
                        .filter((p) => matches(p.sku, p.name, p.category))
                        .map((p) => (
                          <div key={p.sku}>
                            <span className="product-thumb">
                              <Package size={21} />
                            </span>
                            <div>
                              <strong>{p.name}</strong>
                              <small>
                                {p.sku} · {p.category}
                              </small>
                              <small>
                                Reorder below {p.reorder_point} each
                              </small>
                            </div>
                          </div>
                        ))}
                    </div>
                  </Section>
                </>
              )}
              {tab === "fulfillment" && (
                <Section
                  title="Fulfillment orders"
                  detail={`${s.orders.length} orders · ${totals.active} in progress`}
                >
                  {orderTable(
                    s.orders.filter(
                      (o) =>
                        (filter === "all" || o.status === filter) &&
                        matches(
                          o.reference,
                          o.customer,
                          o.source_order_id,
                          o.destination,
                        ),
                    ),
                  )}
                  {historyControl("orders")}
                </Section>
              )}
              {tab === "receiving" && (
                <>
                  <div className="inventory-summary">
                    <span>{totals.receiving} inbound receipts open</span>
                    <span>
                      {fmt(
                        s.stocks
                          .filter((b) =>
                            s.locations.some(
                              (l) =>
                                l.code === b.location && l.kind === "receiving",
                            ),
                          )
                          .reduce((n, b) => n + b.on_hand, 0),
                      )}{" "}
                      units awaiting putaway
                    </span>
                  </div>
                  <Section
                    title="Inbound receipts"
                    detail="Receive against an expected line, then move the goods to storage."
                  >
                    <div className="table-scroll">
                      <table>
                        <thead>
                          <tr>
                            <th>Receipt / supplier</th>
                            <th>Lines</th>
                            <th>Received / expected</th>
                            <th>Status</th>
                            <th>Created</th>
                            <th />
                          </tr>
                        </thead>
                        <tbody>
                          {s.receipts
                            .filter(
                              (r) =>
                                (filter === "all" || r.status === filter) &&
                                matches(r.reference, r.supplier),
                            )
                            .slice()
                            .reverse()
                            .map((r) => (
                              <tr key={r.id}>
                                <td>
                                  <button
                                    className="table-link"
                                    onClick={() => setModal({ receipt: r.id })}
                                  >
                                    {r.reference}
                                  </button>
                                  <span className="subline">{r.supplier}</span>
                                </td>
                                <td>{r.lines.length}</td>
                                <td>
                                  <span className="mono">
                                    {fmt(
                                      r.lines.reduce(
                                        (n, l) => n + l.received,
                                        0,
                                      ),
                                    )}{" "}
                                    <span className="muted-text">
                                      /{" "}
                                      {fmt(
                                        r.lines.reduce(
                                          (n, l) => n + l.quantity,
                                          0,
                                        ),
                                      )}
                                    </span>
                                  </span>
                                  <div className="progress-track">
                                    <span
                                      style={{
                                        width: `${
                                          (100 *
                                            r.lines.reduce(
                                              (n, l) => n + l.received,
                                              0,
                                            )) /
                                          r.lines.reduce(
                                            (n, l) => n + l.quantity,
                                            0,
                                          )
                                        }%`,
                                      }}
                                    />
                                  </div>
                                </td>
                                <td>
                                  <Status value={r.status} />
                                </td>
                                <td className="muted-text">
                                  {when(r.created_at)}
                                </td>
                                <td>
                                  <button
                                    className="mini-btn"
                                    onClick={() => setModal({ receipt: r.id })}
                                  >
                                    {["received", "cancelled"].includes(
                                      r.status,
                                    )
                                      ? "View receipt"
                                      : "Receive goods"}
                                    <ChevronRight size={14} />
                                  </button>
                                </td>
                              </tr>
                            ))}
                        </tbody>
                      </table>
                      {!s.receipts.length && (
                        <Empty
                          title="Your receiving dock is clear"
                          detail="Schedule an inbound receipt to begin."
                        />
                      )}
                    </div>
                    {historyControl("receipts")}
                  </Section>
                </>
              )}
              {tab === "locations" && (
                <div className="locations-grid">
                  {s.locations
                    .filter((l) => matches(l.code, l.zone, l.kind))
                    .map((l) => {
                      const stock = s.stocks.filter(
                        (b) => b.location === l.code,
                      );
                      return (
                        <section key={l.code} className="location-card">
                          <div>
                            <span className={`location-icon ${l.kind}`}>
                              <Warehouse size={23} />
                            </span>
                            <Badge
                              tone={l.kind === "storage" ? "green" : "blue"}
                            >
                              {l.kind}
                            </Badge>
                          </div>
                          <h2>{l.code}</h2>
                          <p>{l.zone}</p>
                          <dl>
                            <div>
                              <dt>Physical units</dt>
                              <dd>
                                {fmt(stock.reduce((n, b) => n + b.on_hand, 0))}
                              </dd>
                            </div>
                            <div>
                              <dt>SKUs in stock</dt>
                              <dd>
                                {
                                  new Set(
                                    stock
                                      .filter((b) => b.on_hand > 0)
                                      .map((b) => b.sku),
                                  ).size
                                }
                              </dd>
                            </div>
                            <div>
                              <dt>Reserved</dt>
                              <dd>
                                {fmt(stock.reduce((n, b) => n + b.reserved, 0))}
                              </dd>
                            </div>
                          </dl>
                          <button
                            className="text-btn"
                            onClick={() => {
                              navigate("inventory");
                              setSearch(l.code);
                            }}
                          >
                            View stock <ArrowRight size={14} />
                          </button>
                        </section>
                      );
                    })}
                </div>
              )}
              {tab === "exceptions" && (
                <Section
                  title="Warehouse exceptions"
                  detail="Resolution records preserve the issue, evidence, and responsible operator."
                >
                  <div className="exception-list">
                    {s.exceptions
                      .filter(
                        (e) =>
                          (filter === "all" || e.status === filter) &&
                          matches(e.category, e.detail, e.resolution),
                      )
                      .map((e) => (
                        <article key={e.id}>
                          <span className={`exception-symbol ${e.status}`}>
                            <AlertTriangle size={20} />
                          </span>
                          <div className="exception-content">
                            <div>
                              <h3>{e.category}</h3>
                              <Status value={e.status} />
                            </div>
                            <p>{e.detail}</p>
                            <small>
                              {when(e.created_at)} · {e.actor}{" "}
                              {e.order_id && (
                                <>
                                  ·{" "}
                                  <button
                                    className="table-link"
                                    onClick={() =>
                                      setModal({ order: e.order_id! })
                                    }
                                  >
                                    {s.orders.find((o) => o.id === e.order_id)
                                      ?.reference ?? e.order_id}
                                  </button>
                                </>
                              )}
                            </small>
                            {e.resolution && (
                              <div className="resolution">
                                <CheckCheck size={16} />
                                {e.resolution}
                                <small>{e.resolved_by}</small>
                              </div>
                            )}
                          </div>
                          {e.status === "open" && canWork && (
                            <button
                              className="btn"
                              disabled={disabled}
                              onClick={() =>
                                setModal({
                                  title: "Resolve exception",
                                  subtitle:
                                    "Record what was checked or corrected. Resolution does not change inventory quantities.",
                                  fields: [
                                    {
                                      name: "resolution",
                                      label: "Resolution evidence",
                                      type: "textarea",
                                    },
                                  ],
                                  submit: "Resolve issue",
                                  build: (d) => ({
                                    type: "resolve_exception",
                                    exception_id: e.id,
                                    ...d,
                                  }),
                                })
                              }
                            >
                              Resolve <ArrowRight size={14} />
                            </button>
                          )}
                        </article>
                      ))}
                    {!s.exceptions.length && (
                      <Empty
                        title="No warehouse exceptions"
                        detail="Report observed issues here so they can be tracked and resolved."
                      />
                    )}
                  </div>
                  {historyControl("exceptions")}
                </Section>
              )}
              {tab === "activity" && (
                <>
                  <div
                    className={`reconciliation-banner ${
                      fullProofOk ? "" : "failed"
                    }`}
                  >
                    <ShieldCheck size={22} />
                    <div>
                      <strong>
                        {fullReconciliationBusy
                          ? "Checking full inventory history…"
                          : fullReconciliationError
                            ? "Full reconciliation unavailable"
                            : fullProofReady
                              ? fullProofOk
                                ? "Inventory ledger reconciles"
                                : "Full reconciliation needs review"
                              : "Full reconciliation needs refresh"}
                      </strong>
                      <p>
                        {fullProofReady
                          ? fullProofOk
                            ? `All physical balances and receipt line totals match the complete movement history. Reservations match order-line allocations. Independently checked at version ${fullProof?.version}.`
                            : `Differences were found while rebuilding the complete movement history, receipt totals, and reservations at version ${fullProof?.version}. Review the exported proof before continuing.`
                          : "This check rebuilds physical balances, received line totals, and reservations from the complete recorded history."}
                      </p>
                      {fullReconciliationError && (
                        <p role="alert">{fullReconciliationError}</p>
                      )}
                      {fullProofReady &&
                        (qualityProofRequired || qualityProof !== undefined) &&
                        (qualityProofValid && qualityProof.ok ? (
                          <p>
                            Lot control history reconciles. Current quarantine
                            and expiry retain the original handoff facts.
                          </p>
                        ) : (
                          <p role="alert">
                            Lot policy or original eligibility evidence needs
                            review. Export the proof and reconcile the affected
                            work before further fulfillment.
                          </p>
                        ))}
                      {fullProofReady &&
                        (unitProofRequired || unitProof !== undefined) &&
                        (!unitProofValid || unitProof?.ok !== true) && (
                          <p className="alert warning">
                            Original pack definitions or quantity observations
                            disagree. Hold work and review the source records
                            before recording further inventory changes.
                          </p>
                        )}
                      {packageDifferences.length > 0 && (
                        <p role="alert">
                          Shipment package evidence needs review for{" "}
                          {packageDifferences.length} order(s). Export the proof
                          and reconcile original packing and handoff records.
                        </p>
                      )}
                      {transferDifferences.length > 0 && (
                        <p role="alert">
                          Transfer evidence needs review for{" "}
                          {transferDifferences.length} handoff(s). Reconcile
                          original dispatch and arrival records in the exported
                          proof before receiving.
                        </p>
                      )}
                      {shipmentDifferences.length > 0 && (
                        <p role="alert">
                          Shipment evidence needs review for{" "}
                          {shipmentDifferences.length} order
                          {shipmentDifferences.length === 1 ? "" : "s"}. Export
                          the proof and reconcile the original handoff before
                          authorizing returns.
                        </p>
                      )}
                    </div>
                    <button
                      className="btn"
                      disabled={fullReconciliationBusy}
                      onClick={() => void runFullReconciliation(s.warehouse.id)}
                    >
                      <RefreshCw
                        size={16}
                        className={fullReconciliationBusy ? "spin" : ""}
                      />
                      Run full reconciliation
                    </button>
                    <button
                      className="btn"
                      disabled={!fullProofReady}
                      onClick={() => {
                        if (!fullProofReady || !fullProof) return;
                        const url = URL.createObjectURL(
                          new Blob([JSON.stringify(fullProof, null, 2)], {
                            type: "application/json",
                          }),
                        );
                        const link = document.createElement("a");
                        link.href = url;
                        link.download = `warehouse-reconciliation-v${fullProof.version}.json`;
                        link.click();
                        URL.revokeObjectURL(url);
                      }}
                    >
                      <Download size={16} />
                      Export proof
                    </button>
                  </div>
                  <Section
                    title="Movement ledger"
                    detail={`${s.movements.length} movements · newest first`}
                  >
                    <div className="table-scroll">
                      <table>
                        <thead>
                          <tr>
                            <th>Movement</th>
                            <th>SKU / lot</th>
                            <th>From → to</th>
                            <th>Units</th>
                            <th>Reference / reason</th>
                            <th>Actor / time</th>
                          </tr>
                        </thead>
                        <tbody>
                          {s.movements
                            .filter(
                              (m) =>
                                (filter === "all" || m.kind === filter) &&
                                matches(
                                  m.sku,
                                  m.lot,
                                  m.reference,
                                  m.reason,
                                  m.actor,
                                  m.from,
                                  m.to,
                                ),
                            )
                            .slice()
                            .reverse()
                            .map((m) => (
                              <tr key={m.id}>
                                <td>
                                  <Badge
                                    tone={
                                      m.kind === "shipment" ? "blue" : "green"
                                    }
                                  >
                                    {m.kind}
                                  </Badge>
                                  <span className="subline mono">
                                    {m.id.slice(0, 8)}
                                  </span>
                                </td>
                                <td>
                                  <strong className="mono">{m.sku}</strong>
                                  <span className="subline">{m.lot}</span>
                                </td>
                                <td>
                                  {m.from ?? "Supplier / adjustment"}
                                  <span className="subline">
                                    → {m.to ?? "Carrier / adjustment"}
                                  </span>
                                </td>
                                <td className="mono strong">
                                  {fmt(m.quantity)}
                                </td>
                                <td>
                                  {m.reference}
                                  <span className="subline">{m.reason}</span>
                                  {m.receipt_line_id && (
                                    <span className="subline">
                                      Receipt line {m.receipt_line_id}
                                    </span>
                                  )}
                                </td>
                                <td>
                                  {m.actor}
                                  <span className="subline">{when(m.at)}</span>
                                </td>
                              </tr>
                            ))}
                        </tbody>
                      </table>
                      {!s.movements.length && (
                        <Empty
                          title="No movements recorded"
                          detail="Receipts, transfers, picks, counts and shipments appear here."
                        />
                      )}
                    </div>
                    {historyControl("movements")}
                  </Section>
                  <Section
                    title="Command audit"
                    detail="Recorded warehouse actions, reasons, and responsible operators."
                  >
                    <div className="table-scroll">
                      <table>
                        <thead>
                          <tr>
                            <th>Action</th>
                            <th>Reference / evidence</th>
                            <th>Actor / time</th>
                          </tr>
                        </thead>
                        <tbody>
                          {s.activity
                            .slice()
                            .reverse()
                            .filter((item: Activity) =>
                              matches(
                                item.kind,
                                item.reference,
                                item.detail,
                                item.actor,
                              ),
                            )
                            .map((item: Activity) => (
                              <tr key={item.id}>
                                <td>{label(item.kind)}</td>
                                <td>
                                  {item.reference}
                                  <span className="subline">{item.detail}</span>
                                </td>
                                <td>
                                  {item.actor}
                                  <span className="subline">
                                    {when(item.at)}
                                  </span>
                                </td>
                              </tr>
                            ))}
                        </tbody>
                      </table>
                    </div>
                    {historyControl("activity")}
                  </Section>
                </>
              )}
              {tab === "transfers" && (
                <TransfersPanel
                  key={s.warehouse.id}
                  state={s}
                  disabled={disabled || loading}
                  canWork={canWork}
                  admin={admin}
                  request={request}
                  onSubmit={submit}
                />
              )}
              {tab === "operations" && (
                <OperationsPanel
                  key={session.identity.tenant + ":" + session.identity.actor}
                  session={session}
                  request={request}
                  navigate={navigate}
                />
              )}
              {tab === "scanner" && (
                <ScannerPanel
                  at={availabilityTime}
                  key={s.warehouse.id}
                  state={state}
                  canWork={canWork}
                  disabled={disabled || loading}
                  active
                  request={request}
                  onSubmit={submit}
                />
              )}
              <NativePanel
                key={`${session.identity.tenant}:${session.identity.actor}:${s.warehouse.id}`}
                session={session}
                state={s}
                active={tab === "integrations"}
                blocked={busy || loading || !!journal || recoveryBlocked}
                request={request}
                refresh={refreshCompleted}
                onPending={setNativeBlocked}
              />
              {tab === "integrations" && (
                <>
                  <div className="integration-cards">
                    {session.integrations.map((i) => (
                      <section key={i.id} className="integration-card">
                        <span className={`integration-icon ${i.id}`}>
                          {i.id === "oms" ? (
                            <Link2 size={24} />
                          ) : i.id === "tms" ? (
                            <Truck size={24} />
                          ) : (
                            <Boxes size={24} />
                          )}
                        </span>
                        <Badge tone={i.configured ? "green" : ""}>
                          {i.configured
                            ? "Configured receiver"
                            : "Not connected"}
                        </Badge>
                        <h2>{i.name}</h2>
                        <p>{i.role}</p>
                        <small>
                          {i.id === "oms"
                            ? "Owns commercial orders and orchestration. Receives warehouse execution events."
                            : i.id === "tms"
                              ? "Owns transportation after warehouse readiness and physical handoff."
                              : "Owns X12 document generation and transport. No direct adapter configured."}
                        </small>
                      </section>
                    ))}
                  </div>
                  <div className="alert info">
                    <Link2 size={18} />
                    <p>
                      The queue below uses the optional WMS event-receiver
                      contract. Configure a receiver that supports this contract
                      to deliver these events. Native OMS imports and TMS
                      planning use the workflows above.
                    </p>
                  </div>
                  <Section
                    title="Durable delivery queue"
                    detail="Latest 200 delivery records. Acceptance is an HTTP acknowledgment; receivers must deduplicate event IDs."
                  >
                    <div className="table-scroll">
                      <table>
                        <thead>
                          <tr>
                            <th>Event</th>
                            <th>Destination</th>
                            <th>Status</th>
                            <th>Attempts</th>
                            <th>Delivery detail</th>
                            <th />
                          </tr>
                        </thead>
                        <tbody>
                          {outbox
                            .filter((o) =>
                              matches(
                                o.type,
                                o.destination,
                                o.status,
                                o.last_error,
                              ),
                            )
                            .map((o) => (
                              <tr key={o.id}>
                                <td>
                                  <strong>{label(o.type)}</strong>
                                  <span className="subline mono">
                                    {o.event_id.slice(0, 8)}
                                  </span>
                                </td>
                                <td>{o.destination.toUpperCase()}</td>
                                <td>
                                  <Status value={o.status} />
                                </td>
                                <td>{o.attempts}</td>
                                <td className="queue-detail">
                                  {o.last_error ||
                                    (o.status === "pending"
                                      ? "Waiting for a configured receiver or next delivery turn"
                                      : "HTTP acceptance recorded")}
                                </td>
                                <td>
                                  <div className="row-actions">
                                    <button
                                      className="mini-btn"
                                      onClick={() => {
                                        void request(`/v1/events/${o.event_id}`)
                                          .then((event) => {
                                            const blob = new Blob(
                                              [JSON.stringify(event, null, 2)],
                                              { type: "application/json" },
                                            );
                                            const url =
                                              URL.createObjectURL(blob);
                                            const a =
                                              document.createElement("a");
                                            a.href = url;
                                            a.download = `${o.event_id}.json`;
                                            a.click();
                                            URL.revokeObjectURL(url);
                                          })
                                          .catch((e) => setError(e.message));
                                      }}
                                    >
                                      Event <Download size={13} />
                                    </button>
                                    {admin && o.status === "dead_letter" && (
                                      <button
                                        className="mini-btn"
                                        disabled={disabled}
                                        onClick={() => {
                                          void request(
                                            `/v1/outbox/${o.id}/retry`,
                                            {},
                                          )
                                            .then(() => refresh(s.warehouse.id))
                                            .catch((e) => setError(e.message));
                                        }}
                                      >
                                        Retry
                                      </button>
                                    )}
                                  </div>
                                </td>
                              </tr>
                            ))}
                        </tbody>
                      </table>
                      {!outbox.length && (
                        <Empty
                          title="No delivery events yet"
                          detail="New warehouse actions will create durable events. Synthetic fixture history does not enqueue deliveries."
                        />
                      )}
                    </div>
                  </Section>
                </>
              )}
            </>
          )}
          <footer className="page-footer">
            <span>
              <ShieldCheck size={14} />
              Warehouse execution by StateSet
            </span>
            <span>
              {s.warehouse.code} · Version {s.version} · {s.warehouse.timezone}
            </span>
          </footer>
        </main>
      </div>
      {toast && (
        <div className="toast" role="status">
          <CheckCheck size={18} />
          {toast}
        </div>
      )}
      {serialPreparing && !serialReview && (
        <div className="alert info" role="status">
          Resolving the original serial quantities… Nothing has been saved.
        </div>
      )}
      {serialReview && (
        <ModalFrame
          title="Observe serial labels"
          close={() => completeSerialReview(null)}
        >
          <SerialObservationForm
            key={serialReview.id}
            targets={serialReview.targets}
            operation={serialReview.operation}
            originalRequest={serialReview.originalRequest}
            onConfirm={completeSerialReview}
            onCancel={() => completeSerialReview(null)}
          />
        </ModalFrame>
      )}
      {modal && "title" in modal && (
        <ModalFrame
          suspended={serialPreparing || !!serialReview}
          title={modal.title}
          close={closeModal}
        >
          <CommandForm
            key={modal.title + JSON.stringify(modal.fields)}
            spec={modal}
            state={s}
            busy={disabled}
            submit={(command, inputs) =>
              submit(command, undefined, undefined, inputs)
            }
          />
          {error && (
            <div className="modal-error" role="alert">
              {error}
            </div>
          )}
        </ModalFrame>
      )}
      {detailKey && !selectedOrder && !selectedReceipt && (
        <ModalFrame
          suspended={serialPreparing || !!serialReview}
          title="Warehouse record"
          close={closeModal}
        >
          <div className="detail-body">
            {detailLoading ? (
              <p role="status">
                <Loader2 size={18} className="spin" /> Loading current record…
              </p>
            ) : (
              <p role="alert">{error || "This record is unavailable."}</p>
            )}
          </div>
        </ModalFrame>
      )}
      {selectedOrder && (
        <ModalFrame
          suspended={serialPreparing || !!serialReview}
          title={`${selectedOrder.reference} · ${selectedOrder.customer}`}
          close={closeModal}
        >
          <div className="detail-body">
            <div className="detail-summary">
              <Status value={selectedOrder.status} />
              <span className={`priority ${selectedOrder.priority}`}>
                <i />
                {selectedOrder.priority}
              </span>
              <span>{selectedOrder.destination}</span>
            </div>
            <div className="detail-reference">
              <span>OMS source</span>
              <code>
                {selectedOrder.source_order_id ?? "Manual warehouse work"}
              </code>
            </div>
            <h3>Order lines</h3>
            {selectedOrder.lines.map((l) => (
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
            {selectedOrder.allocations.length > 0 && (
              <>
                <h3>Reserved pick instructions</h3>
                {selectedOrder.allocations.map((a: Allocation, i: number) => (
                  <div className="allocation" key={i}>
                    <div>
                      <strong>{a.sku}</strong>
                      <small>
                        {a.location} · {a.lot} · line {a.line_id.slice(0, 8)}
                      </small>
                      <span>
                        {a.quantity > a.shipped + a.released
                          ? availabilityLabel[
                              availability(s, a.sku, a.lot, availabilityTime)
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
                        selectedOrder.status,
                      ) &&
                      a.quantity > a.picked + a.released && (
                        <button
                          className="btn"
                          disabled={
                            disabled ||
                            orderBlocked ||
                            availability(s, a.sku, a.lot, availabilityTime) !==
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
                                order_id: selectedOrder.id,
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
                      ].includes(selectedOrder.status) &&
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
                                order_id: selectedOrder.id,
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
                (e) => e.order_id === selectedOrder.id && e.status === "open",
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
            {selectedOrder.status === "shipped" && (
              <div className="shipment-proof">
                <Truck size={22} />
                <div>
                  <strong>Physical handoff recorded</strong>
                  <p>
                    {selectedOrder.carrier
                      ? `${selectedOrder.carrier} · ${selectedOrder.handoff_reference}`
                      : "Multiple handoffs — review each shipment package below"}
                  </p>
                  <small>
                    {selectedOrder.shipped_at && when(selectedOrder.shipped_at)}
                  </small>
                </div>
              </div>
            )}
            {["shipped", "partially_shipped", "closed_partial"].includes(
              selectedOrder.status,
            ) &&
              canWork && (
                <ReturnForm
                  packs={s.unit_packs}
                  warehouse={s.warehouse.id}
                  orderId={selectedOrder.id}
                  disabled={disabled}
                  request={request}
                  onSubmit={submit}
                />
              )}
            {(selectedOrder.shipment_mode === "packages" ||
              !["shipped", "cancelled", "closed_partial"].includes(
                selectedOrder.status,
              )) && (
              <ShipmentsPanel
                warehouse={s.warehouse.id}
                orderId={selectedOrder.id}
                disabled={disabled}
                forwardBlocked={orderBlocked}
                state={s}
                at={availabilityTime}
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
                  {!selectedOrder.shipment_mode &&
                    selectedOrder.status === "pending" && (
                      <button
                        className="btn primary"
                        disabled={disabled || orderBlocked}
                        onClick={() =>
                          action({
                            type: "allocate",
                            order_id: selectedOrder.id,
                          })
                        }
                      >
                        <ClipboardCheck size={16} />
                        Allocate stock
                      </button>
                    )}
                  {!selectedOrder.shipment_mode &&
                    selectedOrder.status === "allocated" &&
                    selectedOrder.allocations.every(
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
                              order_id: selectedOrder.id,
                              reason: data.reason,
                            }),
                          })
                        }
                      >
                        Release allocation
                      </button>
                    )}
                  {!selectedOrder.shipment_mode &&
                    selectedOrder.status === "picked" && (
                      <button
                        className="btn primary"
                        disabled={disabled || orderBlocked || legacyLotHeld}
                        onClick={() =>
                          action({ type: "pack", order_id: selectedOrder.id })
                        }
                      >
                        <PackageCheck size={16} />
                        Confirm packed
                      </button>
                    )}
                  {!selectedOrder.shipment_mode &&
                    selectedOrder.status === "packed" && (
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
                              order_id: selectedOrder.id,
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
                    selectedOrder.status,
                  ) && (
                    <button
                      className="btn"
                      disabled={disabled}
                      onClick={() => report(selectedOrder)}
                    >
                      Report exception
                    </button>
                  )}
                  {["pending", "allocated"].includes(selectedOrder.status) && (
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
                            order_id: selectedOrder.id,
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
      )}
      {selectedReceipt && (
        <ModalFrame
          suspended={serialPreparing || !!serialReview}
          title={`${selectedReceipt.reference} · ${selectedReceipt.supplier}`}
          close={closeModal}
        >
          <div className="detail-body">
            <div className="detail-summary">
              <Status value={selectedReceipt.status} />
              <span>Created {when(selectedReceipt.created_at)}</span>
            </div>
            {selectedReceipt.cancellation && (
              <p className="shipment-proof">
                Unreceived remainder cancelled ·{" "}
                {selectedReceipt.cancellation.reason}
                <br />
                {selectedReceipt.cancellation.actor} ·{" "}
                {when(selectedReceipt.cancellation.cancelled_at)}
              </p>
            )}
            <h3>Expected receipt lines</h3>
            {selectedReceipt.return_source && (
              <p className="shipment-proof">
                Customer return · original order{" "}
                {selectedReceipt.return_source.order_id}
                <br />
                {selectedReceipt.return_source.reason}
              </p>
            )}
            {selectedReceipt.lines.map((l) => (
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
                  {selectedReceipt.cancellation && (
                    <span>
                      {selectedReceipt.cancellation.lines[l.id]?.cancelled ?? 0}{" "}
                      cancelled
                    </span>
                  )}
                </div>
                {canWork &&
                  !selectedReceipt.cancellation &&
                  selectedReceipt.status !== "cancelled" &&
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
                            receipt_id: selectedReceipt.id,
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
              !selectedReceipt.cancellation &&
              ["expected", "partial"].includes(selectedReceipt.status) &&
              selectedReceipt.lines.some(
                (line) => line.received < line.quantity,
              ) && (
                <button
                  className="btn danger"
                  disabled={disabled}
                  onClick={() =>
                    setModal({
                      title: "Cancel unreceived remainder",
                      subtitle: `${selectedReceipt.reference}: close only goods that have not arrived. Received stock and its history stay intact. Cancelled return quantities become available for a new authorization.`,
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
                        receipt_id: selectedReceipt.id,
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
      )}
      {modal && "help" in modal && (
        <ModalFrame
          suspended={serialPreparing || !!serialReview}
          title="Your warehouse workspace"
          close={closeModal}
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
      )}
    </div>
  );
}

