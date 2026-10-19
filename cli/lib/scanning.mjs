import { randomUUID } from 'node:crypto';
import { safeText } from './client.mjs';
import { needsSerialPlan } from './commands.mjs';

export class Observations {
  #targets; #steps; #position = 0; #inputs; #seen = new Set(); #observed = 0; #message = '';
  constructor(targets, requiredLabels) {
    if (!Array.isArray(targets) || targets.length > 100) throw new Error('Malformed observation target set');
    let total = 0; const seen = new Set();
    for (const target of targets) {
      if (!Number.isSafeInteger(target.each_quantity) || target.each_quantity < 0 || total + target.each_quantity > 10000 || !Array.isArray(target.target) || target.target.length > 3 || !target.target.every(value => typeof value === 'string') || typeof target.sku !== 'string' || !target.sku) throw new Error('Malformed observation target');
      const key = JSON.stringify(target.target);
      if (seen.has(key)) throw new Error('Duplicate original observation target');
      seen.add(key); total += target.each_quantity;
    }
    if (requiredLabels !== undefined && total !== requiredLabels) throw new Error('Observation plan total disagrees');
    this.#targets = structuredClone(targets);
    this.#inputs = targets.map(target => ({ target: [...target.target], sku: target.sku, serials: [] }));
    this.#steps = targets.flatMap((target, index) => Array.from({ length: Math.max(1, target.each_quantity) }, (_, label) => ({ index, label, zero: target.each_quantity === 0 })));
  }
  get canUndo() { return this.#position > 0; }
  get inputs() { return structuredClone(this.#inputs); }
  undo() {
    if (!this.canUndo) { this.#message = 'No earlier observation to undo.'; return false; }
    const step = this.#steps[--this.#position]; const input = this.#inputs[step.index];
    if (!step.zero) { const label = input.serials.pop(); this.#seen.delete(JSON.stringify([input.sku, label])); this.#observed--; }
    this.#message = 'Previous observation removed. Observe this target again.';
    return true;
  }
  async capture(ask, { retry = false, progress = () => {}, notice = () => {} } = {}) {
    const required = this.#targets.reduce((total, target) => total + target.each_quantity, 0);
    while (this.#position < this.#steps.length) {
      const step = this.#steps[this.#position]; const target = this.#targets[step.index];
      const latest = [];
      for (let index = this.#inputs.length - 1; index >= 0 && latest.length < 3; index--) {
        const input = this.#inputs[index];
        latest.unshift(...input.serials.slice(-(3 - latest.length)).map(serial => ({ sku: input.sku, serial })));
      }
      progress({ observed: this.#observed, required, target: structuredClone(target), targetIndex: step.index, targetCount: this.#targets.length, labelIndex: step.label, latest, message: this.#message });
      this.#message = '';
      const answer = await ask(step.zero
        ? `Confirm physically observed zero labels for ${safeText(target.sku)} ${safeText(JSON.stringify(target.target))} (yes/no): `
        : `Scan actual ${safeText(target.sku)} ${safeText(JSON.stringify(target.target))} label ${step.label + 1}/${target.each_quantity}: `);
      if (answer === null) throw new Error('Input ended; no request sent');
      if (answer && typeof answer === 'object' && answer.key === 'undo') { this.undo(); continue; }
      let error;
      if (step.zero) {
        if (answer !== 'yes') error = 'Empty count was not physically confirmed';
      } else if (typeof answer !== 'string' || !answer || answer.trim() !== answer || Buffer.byteLength(answer) > 240 || /[\x00-\x1f\x7f-\x9f]/.test(answer)) error = 'Invalid literal label; use 1–240 UTF-8 bytes without surrounding spaces or controls';
      else if (this.#seen.has(JSON.stringify([target.sku, answer]))) error = 'Duplicate literal serial observation for this SKU; scan a different actual label';
      if (error) {
        if (!retry) throw new Error(`${error}; no request sent`);
        this.#message = error; notice(error); continue;
      }
      if (!step.zero) {
        this.#seen.add(JSON.stringify([target.sku, answer])); this.#inputs[step.index].serials.push(answer); this.#observed++;
      }
      this.#position++;
    }
    return this.inputs;
  }
}

export async function observationSession(client, warehouse, snapshot, command) {
  if (!needsSerialPlan.has(command.type)) return new Observations([]);
  const planId = randomUUID();
  const reply = await client.call('POST', client.warehousePath(warehouse, '/serial-observation-plan'), { plan_id: planId, command });
  if (!reply.ok) throw new Error(`Observation plan held: HTTP ${reply.status}; ${safeText(reply.data.error ?? 'unknown error')}`);
  const plan = reply.data;
  if (plan.plan_id !== planId || plan.warehouse_id !== warehouse || plan.command_type !== command.type || plan.observed_warehouse_version !== snapshot.version || !Array.isArray(plan.targets) || plan.targets.length > 100 || !Number.isSafeInteger(plan.required_labels) || plan.required_labels < 0 || plan.required_labels > 10000) throw new Error('Observation plan changed or does not match this reviewed work; review again');
  return new Observations(plan.targets, plan.required_labels);
}
export async function collectSerials(client, warehouse, snapshot, command, ask) {
  return (await observationSession(client, warehouse, snapshot, command)).capture(ask);
}
