import test from 'node:test';
import assert from 'node:assert/strict';
import { Observations, observationSession } from '../lib/scanning.mjs';

const target = (name, quantity, sku = 'SKU') => ({ target: [name], sku, each_quantity: quantity });
function scripted(values, prompts = []) {
  return async prompt => { prompts.push(prompt); assert.ok(values.length, 'unexpected extra observation'); return values.shift(); };
}

test('duplicate and malformed scans retain earlier observations and exact case until corrected', async () => {
  const session = new Observations([target('Original-Line', 2)]);
  const notices = []; const states = [];
  const inputs = await session.capture(scripted(['Original', 'Original', ' invalid ', '', 'original']), { retry: true, notice: text => notices.push(text), progress: state => states.push(state) });
  assert.deepEqual(inputs, [{ target: ['Original-Line'], sku: 'SKU', serials: ['Original', 'original'] }]);
  assert.equal(notices.length, 3);
  assert.match(notices[0], /Duplicate/);
  assert.equal(states.at(-1).observed, 1);
  assert.equal(states.at(-1).required, 2);
  assert.deepEqual(states.at(-1).latest, [{ sku: 'SKU', serial: 'Original' }]);
});

test('undo at a later group returns to the original previous target without losing other observations', async () => {
  const session = new Observations([target('Line-A', 2), target('Line-B', 1)]);
  const prompts = [];
  const inputs = await session.capture(scripted(['First', 'Mistake', { key: 'undo' }, 'Corrected', 'Third'], prompts), { retry: true });
  assert.deepEqual(inputs.map(input => input.serials), [['First', 'Corrected'], ['Third']]);
  assert.ok(prompts[3].includes('["Line-A"] label 2/2'));
  assert.ok(prompts[4].includes('["Line-B"] label 1/1'));
});

test('correction from complete unsent review reopens only the last observation and protects returned copies', async () => {
  const session = new Observations([target('Line-A', 2), target('Line-B', 1)]);
  await session.capture(scripted(['First', 'Second', 'Wrong-last']));
  const old = session.inputs; old[0].serials[0] = 'cannot-rewrite-session';
  assert.equal(session.undo(), true);
  const prompts = [];
  const corrected = await session.capture(scripted(['Actual-last'], prompts));
  assert.equal(prompts.length, 1);
  assert.ok(prompts[0].includes('["Line-B"]'));
  assert.deepEqual(corrected.map(input => input.serials), [['First', 'Second'], ['Actual-last']]);
  assert.ok(!JSON.stringify(corrected).includes('Wrong-last'));
});

test('literal command-like labels stay literal and duplicate observations are scoped by exact SKU', async () => {
  const session = new Observations([target('Line-A', 3, 'SKU'), target('Line-B', 1, 'sku')]);
  const result = await session.capture(scripted(['undo', '/undo', 'F2', 'undo']));
  assert.deepEqual(result.map(input => input.serials), [['undo', '/undo', 'F2'], ['undo']]);
});

test('zero targets require explicit physical confirmation again after undo and EOF never returns partial capture', async () => {
  const session = new Observations([target('Original-Zero', 0)]);
  assert.equal(session.undo(), false);
  await assert.rejects(session.capture(scripted(['no'])), /not physically confirmed/);
  assert.deepEqual(await session.capture(scripted(['yes'])), [{ target: ['Original-Zero'], sku: 'SKU', serials: [] }]);
  assert.equal(session.undo(), true);
  const notices = [];
  await session.capture(scripted(['no', 'yes']), { retry: true, notice: text => notices.push(text) });
  assert.equal(notices.length, 1);
  const incomplete = new Observations([target('Original-Line', 2)]);
  await assert.rejects(incomplete.capture(scripted(['First', null])), /Input ended/);
});

test('the complete original plan is validated before asking for any observations', async () => {
  for (const change of [
    { required_labels: 3 },
    { required_labels: -1 },
    { targets: [target('Line', 1), target('Line', 1)] },
    { targets: [target('Line', -2)] },
    { observed_warehouse_version: 8 },
  ]) {
    const client = { warehousePath: () => '/v1/plan', call: async (_method, _path, request) => ({ ok: true, data: { plan_id: request.plan_id, warehouse_id: 'wh', command_type: 'move_stock', observed_warehouse_version: 7, required_labels: 2, targets: [target('Line', 2)], ...change } }) };
    await assert.rejects(observationSession(client, 'wh', { version: 7 }, { type: 'move_stock' }));
  }
  assert.throws(() => new Observations([target('Too-large', 10001)]), /Malformed/);
});
