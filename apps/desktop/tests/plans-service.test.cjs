const { test } = require('node:test');
const assert = require('node:assert/strict');
const { randomUUID } = require('node:crypto');
const { createPlansService } = require('../electron/plans.cjs');
const projectId = randomUUID(), runId = randomUUID();
function fixture() {
  const calls = [], run = { id: runId, project_id: projectId, version: 2, status: 'planned', request: { request: 'Bibliothèque synthétique', plan_only: true }, calls: [], results: {}, plan: { objective: 'Gestion des livres', missing_capabilities: [], tasks: [] }, snapshot: { private: 'Do not forward' } };
  let fail;
  const service = createPlansService({ async ensureSession() { calls.push('authenticate'); }, async jsonRequest(path, options) {
    calls.push({ path, ...options });
    if (fail) throw fail;
    return { data: path.endsWith('/capabilities') ? { configured: true, roles: [] } : path.endsWith('/budget') ? { currency: 'USD', unit_scale: 1e9, spent_units: 2, reserved_units: 3, limit_units: 7 } : path === `/v1/projects/${projectId}` ? { project: { current_revision: 4 } } : run };
  } });
  return { service, calls, run, fail(error) { fail = error; }, input: { projectId, key: randomUUID(), revision: 4, request: 'Bibliothèque synthétique', contextBytes: 16384 } };
}
test('planning is read-only; retry keeps key and original AppSpec revision without changing grants/budget', async () => {
  const f = fixture();
  const run = await f.service.plansStart(f.input);
  assert.equal(run.status, 'planned'); assert.equal(run.snapshot, undefined);
  await f.service.plansStart(f.input);
  const commands = f.calls.filter(value => value?.method);
  assert.equal(commands.length, 2);
  for (const command of commands) {
    assert.equal(command.headers['idempotency-key'], f.input.key);
    assert.equal(command.headers['if-match'], '"rev-4"');
    assert.equal(command.body.plan_only, true);
    assert.equal(command.path, `/v1/projects/${projectId}/plans`);
    assert.equal(command.body.limits.context_bytes, 16384);
  }
});
test('execute and cancel use run version; forbidden/stale errors stay visible and are never retried', async () => {
  const f = fixture(), input = { projectId, runId, version: 7 };
  await f.service.plansExecute(input); await f.service.plansCancel(input);
  assert.deepEqual(f.calls.filter(value => value?.method).map(value => [value.method, value.headers['if-match']]), [['POST', '"rev-7"'], ['DELETE', '"rev-7"']]);
  f.fail(Object.assign(new Error('Refus'), { code: 'forbidden' }));
  await assert.rejects(f.service.plansStart(f.input), error => error.code === 'forbidden');
  f.fail(new Error('Lost response'));
  await assert.rejects(f.service.plansStart(f.input), error => error.code === 'transport_unknown');
});
test('paths, limits and extra authority fields are rejected before authentication', async () => {
  for (const input of [{ projectId: '../secret' }, { runId: '../execute' }, { runId: [runId] }, { version: 0 }, { access: 'full' }]) {
    const f = fixture();
    await assert.rejects(f.service.plansExecute({ projectId, runId, version: 1, ...input }));
    assert.equal(f.calls.length, 0);
  }
  for (const change of [{ model: 'forged' }, { key: [randomUUID()] }, { contextBytes: 2e6 }, { revision: -1 }, { request: 'é'.repeat(4097) }]) {
    const f = fixture(); await assert.rejects(f.service.plansStart({ ...f.input, ...change })); assert.equal(f.calls.length, 0);
  }
});
test('a new project revision zero is valid independently of positive run versions', async () => {
  const f = fixture(); await f.service.plansStart({ ...f.input, revision: 0 });
  assert.equal(f.calls.find(value => value?.method).headers['if-match'], '"rev-0"');
});
test('inventory uses authoritative capabilities and project budget without funding it', async () => {
  const f = fixture(), status = await f.service.plansStatus(projectId);
  assert.equal(status.configured, true); assert.equal(status.revision, 4);
  assert.deepEqual(status.budget, { currency: 'USD', scale: 1e9, spent: 2, reserved: 3, limit: 7 });
  assert.equal(f.calls.some(value => value?.method), false);
});
