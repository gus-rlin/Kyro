const { test } = require('node:test');
const assert = require('node:assert/strict');
const { mkdtemp, writeFile, readFile, rm } = require('node:fs/promises');
const { join } = require('node:path');
const { tmpdir } = require('node:os');
const { createHash, randomUUID } = require('node:crypto');
const { createChatService } = require('../electron/chat.cjs');

async function fixture(t, { standard = false, allowUnknown = !standard } = {}) {
  const stateDir = await mkdtemp(join(tmpdir(), 'kyro-chat-service-'));
  t.after(() => rm(stateDir, { recursive: true, force: true }));
  const model = 'nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B', projectId = randomUUID();
  const vault = Buffer.from('synthetic encrypted vault placeholder');
  const proof = { checked_at: new Date().toISOString(), model, endpoint: 'https://api.tokenfactory.nebius.com/v1/', source: 'https://docs.nebius.com/legal/token-factory', max_completion_tokens_includes_reasoning: true, vault_sha256: createHash('sha256').update(vault).digest('hex'), zero_retention_confirmed: !standard, account_evidence: 'Synthetic test only', ...(standard ? { retention_mode: 'provider_standard', standard_retention_accepted: true, consent_reference: 'Synthetic consent' } : {}) };
  const destination = { id: 'nebius-chat', provider: 'nebius', kind: 'cloud', base_url: proof.endpoint, qualified: true, retention_seconds: standard ? null : 0, nebius: { provider_standard_retention_accepted: standard }, models: [{ id: model, output_mode: 'text_chat', max_input_tokens: 16384, pricing: { input_units_per_million_tokens: 60000000, output_units_per_million_tokens: 240000000 } }] };
  const client = { organization_id: randomUUID(), project_id: projectId };
  for (const [name, data] of Object.entries({ 'qualification.json': proof, 'models.json': { destinations: [destination] }, 'budget.json': { ceiling_eur: 1, currency: 'USD', unit_scale: 1e9, limit_units: 600000000, eur_usd: 1 }, 'client.json': client })) await writeFile(join(stateDir, name), JSON.stringify(data));
  const vaultPath = join(stateDir, 'vault');
  await writeFile(vaultPath, vault);
  const budget = { currency: 'USD', unit_scale: 1e9, limit_units: 600000000, spent_units: 259080, reserved_units: 1000 };
  const policy = { allow_unknown_provider_retention: allowUnknown, allowed_destinations: ['nebius-chat'], allowed_categories: ['user_request'], allowed_purposes: ['conversation'], limits: { max_retention_seconds: 0, max_input_bytes: 32768 } };
  const state = { logins: 0, calls: [], expired: false, denied: null, policyFailure: false, loginFailure: false, revision: 4, policy, budget };
  const json = (data, status = 200, headers = {}) => new Response(JSON.stringify(data), { status, headers: { 'content-type': 'application/json', ...headers } });
  const fetcher = async (url, opts) => {
    const path = url.pathname, body = opts.body && JSON.parse(opts.body);
    state.calls.push({ path, method: opts.method, body, headers: { ...opts.headers } });
    if(path === state.expirePath) { state.expired = true; state.expirePath = null; }
    if (path === '/v1/auth/login') {
      state.logins++;
      if (state.loginFailure) return json({}, 503);
      return new Response(null, { status: 303, headers: { location: 'http://127.0.0.1:59190/authorize?state=test', 'set-cookie': 'kyro_oidc_binding=synthetic' } });
    }
    if (path === '/authorize') return json({ code: 'synthetic' });
    if (path === '/v1/auth/callback') {
      state.expired = false;
      const headers = new Headers();
      headers.append('set-cookie', `kyro_session=synthetic-${state.logins}`);
      headers.append('set-cookie', `kyro_csrf=csrf-${state.logins}`);
      return new Response(null, { status: 303, headers });
    }
    if (state.expired || state.denied) return json({ error: { code: state.denied || 'unauthenticated' } }, state.denied === 'forbidden' ? 403 : 401);
    assert.equal(opts.headers.cookie, `kyro_session=synthetic-${state.logins}; kyro_csrf=csrf-${state.logins}`);
    if (path === '/v1/auth/session') {
      await state.onSession?.();
      return json({ csrf_token: `csrf-${state.logins}` });
    }
    if (opts.method !== 'GET') assert.equal(opts.headers['x-csrf-token'], `csrf-${state.logins}`);
    if (path === `/v1/projects/${projectId}/budget`) {
      assert.equal(opts.method, 'GET', 'The durable budget must not be reset');
      return json(budget);
    }
    if (path === `/v1/projects/${projectId}`) return json({ project: { id: projectId, current_revision: state.revision, data_policy: state.policy } }, 200, { etag: `"rev-${state.revision}"` });
    if (path === `/v1/projects/${projectId}/data-policy`) {
      assert.equal(opts.method, 'PUT');
      assert.equal(opts.headers['if-match'], `"rev-${state.revision}"`);
      if (state.policyFailure) return json({ error: { code: 'stale_revision' } }, 412);
      state.policy = body; state.revision++;
      return json({ project: { data_policy: body } });
    }
    if (path === `/v1/projects/${projectId}/jobs` && opts.method === 'POST') return json({ id: opts.headers['idempotency-key'] });
    if (path.endsWith('/stream')) return new Response('event: complete\nid: resume-2\ndata: {"status":"succeeded"}\n\n', { headers: { 'content-type': 'text/event-stream' } });
    if (path.includes('/jobs/') && opts.method === 'DELETE') return json({});
    throw new Error(`Unexpected endpoint ${path}`);
  };
  const chat = createChatService({ stateDir, vaultPath, fetch: fetcher });
  return { chat, state, client, stateDir, input: { key: randomUUID(), messages: [{ role: 'user', content: 'Synthetic test' }], contextTokens: 8192 } };
}

for (const standard of [true, false]) test(`reused project reconciles retention (${standard ? 'standard' : 'zero'}) without refunding the budget`, async t => {
  const f = await fixture(t, { standard });
  const expectedPolicy = { ...f.state.policy, allow_unknown_provider_retention: standard };
  assert.equal((await f.chat.status()).ready, true);
  assert.deepEqual(f.state.policy, expectedPolicy);
  assert.deepEqual(JSON.parse(await readFile(join(f.stateDir, 'client.json'))), f.client);
  assert.equal((await f.chat.status()).budget.spentUsd, f.state.budget.spent_units / 1e9);
  assert.equal(f.state.calls.filter(c => c.path.endsWith('/data-policy')).length, 1);
});

test('matching retention is unchanged; a failed policy update never reports ready', async t => {
  const f = await fixture(t, { allowUnknown: false });
  assert.equal((await f.chat.status()).ready, true);
  assert.equal(f.state.calls.some(c => c.path.endsWith('/data-policy')), false);
  const denied = await fixture(t, { standard: true });
  denied.state.policyFailure = true;
  assert.equal((await denied.chat.status()).ready, false);
  assert.equal(denied.state.policy.allow_unknown_provider_retention, false);
});

test('expired status renews the cached session', async t => {
  const f = await fixture(t, { allowUnknown: false });
  assert.equal((await f.chat.status()).ready, true);
  f.state.expired = true;
  assert.equal((await f.chat.status()).ready, true);
  assert.equal(f.state.logins, 2);
});

test('concurrent expired send and stream share a renewal and preserve key/cursor', async t => {
  const f = await fixture(t, { allowUnknown: false });
  const job = await f.chat.send(f.input);
  f.state.expired = true;
  const events = [];
  const [resent] = await Promise.all([
    f.chat.send(f.input),
    f.chat.watch(job.jobId, 'resume-1', e => events.push(e), new AbortController().signal),
  ]);
  assert.deepEqual(resent, job);
  assert.equal(f.state.logins, 2);
  assert.equal(events[0].data.status, 'succeeded');
  assert.equal(f.state.calls.filter(c => c.path.endsWith('/jobs') && c.method === 'POST').length, 2);
  assert.equal(f.state.calls.filter(c => c.path.endsWith('/stream')).length, 2);
  assert.ok(f.state.calls.filter(c => c.path.endsWith('/stream')).every(c => c.headers['last-event-id'] === 'resume-1'));
  f.state.expired = true;
  assert.deepEqual(await f.chat.cancel(job.jobId), { cancelled: true });
  assert.equal(f.state.logins, 3);
});

test('renewal retries an unauthorized POST once with its unchanged idempotency key', async t => {
  const f = await fixture(t, { allowUnknown: false });
  await f.chat.status();
  // Expire at the mutation boundary, after the project snapshot succeeds.
  const originalKey = f.input.key;
  f.state.expirePath = `/v1/projects/${f.client.project_id}/jobs`;
  assert.equal((await f.chat.send(f.input)).jobId, originalKey);
  const posts = f.state.calls.filter(c => c.path.endsWith('/jobs'));
  assert.equal(posts.length, 2);
  assert.deepEqual(posts[0].body, posts[1].body);
  assert.equal(posts[1].headers['idempotency-key'], originalKey);
});

test('a request arriving during renewal waits for the new CSRF token', async t => {
  const f = await fixture(t, { allowUnknown: false });
  const job = await f.chat.send(f.input);
  let release, entered;
  const waiting = new Promise(resolve => { release = resolve; });
  const started = new Promise(resolve => { entered = resolve; });
  f.state.onSession = () => { entered(); return waiting; };
  f.state.expired = true;
  const status = f.chat.status();
  await started;
  const cancelled = f.chat.cancel(job.jobId);
  release();
  assert.equal((await status).ready, true);
  assert.deepEqual(await cancelled, { cancelled: true });
  assert.equal(f.state.logins, 2);
});

test('failed renewal can recover, permanent auth failures are bounded, other refusals are not replayed', async t => {
  const f = await fixture(t, { allowUnknown: false });
  await f.chat.status();
  f.state.expired = true; f.state.loginFailure = true;
  assert.equal((await f.chat.status()).ready, false);
  f.state.loginFailure = false;
  assert.equal((await f.chat.status()).ready, true);
  const before = f.state.logins;
  f.state.denied = 'unauthenticated';
  assert.equal((await f.chat.status()).ready, false);
  assert.equal(f.state.logins, before + 1);
  f.state.denied = null;
  assert.equal((await f.chat.status()).ready, true);
  const after = f.state.logins;
  f.state.denied = 'forbidden';
  assert.equal((await f.chat.status()).ready, false);
  assert.equal(f.state.logins, after);
});
