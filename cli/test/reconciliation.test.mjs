import test from 'node:test';
import assert from 'node:assert/strict';
import { Client } from '../lib/client.mjs';

const proof = { ok: true, version: 7084, method: 'full_append_only_ledger_and_order_line_allocations' };
function server(routes) {
  const calls = [];
  const fetchImpl = async (url, options) => {
    const key = `${options.method} ${new URL(url).pathname}`;
    calls.push(key);
    const next = routes[key];
    assert.ok(next, `unexpected request ${key}`);
    const [status, body] = typeof next === 'function' ? next() : next;
    return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
  };
  return { client: new Client('http://127.0.0.1:8088', 'synthetic-test-token', { fetchImpl }), calls };
}
const base = '/v1/warehouses/Original-Facility/reconciliation';
const instant = { pollMs: 0, sleep: async () => {} };

test('reconciliation returns the synchronous proof when it fits the request deadline', async () => {
  const { client, calls } = server({ [`GET ${base}`]: [200, proof] });
  assert.deepEqual(await client.reconciliation('Original-Facility', instant), proof);
  assert.deepEqual(calls, [`GET ${base}`]);
});

test('past the request deadline the same proof runs in the background and is polled', async () => {
  let polls = 0;
  const { client, calls } = server({
    [`GET ${base}`]: [503, { error: 'Database operation unavailable; retry with the same request ID' }],
    [`POST ${base}/runs`]: [202, { id: 'run-1', status: 'queued' }],
    [`GET ${base}/runs/run-1`]: () => (++polls < 3 ? [200, { id: 'run-1', status: 'running' }] : [200, { id: 'run-1', status: 'succeeded', version: 7084, result: proof }]),
  });
  assert.deepEqual(await client.reconciliation('Original-Facility', instant), proof);
  assert.equal(polls, 3);
  assert.deepEqual(calls.slice(0, 2), [`GET ${base}`, `POST ${base}/runs`]);
});

test('a failed run, a refused start and other errors are reported, never retried as success', async () => {
  const failed = server({
    [`GET ${base}`]: [503, { error: 'unavailable' }],
    [`POST ${base}/runs`]: [202, { id: 'run-2', status: 'queued' }],
    [`GET ${base}/runs/run-2`]: [200, { id: 'run-2', status: 'failed', error: 'Abandoned: the run did not finish before its deadline' }],
  });
  await assert.rejects(failed.client.reconciliation('Original-Facility', instant), /Reconciliation run failed: Abandoned/);
  const limited = server({
    [`GET ${base}`]: [503, { error: 'unavailable' }],
    [`POST ${base}/runs`]: [429, { error: 'Two reconciliation runs are already in progress for this warehouse; poll them' }],
  });
  await assert.rejects(limited.client.reconciliation('Original-Facility', instant), /HTTP 429: Two reconciliation runs/);
  const forbidden = server({ [`GET ${base}`]: [403, { error: 'Forbidden' }] });
  await assert.rejects(forbidden.client.reconciliation('Original-Facility', instant), /HTTP 403/);
  assert.deepEqual(forbidden.calls, [`GET ${base}`], 'only the request deadline falls back to a run');
});

test('a run that never finishes stops at the wait limit with its run ID', async () => {
  const { client } = server({
    [`GET ${base}`]: [503, { error: 'unavailable' }],
    [`POST ${base}/runs`]: [202, { id: 'run-3', status: 'queued' }],
    [`GET ${base}/runs/run-3`]: [200, { id: 'run-3', status: 'running' }],
  });
  await assert.rejects(client.reconciliation('Original-Facility', { pollMs: 0, waitMs: 5, sleep: () => new Promise(resolve => setTimeout(resolve, 2)) }), /run-3 did not finish/);
});
