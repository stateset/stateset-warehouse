import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowRight, CheckCheck, ClipboardList, RefreshCw } from "lucide-react";
import { Badge, Empty, Section, label } from "./WarehouseUi";
import { fmt, when } from "./format";
import type { RequestJson, SubmitCommand } from "./uiTypes";
import type { Command, Session, WarehouseState } from "./types";

type Task = {
  id: string;
  sku: string;
  lot: string;
  from_location: string;
  quantity: number;
  completed_quantity: number;
  status: string;
  owner: string | null;
  priority: number;
  revision: number;
  reason: string | null;
  created_at: string;
  updated_at: string;
};
type Queue = {
  tasks: Task[];
  total: number;
  counts: { ready: number; mine: number; blocked: number; active: number };
};
type Review = {
  task_id: string;
  revision: number;
  reason: string;
  capacity_limited: boolean;
  request: {
    request_id: string;
    expected_version: number;
    resource_versions: Record<string, number>;
    command: Command & {
      sku: string;
      lot: string;
      from: string;
      to: string;
      quantity: number;
    };
  };
};

export default function WorkTasksTab({
  state,
  identity,
  request,
  submit,
  disabled,
}: {
  state: WarehouseState;
  identity: Session["identity"];
  request: RequestJson;
  submit: SubmitCommand;
  disabled: boolean;
}) {
  const [view, setView] = useState("mine");
  const [offset, setOffset] = useState(0);
  const [queue, setQueue] = useState<Queue | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [review, setReview] = useState<Review | null>(null);
  const [reasonFor, setReasonFor] = useState<{
    task: Task;
    action: "block" | "cancel";
  } | null>(null);
  const [reason, setReason] = useState("");
  const [scans, setScans] = useState<Record<string, string>>({});
  const [quantities, setQuantities] = useState<Record<string, string>>({});
  const [fit, setFit] = useState(false);
  const alive = useRef(true);
  const loading = useRef(0);
  const acting = useRef(false);
  const base = `/v1/warehouses/${encodeURIComponent(state.warehouse.id)}/work`;
  const admin = identity.role === "admin";
  const worker = admin || identity.role === "operator";
  const locked = disabled || busy;
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      loading.current++;
    };
  }, []);
  const load = useCallback(async () => {
    const sequence = ++loading.current;
    try {
      const data: Queue = await request(
        `${base}?view=${view}&offset=${offset}`,
      );
      if (alive.current && sequence === loading.current) setQueue(data);
    } catch (e) {
      if (alive.current && sequence === loading.current)
        setError((e as Error).message);
    }
  }, [request, base, view, offset]);
  useEffect(() => {
    setQueue(null);
    void load();
  }, [load]);
  useEffect(() => {
    if (locked || review || reasonFor) return;
    const timer = window.setInterval(() => void load(), 15000);
    return () => window.clearInterval(timer);
  }, [load, locked, review, reasonFor]);
  const run = async (operation: () => Promise<void>) => {
    if (locked || acting.current) return;
    acting.current = true;
    setBusy(true);
    setError("");
    setMessage("");
    try {
      await operation();
    } catch (e) {
      if (alive.current)
        setError(`${(e as Error).message} Refresh to check the current task.`);
    } finally {
      if (alive.current) {
        setBusy(false);
        await load();
      }
      acting.current = false;
    }
  };
  const action = (task: Task, action: string, extra = {}) =>
    run(async () => {
      await request(`${base}/${encodeURIComponent(task.id)}/action`, {
        revision: task.revision,
        action,
        ...extra,
      });
      if (!alive.current) return;
      setReasonFor(null);
      setReason("");
      setMessage(`Work ${label(action)} saved.`);
    });
  const begin = (task: Task) =>
    run(async () => {
      const next: Review = await request(
        `${base}/${encodeURIComponent(task.id)}/review`,
        {
          revision: task.revision,
          quantity: Number(
            quantities[task.id] ??
              Math.min(task.quantity - task.completed_quantity, 100),
          ),
        },
      );
      if (!alive.current) return;
      setScans({});
      setFit(false);
      setReview(next);
    });
  const move = review?.request.command;
  const observations = move
    ? [
        ["from", "Source bin", move.from],
        ["sku", "Product SKU", move.sku],
        ["lot", "Lot", move.lot],
        ["to", "Destination bin", move.to],
        ["quantity", "Units moved", String(move.quantity)],
      ]
    : [];
  const observed = observations.every(([key, , value]) => scans[key] === value);
  return (
    <div className="work-space">
      <div className="work-metrics" aria-label="Putaway work summary">
        {[
          ["Available", queue?.counts.ready],
          ["Assigned to you", queue?.counts.mine],
          ["Blocked", queue?.counts.blocked],
        ].map(([name, value]) => (
          <div key={name}>
            <span>{name}</span>
            <strong>{value === undefined ? "—" : fmt(Number(value))}</strong>
          </div>
        ))}
      </div>
      <Section
        title="Putaway work"
        detail="Claim a task, check the labels, and confirm the physical move. New receipts appear here automatically."
        action={
          <div className="work-actions">
            <button
              className="btn"
              disabled={locked || !!review}
              onClick={() => {
                setError("");
                void load();
              }}
            >
              <RefreshCw size={15} />
              Refresh work
            </button>
            {worker && (
              <button
                className="btn"
                disabled={locked || !!review}
                onClick={() =>
                  void run(async () => {
                    const result = await request(
                      `${base}/discover`,
                      {},
                      "POST",
                    );
                    if (alive.current) {
                      setView("available");
                      setOffset(0);
                      setMessage(
                        `${result.created} task(s) created from unassigned receiving stock.${result.created === result.limit ? " Run again to find more." : ""}`,
                      );
                    }
                  })
                }
              >
                <ClipboardList size={15} />
                Find existing stock
              </button>
            )}
          </div>
        }
      >
        <div className="work-body">
          <div className="work-tabs" role="group" aria-label="Work queue">
            {[
              ["mine", "My queue"],
              ["available", "Available"],
              ["all", "Team board"],
              ["completed", "Closed"],
            ].map(([id, title]) => (
              <button
                key={id}
                className={`btn ${view === id ? "active" : ""}`}
                aria-pressed={view === id}
                disabled={locked || !!review}
                onClick={() => {
                  setView(id);
                  setOffset(0);
                  setReasonFor(null);
                }}
              >
                {title}
              </button>
            ))}
          </div>
          {error && (
            <p role="alert" className="work-error">
              {error}
            </p>
          )}
          {message && (
            <p role="status" className="work-notice">
              {message}
            </p>
          )}
          {review && move && (
            <form
              className="work-review"
              aria-label="Confirm putaway"
              onSubmit={(e) => {
                e.preventDefault();
                if (!observed || !fit) return;
                void run(async () => {
                  const original = review.request;
                  const ok = await submit(
                    original.command,
                    original.expected_version,
                    original.resource_versions,
                    undefined,
                    original.request_id,
                  );
                  if (alive.current) {
                    setReview(null);
                    setScans({});
                    setFit(false);
                    if (ok) {
                      setQuantities((previous) => {
                        const next = { ...previous };
                        delete next[review.task_id];
                        return next;
                      });
                      setMessage(
                        "Putaway confirmed. Inventory and task progress are saved.",
                      );
                    }
                  }
                });
              }}
            >
              <div className="work-review-heading">
                <div>
                  <span className="eyebrow">CHECK THE PHYSICAL MOVE</span>
                  <h3>
                    {move.from} <ArrowRight size={17} /> {move.to}
                  </h3>
                </div>
                <Badge tone="green">{fmt(move.quantity)} each</Badge>
              </div>
              <p>
                {move.sku} · Lot {move.lot} · {label(review.reason)}
              </p>
              <p>
                {review.capacity_limited
                  ? "Configured unit and SKU limits were checked."
                  : "No unit limit is configured for this destination."}{" "}
                Confirm space and safe physical fit before moving.
              </p>
              <div className="work-scans">
                {observations.map(([key, title, value], index) => (
                  <label key={key}>
                    {title}
                    <span className="work-expected">Expected: {value}</span>
                    <input
                      autoFocus={index === 0}
                      required
                      autoComplete="off"
                      inputMode={key === "quantity" ? "numeric" : "text"}
                      aria-label={`Confirm ${title.toLowerCase()}`}
                      disabled={locked}
                      value={scans[key] ?? ""}
                      onChange={(e) =>
                        setScans((old) => ({ ...old, [key]: e.target.value }))
                      }
                    />
                  </label>
                ))}
              </div>
              <label className="work-fit">
                <input
                  type="checkbox"
                  checked={fit}
                  disabled={locked}
                  onChange={(e) => setFit(e.target.checked)}
                />
                I checked physical fit and moved these units to the destination.
              </label>
              <div className="work-actions">
                <button
                  className="btn primary"
                  type="submit"
                  disabled={locked || !observed || !fit}
                >
                  <CheckCheck size={16} />
                  Confirm putaway
                </button>
                <button
                  className="btn"
                  type="button"
                  disabled={locked}
                  onClick={() => {
                    setReview(null);
                    void load();
                  }}
                >
                  Close review
                </button>
              </div>
            </form>
          )}
          {!queue && <p role="status">Loading work…</p>}
          {queue?.tasks.length === 0 && (
            <Empty
              title={
                view === "mine"
                  ? "Your queue is clear"
                  : "No tasks in this view"
              }
              detail={
                view === "mine"
                  ? "Open Available to claim putaway work. Each confirmed receipt creates a task."
                  : "Use Find existing stock to create tasks for stock already waiting in receiving."
              }
              action={
                view === "mine" && (
                  <button
                    className="btn"
                    onClick={() => {
                      setView("available");
                      setOffset(0);
                    }}
                  >
                    Browse available work
                  </button>
                )
              }
            />
          )}
          <div className="work-list">
            {queue?.tasks.map((task) => {
              const own = task.owner === identity.actor;
              const open = !["completed", "cancelled"].includes(task.status);
              const actionDisabled = locked || !!review;
              const quantity =
                quantities[task.id] ??
                String(Math.min(task.quantity - task.completed_quantity, 100));
              const validQuantity =
                Number.isInteger(Number(quantity)) &&
                Number(quantity) > 0 &&
                Number(quantity) <=
                  Math.min(10_000, task.quantity - task.completed_quantity);
              return (
                <article
                  className="work-card"
                  key={task.id}
                  aria-label={`Putaway ${task.sku} lot ${task.lot}`}
                >
                  <div className="work-card-top">
                    <div>
                      <h3>{task.sku}</h3>
                      <p>
                        Lot {task.lot} · From {task.from_location}
                      </p>
                    </div>
                    <div className="work-actions">
                      {task.priority > 0 && (
                        <Badge tone="amber">
                          {task.priority === 2 ? "Urgent" : "High priority"}
                        </Badge>
                      )}
                      <Badge
                        tone={
                          task.status === "blocked"
                            ? "red"
                            : task.status === "completed"
                              ? "green"
                              : "blue"
                        }
                      >
                        {label(task.status)}
                      </Badge>
                    </div>
                  </div>
                  <div className="work-progress">
                    <strong>
                      {fmt(task.quantity - task.completed_quantity)} each
                      remaining
                    </strong>
                    <span>
                      {fmt(task.completed_quantity)} / {fmt(task.quantity)} put
                      away
                    </span>
                  </div>
                  <progress
                    max={task.quantity}
                    value={task.completed_quantity}
                    aria-label={`Putaway progress for ${task.sku}`}
                  />
                  <div className="work-meta">
                    <span>
                      {task.owner ? `Owner: ${task.owner}` : "Unassigned"}
                    </span>
                    <span>Created {when(task.created_at)}</span>
                  </div>
                  {task.reason && <p className="work-reason">{task.reason}</p>}
                  {worker && open && !review && (
                    <div className="work-actions">
                      {task.status === "ready" && (
                        <button
                          className="btn primary"
                          disabled={actionDisabled}
                          onClick={() => void action(task, "claim")}
                        >
                          Claim task
                        </button>
                      )}
                      {own && task.status === "claimed" && (
                        <>
                          <label className="work-priority">
                            Units this move
                            <input
                              type="number"
                              min={1}
                              max={Math.min(
                                10_000,
                                task.quantity - task.completed_quantity,
                              )}
                              step={1}
                              aria-label={`Units this move for ${task.sku} lot ${task.lot}`}
                              disabled={actionDisabled}
                              value={quantity}
                              onChange={(e) =>
                                setQuantities((old) => ({
                                  ...old,
                                  [task.id]: e.target.value,
                                }))
                              }
                            />
                          </label>
                          <button
                            className="btn primary"
                            disabled={actionDisabled || !validQuantity}
                            onClick={() => void begin(task)}
                          >
                            Review putaway
                          </button>
                          <button
                            className="btn"
                            disabled={actionDisabled}
                            onClick={() => void action(task, "pause")}
                          >
                            Pause
                          </button>
                        </>
                      )}
                      {own && ["paused", "blocked"].includes(task.status) && (
                        <button
                          className="btn primary"
                          disabled={actionDisabled}
                          onClick={() => void action(task, "resume")}
                        >
                          Resume
                        </button>
                      )}
                      {own && ["claimed", "paused"].includes(task.status) && (
                        <button
                          className="btn"
                          disabled={actionDisabled}
                          onClick={() => {
                            setReasonFor({ task, action: "block" });
                            setReason("");
                          }}
                        >
                          Block
                        </button>
                      )}
                      {(own || admin) && task.status !== "ready" && (
                        <button
                          className="btn"
                          disabled={actionDisabled}
                          onClick={() => void action(task, "release")}
                        >
                          Release
                        </button>
                      )}
                      {admin && (
                        <>
                          <label className="work-priority">
                            Priority
                            <select
                              aria-label={`Priority for ${task.sku} lot ${task.lot}`}
                              value={task.priority}
                              disabled={actionDisabled}
                              onChange={(e) =>
                                void action(task, "priority", {
                                  priority: Number(e.target.value),
                                })
                              }
                            >
                              <option value={0}>Normal</option>
                              <option value={1}>High</option>
                              <option value={2}>Urgent</option>
                            </select>
                          </label>
                          <button
                            className="btn"
                            disabled={actionDisabled}
                            onClick={() => {
                              setReasonFor({ task, action: "cancel" });
                              setReason("");
                            }}
                          >
                            Cancel work
                          </button>
                        </>
                      )}
                    </div>
                  )}
                  {reasonFor?.task.id === task.id && (
                    <form
                      className="work-reason-form"
                      onSubmit={(e) => {
                        e.preventDefault();
                        void action(reasonFor.task, reasonFor.action, {
                          reason,
                        });
                      }}
                    >
                      <label>
                        Reason to {reasonFor.action} this work
                        <textarea
                          aria-label="Work reason"
                          required
                          maxLength={500}
                          disabled={locked}
                          value={reason}
                          onChange={(e) => setReason(e.target.value)}
                        />
                      </label>
                      <p>Inventory quantities stay unchanged.</p>
                      <div className="work-actions">
                        <button
                          className="btn"
                          disabled={locked || !reason.trim()}
                          type="submit"
                        >
                          Save reason
                        </button>
                        <button
                          className="btn"
                          type="button"
                          disabled={locked}
                          onClick={() => setReasonFor(null)}
                        >
                          Keep working
                        </button>
                      </div>
                    </form>
                  )}
                </article>
              );
            })}
          </div>
          {queue && queue.total > 50 && (
            <div className="work-pagination">
              <button
                className="btn"
                disabled={locked || !!review || offset === 0}
                onClick={() => setOffset(Math.max(0, offset - 50))}
              >
                Previous
              </button>
              <span>
                {offset + 1}–{Math.min(offset + 50, queue.total)} of{" "}
                {fmt(queue.total)}
              </span>
              <button
                className="btn"
                disabled={locked || !!review || offset + 50 >= queue.total}
                onClick={() => setOffset(offset + 50)}
              >
                Next
              </button>
            </div>
          )}
        </div>
      </Section>
    </div>
  );
}
