// Server-planned work for the terminal: putaway, batch picks, cycle counts
// and due replenishment. Plans reserve and move nothing. Each chosen task is
// the plan's exact canonical command, recorded through the same reviewed,
// journaled save as hand-entered work.
import { safeText } from './client.mjs';

export const PLANS = Object.freeze({
  putaway: { label: 'Directed putaway', method: 'POST', suffix: '/putaway/plan', body: {} },
  batch: { label: 'Batch picking', method: 'POST', suffix: '/picking/batch-plan', body: {} },
  counts: { label: 'Cycle counts due', method: 'POST', suffix: '/counts/plan', body: {} },
  replenishment: { label: 'Replenishment due', method: 'GET', suffix: '/replenishment/due' },
});

const text = value => safeText(String(value ?? ''));

/** Flatten a verified plan into menu tasks: `{ label, command, needsCount }`. */
export function planTasks(kind, plan, warehouse) {
  if (!plan || plan.warehouse_id !== warehouse || !Number.isSafeInteger(plan.version)) throw new Error('Plan could not be verified for this facility');
  if (kind === 'putaway' || kind === 'replenishment') {
    if (!Array.isArray(plan.tasks)) throw new Error('Plan has no task list');
    return plan.tasks.map(task => {
      const c = task.command;
      const why = task.reason ? ` | ${text(task.reason).replaceAll('_', ' ')}` : '';
      return { label: `${text(c.sku)} ${text(c.lot)} | ${text(c.from)} -> ${text(c.to)} | ${text(c.quantity)} each${why}`, command: c, needsCount: false };
    });
  }
  if (kind === 'batch') {
    if (!Array.isArray(plan.stops)) throw new Error('Plan has no stops');
    return plan.stops.flatMap(stop => (stop.picks ?? []).map(pick => ({
      label: `Stop ${text(stop.sequence)} ${text(stop.location)} | ${text(stop.sku)} ${text(stop.lot)} | tote ${text(pick.tote)} | ${text(pick.quantity)} each`,
      command: pick.command, needsCount: false,
    })));
  }
  if (kind === 'counts') {
    if (!Array.isArray(plan.tasks)) throw new Error('Plan has no task list');
    return plan.tasks.map(task => ({
      label: `${text(task.location)} | ${text(task.sku)} ${text(task.lot)} | class ${text(task.class)} | ${task.last_counted_at ? `${text(task.overdue_days)} days overdue` : 'never counted'}`,
      command: task.command, needsCount: true,
    }));
  }
  throw new Error('Unknown plan');
}

/** A blind count's command with the counter's observed quantity filled in. */
export function withCount(command, raw) {
  const value = String(raw ?? '').trim();
  if (!/^(0|[1-9][0-9]{0,6})$/.test(value) || Number(value) > 1_000_000) throw new Error('Enter the counted whole units (0-1,000,000)');
  return { ...command, counted: Number(value) };
}

export async function fetchPlan(client, warehouse, kind) {
  const spec = PLANS[kind];
  if (!spec) throw new Error('Unknown plan');
  const response = await client.call(spec.method, client.warehousePath(warehouse, spec.suffix), spec.body);
  if (!response.ok) throw new Error(`HTTP ${response.status}: ${text(response.data?.error ?? 'Plan unavailable')}`);
  return response.data;
}
