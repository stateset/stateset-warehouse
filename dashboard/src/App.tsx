import { CommandForm, ModalFrame } from "./CommandDialog";
import { ApiError, provesStaleCommandDidNotCommit } from "./commandRecovery";
import { Badge, Empty, Section, Status, label } from "./WarehouseUi";
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
import { BatchPickPanel } from "./PlanningPanels";
import { availability, useAvailabilityTime } from "./lotAvailability";
import OrderDetail from "./OrderDetail";
import ReceiptDetail from "./ReceiptDetail";
import WorkspaceGuide from "./WorkspaceGuide";
import TransfersPanel from "./TransfersPanel";
import WarehousePicker from "./WarehousePicker";
import { reviewedVersions } from "./concurrency";
import { deriveProofView, type ReconciliationProof } from "./proofView";
import ActivityTab from "./ActivityTab";
import { fmt, when } from "./format";
import type { Modal, Tab } from "./uiTypes";
import InventoryTab from "./InventoryTab";
import OverviewTab from "./OverviewTab";
import MobileNavigation from "./MobileNavigation";
import { matchesOrderStatus, prioritizeOrders } from "./orderView";
import { useTheme } from "./theme";
import ReceivingTab from "./ReceivingTab";
import WorkTasksTab from "./WorkTasksTab";
import LocationsTab from "./LocationsTab";
import ExceptionsTab from "./ExceptionsTab";
import {
  Activity as ActivityIcon,
  ArrowDownToLine,
  ArrowRight,
  ArrowUpRight,
  Box,
  Boxes,
  CheckCheck,
  ChevronRight,
  CircleHelp,
  Clock3,
  Download,
  Filter,
  LayoutDashboard,
  Link2,
  Loader2,
  LogOut,
  MapPin,
  Moon,
  PackageCheck,
  Plus,
  RefreshCw,
  Search,
  ScanLine,
  Settings2,
  ShieldCheck,
  Sun,
  Truck,
  Warehouse,
  X,
  AlertTriangle,
} from "lucide-react";
import type {
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
import { validUnitInputs } from "./unitInputs";
import {
  serialCommands,
  validSerialInputs,
  validSerialTargets,
  type SerialTarget,
} from "./serialInputs";
import SerialObservationForm from "./SerialObservationForm";
import type { SerialInput } from "./types";

type HistoryState = {
  [K in HistoryKind]?: {
    items: HistoryData[K][];
    nextCursor: number | null;
    loading: boolean;
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
  { id: "work", title: "My work", icon: CheckCheck },
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
const navGroups: { title: string; items: Tab[] }[] = [
  { title: "Workspace", items: ["overview", "inventory", "locations"] },
  {
    title: "Execution",
    items: ["work", "receiving", "fulfillment", "scanner", "transfers"],
  },
  {
    title: "Control",
    items: ["exceptions", "activity", "integrations", "operations"],
  },
];
const opts = (items: string[]) =>
  items.map((value) => ({ value, label: value }));
export default function App({ auth }: { auth?: WarehouseAuth }) {
  const { theme, toggleTheme } = useTheme();
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
  const [orderSort, setOrderSort] = useState("priority");
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
    async (path: string, body?: unknown, method?: "GET" | "POST" | "PUT") => {
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
          method: method ?? (body ? "POST" : "GET"),
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
      const base = `/v1/warehouses/${encodeURIComponent(warehouse)}/reconciliation`;
      const stale = () =>
        current !== epoch.current ||
        sequence !== reconciliationSequence.current;
      try {
        let proof: ReconciliationProof | null;
        try {
          proof = await request(base);
        } catch (e) {
          // Long serial history can exceed the 10-second request deadline;
          // the same proof then runs in the background and is polled.
          if (!(e instanceof ApiError) || e.status !== 503) throw e;
          let run = await request(`${base}/runs`, undefined, "POST");
          const deadline = Date.now() + 11 * 60_000;
          while (run.status !== "succeeded" && !stale()) {
            if (run.status === "failed")
              throw new Error(run.error ?? "The reconciliation run failed.");
            if (Date.now() > deadline)
              throw new Error(
                "The reconciliation run did not finish; run it again.",
              );
            await new Promise((resolve) => setTimeout(resolve, 1000));
            run = await request(`${base}/runs/${encodeURIComponent(run.id)}`);
          }
          proof = stale() ? null : run.result;
        }
        if (proof && !stale()) setFullReconciliation({ warehouse, proof });
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
    // Internal navigation must not fire a later hashchange that clears the
    // search/filter applied by a contextual shortcut. Back/forward still uses
    // the hashchange listener above.
    if (location.hash !== `#${next}`)
      window.history.pushState(null, "", `#${next}`);
    window.scrollTo({ top: 0, behavior: "auto" });
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
    requestId?: string,
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
        request_id: requestId ?? crypto.randomUUID(),
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
  const proofView = deriveProofView(
    s,
    fullReconciliation,
    fullReconciliationBusy,
    fullReconciliationError,
  );
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
    <div
      className="table-scroll"
      role="region"
      aria-label="Order list"
      tabIndex={0}
    >
      <table className="operational-table order-table">
        <thead>
          <tr>
            <th>Order / customer</th>
            <th>Destination</th>
            <th className="numeric">Units</th>
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
                <span
                  className={`priority order-mobile-priority ${o.priority}`}
                >
                  <i />
                  {o.priority} priority
                </span>
              </td>
              <td>{o.destination}</td>
              <td className="mono numeric">
                {fmt(o.lines.reduce((n, l) => n + l.quantity, 0))}
              </td>
              <td>
                <Status value={o.status} />
                {s.exceptions.some(
                  (issue) => issue.order_id === o.id && issue.status === "open",
                ) && (
                  <span className="order-blocked">
                    <AlertTriangle size={12} /> Open issue
                  </span>
                )}
              </td>
              <td>
                <span className={`priority ${o.priority}`}>
                  <i />
                  {o.priority}
                </span>
              </td>
              <td>
                <button
                  className="mini-btn review-order"
                  aria-label={`Open ${o.reference}`}
                  onClick={() => setModal({ order: o.id })}
                >
                  <span>Review</span>
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
          detail={
            search || filter !== "all"
              ? "Try a different search or clear the filters to see loaded orders."
              : "New fulfillment work will appear here."
          }
          action={
            (search || filter !== "all") && (
              <button
                className="btn"
                onClick={() => {
                  setSearch("");
                  setFilter("all");
                }}
              >
                Clear filters
              </button>
            )
          }
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

  return (
    <div className="app-shell">
      <a
        className="skip-link"
        href="#workspace-content"
        onClick={(event) => {
          event.preventDefault();
          document.getElementById("workspace-content")?.focus();
          window.scrollTo({ top: 0, behavior: "auto" });
        }}
      >
        Skip to workspace content
      </a>
      <MobileNavigation
        items={nav.filter((item) => item.id !== "operations" || canWork)}
        groups={navGroups}
        tab={tab}
        navigate={navigate}
        openIssues={openExceptions.length}
        onHelp={() => setModal({ help: true })}
        onLogout={logout}
      />
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
        <nav aria-label="Warehouse navigation">
          {navGroups.map((group) => (
            <div className="nav-group" key={group.title}>
              <div className="nav-label">{group.title}</div>
              {group.items
                .map((id) => nav.find((n) => n.id === id)!)
                .filter((n) => n.id !== "operations" || canWork)
                .map((n) => (
                  <button
                    key={n.id}
                    aria-label={n.title}
                    title={n.title}
                    aria-current={tab === n.id ? "page" : undefined}
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
            </div>
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
            Warehouse <ChevronRight size={14} />
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
              className="icon-btn theme-toggle"
              aria-label={`Switch to ${theme === "dark" ? "light" : "dark"} theme`}
              title={`Switch to ${theme === "dark" ? "light" : "dark"} theme`}
              onClick={toggleTheme}
            >
              {theme === "dark" ? <Sun size={18} /> : <Moon size={18} />}
            </button>
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
        <main id="workspace-content" tabIndex={-1}>
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
                    work: "Your next move, with clear ownership and progress across the floor.",
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
            <OverviewTab
              availableStorage={availableStorage}
              lowStock={lowStock}
              navigate={navigate}
              openExceptions={openExceptions}
              orderTable={orderTable}
              reconciliation={reconciliation}
              s={s}
              setFilter={setFilter}
              setSearch={setSearch}
              totals={totals}
            />
          )}

          {tab !== "overview" && (
            <>
              {tab !== "operations" && tab !== "transfers" && tab !== "work" && (
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
                                "pick_queue",
                                "partially_shipped",
                                "closed_partial",
                                "picked",
                                "packed",
                                "shipped",
                                "cancelled",
                              ]
                            : tab === "receiving"
                              ? [
                                  "inbound_queue",
                                  "expected",
                                  "partial",
                                  "received",
                                  "cancelled",
                                ]
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
                              : o.value === "pick_queue"
                                ? "To pick"
                                : o.value === "inbound_queue"
                                  ? "Open receipts"
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
                <InventoryTab
                  admin={admin}
                  availabilityTime={availabilityTime}
                  canWork={canWork}
                  countStock={countStock}
                  disabled={disabled}
                  lowStock={lowStock}
                  matches={matches}
                  moveStock={moveStock}
                  clearSearch={() => setSearch("")}
                  request={request}
                  s={s}
                  session={session}
                  submit={submit}
                  totals={totals}
                />
              )}
              {tab === "fulfillment" && canWork && (
                <BatchPickPanel
                  key={`batch-${s.warehouse.id}`}
                  state={s}
                  disabled={disabled}
                  request={request}
                  onSubmit={submit}
                />
              )}
              {tab === "fulfillment" && (
                <Section
                  title="Fulfillment orders"
                  detail={`${s.orders.filter((o) => matchesOrderStatus(o, filter) && matches(o.reference, o.customer, o.source_order_id, o.destination)).length} shown · ${s.orders.length} loaded · ${totals.active} in progress`}
                  action={
                    <label className="sort-control">
                      Sort by
                      <select
                        aria-label="Sort fulfillment orders"
                        value={orderSort}
                        onChange={(e) => setOrderSort(e.target.value)}
                      >
                        <option value="priority">Priority</option>
                        <option value="newest">Newest first</option>
                        <option value="oldest">Oldest first</option>
                      </select>
                    </label>
                  }
                >
                  {orderTable(
                    (orderSort === "priority"
                      ? prioritizeOrders(s.orders)
                      : [...s.orders].sort((a, b) =>
                          orderSort === "newest"
                            ? b.created_at.localeCompare(a.created_at)
                            : a.created_at.localeCompare(b.created_at),
                        )
                    ).filter(
                      (o) =>
                        matchesOrderStatus(o, filter) &&
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
              {tab === "work" && (
                <WorkTasksTab
                  key={`work:${session.identity.tenant}:${session.identity.actor}:${s.warehouse.id}`}
                  state={s}
                  identity={session.identity}
                  request={request}
                  submit={submit}
                  disabled={disabled}
                />
              )}
              {tab === "receiving" && (
                <ReceivingTab
                  canWork={canWork}
                  disabled={disabled}
                  filter={filter}
                  historyControl={historyControl}
                  matches={matches}
                  request={request}
                  s={s}
                  setModal={setModal}
                  clearFilters={() => {
                    setSearch("");
                    setFilter("all");
                  }}
                  submit={submit}
                  totals={totals}
                />
              )}
              {tab === "locations" && (
                <LocationsTab
                  matches={matches}
                  navigate={navigate}
                  s={s}
                  setSearch={setSearch}
                />
              )}
              {tab === "exceptions" && (
                <ExceptionsTab
                  canWork={canWork}
                  disabled={disabled}
                  filter={filter}
                  historyControl={historyControl}
                  matches={matches}
                  s={s}
                  setModal={setModal}
                />
              )}
              {tab === "activity" && (
                <ActivityTab
                  s={s}
                  proof={proofView}
                  fullReconciliationBusy={fullReconciliationBusy}
                  fullReconciliationError={fullReconciliationError}
                  runFullReconciliation={runFullReconciliation}
                  filter={filter}
                  matches={matches}
                  historyControl={historyControl}
                />
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
        <OrderDetail
          at={availabilityTime}
          canWork={canWork}
          close={closeModal}
          disabled={disabled}
          error={error}
          order={selectedOrder}
          refresh={refresh}
          report={report}
          request={request}
          s={s}
          setModal={setModal}
          submit={submit}
          suspended={serialPreparing || !!serialReview}
        />
      )}
      {selectedReceipt && (
        <ReceiptDetail
          canWork={canWork}
          close={closeModal}
          disabled={disabled}
          fieldOptions={fieldOptions}
          receipt={selectedReceipt}
          s={s}
          setModal={setModal}
          suspended={serialPreparing || !!serialReview}
        />
      )}
      {modal && "help" in modal && (
        <WorkspaceGuide
          close={closeModal}
          logout={logout}
          suspended={serialPreparing || !!serialReview}
        />
      )}
    </div>
  );
}
