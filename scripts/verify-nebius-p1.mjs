import { readFileSync, writeFileSync, openSync, closeSync, unlinkSync, renameSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import assert from 'node:assert/strict';

const live = process.argv.includes('--run');
assert(process.argv.length === 3 && ['--preflight', '--run'].includes(process.argv[2]), 'Use --preflight or --run');
const state = join(homedir(), '.kyro', 'nebius-p1');
const registry = JSON.parse(readFileSync(join(state, 'models.json'), 'utf8'));
const campaignPath = join(state, 'campaign.json');
const lockPath = join(state, 'campaign.lock');
const lock = openSync(lockPath, 'wx'); // A crash leaves a lock and pending flag, requiring inspection.
const campaign = JSON.parse(readFileSync(campaignPath, 'utf8'));
const destination = registry.destinations[0];
const model = destination.models[0];
const origin = 'http://127.0.0.1:58090';
const report = { kind: 'nebius_p1_qualification', started_at: new Date().toISOString(), mode: live ? 'live' : 'preflight', oidc: 'synthetic', provider: 'nebius', model: model.id, model_weights_version: model.version, billed_amount_usd: null, calls: [], checks: {}, status: 'failed' };
let session;
let stage = 'readiness';
function saveCampaign() {
  const temporary = `${campaignPath}.tmp`;
  writeFileSync(temporary, JSON.stringify(campaign, null, 2) + '\n'); renameSync(temporary, campaignPath);
}
async function request(path, { method = 'GET', body, headers = {}, auth = true } = {}) {
  if (auth && session) {
    headers.cookie = session.cookie;
    if (method !== 'GET') { headers.origin = origin; headers['x-csrf-token'] = session.csrf; }
  }
  if (body) headers['content-type'] = 'application/json';
  const response = await fetch(new URL(path, origin), { method, body: body && JSON.stringify(body), headers, redirect: 'manual', signal: AbortSignal.timeout(35_000) });
  const text = await response.text(); assert(text.length < 131072, 'response_limit');
  return { response, data: text ? JSON.parse(text) : null };
}
function cookie(headers, name) {
  return headers.getSetCookie().map((value) => value.split(';')[0]).find((value) => value.startsWith(`${name}=`));
}
async function login() {
  stage = 'oidc_login';
  const initial = await request('/v1/auth/login', { auth: false }); assert.equal(initial.response.status, 303);
  const authorization = new URL(initial.response.headers.get('location'));
  assert.equal(authorization.origin, 'http://127.0.0.1:59090');
  stage = 'oidc_authorization';
  const authorized = await request(authorization.href, { auth: false }); assert.equal(authorized.response.status, 200);
  const callback = `/v1/auth/callback?code=${encodeURIComponent(authorized.data.code)}&state=${encodeURIComponent(authorization.searchParams.get('state'))}`;
  stage = 'oidc_callback';
  const completed = await request(callback, { auth: false, headers: { cookie: cookie(initial.response.headers, 'kyro_oidc_binding') } });
  assert.equal(completed.response.status, 303);
  const cookies = [cookie(completed.response.headers, 'kyro_session'), cookie(completed.response.headers, 'kyro_csrf')];
  assert(cookies.every(Boolean)); session = { cookie: cookies.join('; ') };
  stage = 'oidc_session';
  const resolved = await request('/v1/auth/session'); assert.equal(resolved.response.status, 200); session.csrf = resolved.data.csrf_token;
}
async function budget() { return request(`/v1/projects/${campaign.project_id}/budget`); }
async function admit(input, key) {
  return request(`/v1/projects/${campaign.project_id}/jobs`, { method: 'POST', headers: { 'if-match': '"rev-0"', 'idempotency-key': key }, body: { payload: { kind: 'model_call', request: input }, max_attempts: 1, ttl_seconds: 60 } });
}
try {
  const ready = await request('/health/ready'); assert.equal(ready.response.status, 200);
  await login(); report.checks.http_oidc_postgres = true;
  if (!campaign.project_id) {
    stage = 'create_organization';
    const organization = await request('/v1/organizations', { method: 'POST', body: { name: 'Nebius P1 qualification' } }); assert.equal(organization.response.status, 201);
    stage = 'create_project';
    const created = await request('/v1/projects', { method: 'POST', body: {
      organization_id: organization.data.id, name: 'Nebius P1 — fictitious data',
      data_policy: { allowed_destinations: [destination.id], allowed_categories: ['user_request'], allowed_purposes: ['structured_extraction'], limits: { max_input_bytes: model.max_input_bytes, max_input_tokens: model.max_input_tokens, max_output_tokens: model.max_output_tokens, max_deadline_ms: model.max_deadline_ms, max_response_bytes: model.max_response_bytes, max_retention_seconds: 0 } },
      limits: { max_active_jobs: 1, max_queued_jobs: 3, max_job_attempts: 1, job_ttl_secs: 60, max_revisions: 10 },
    } });
    assert.equal(created.response.status, 201); campaign.project_id = (created.data.project ?? created.data).id; saveCampaign();
    stage = 'configure_budget';
    const before = await budget(); assert.equal(before.response.status, 200);
    const configured = await request(`/v1/projects/${campaign.project_id}/budget`, { method: 'PUT', headers: { 'if-match': before.response.headers.get('etag') }, body: { limit_units: campaign.limit_units, currency: 'USD', unit_scale: 1_000_000_000 } });
    assert.equal(configured.response.status, 200);
  }
  report.project_id = campaign.project_id;
  const before = await budget(); assert.equal(before.response.status, 200);
  assert.equal(before.data.currency, 'USD'); assert.equal(before.data.unit_scale, 1_000_000_000);
  assert(before.data.limit_units <= campaign.limit_units);
  report.budget_before = before.data;
  const input = { destination_id: destination.id, model: model.id, input: { purpose: 'structured_extraction', categories: ['user_request'], content: { brief: 'Fictitious project Cedar. Return a summary under ten words and one short item.' } }, max_output_tokens: 512, deadline_ms: 30000 };
  const canary = structuredClone(input); canary.input.content = { api_key: 'fake-local-canary-never-persist' };
  stage = 'secret_admission';
  const refusal = await admit(canary, 'nebius-preflight-secret'); assert.equal(refusal.response.status, 403); report.checks.secret_refused = true;
  if (!live) {
    assert.equal(destination.qualified, false, 'preflight_requires_disabled_registry');
    stage = 'unqualified_admission';
    const refused = await admit(input, 'nebius-preflight-disabled'); assert.equal(refused.response.status, 503);
    stage = 'queue_persistence';
    const jobs = await request(`/v1/projects/${campaign.project_id}/jobs`); assert.equal(jobs.response.status, 200);
    assert.equal(jobs.data.items.length, 0); report.checks.refused_before_queue_persistence = true;
    report.status = 'passed_preflight_live_blocked';
  } else {
    assert(destination.qualified && destination.retention_seconds === 0 && destination.nebius.json_schema && destination.nebius.bounded_completion, 'qualification_required');
    assert(!campaign.pending && !campaign.stopped_on_uncertainty && campaign.attempts < 3, 'campaign_closed');
    // Persist the attempt BEFORE admission. Interrupted runs are never silently reissued.
    campaign.attempts++; campaign.pending = true; saveCampaign();
    const key = `nebius-p1-${campaign.attempts}`;
    const started = performance.now();
    stage = 'live_admission';
    const admitted = await admit(input, key); assert.equal(admitted.response.status, 202);
    const id = admitted.data.id; let job;
    stage = 'live_result';
    for (let index = 0; index < 400; index++) {
      job = await request(`/v1/projects/${campaign.project_id}/jobs/${id}`);
      if (['succeeded', 'failed', 'unknown', 'cancelled'].includes(job.data.status)) break;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    const elapsed = performance.now() - started;
    assert.equal(job.data.status, 'succeeded', 'provider_result_uncertain');
    stage = 'live_idempotency_replay';
    const replay = await admit(input, key); assert.equal(replay.response.status, 202); assert.equal(replay.data.id, id);
    const effectId = job.data.result.effect_id;
    const effect = await request(`/v1/projects/${campaign.project_id}/effects/${effectId}`);
    assert.equal(effect.response.status, 200); assert.equal(effect.data.status, 'succeeded');
    const result = effect.data.result;
    assert(result?.provider_request_id && result?.usage?.input_tokens >= 0 && result?.usage?.output_tokens >= 0, 'provider_usage_and_receipt_required');
    stage = 'live_settlement';
    const after = await budget(); assert.equal(after.data.reserved_units, 0); assert(after.data.spent_units > before.data.spent_units);
    report.calls.push({ job_id: id, effect_id: effectId, provider_request_id: result.provider_request_id, latency_ms: Math.round(elapsed), input_tokens: result.usage.input_tokens, output_tokens: result.usage.output_tokens, estimated_usd: (after.data.spent_units-before.data.spent_units)/1e9, status: 'settled' });
    report.checks.idempotency_replay = true; campaign.pending = false; saveCampaign(); report.status = 'passed_live';
  }
  report.budget_after = (await budget()).data;
} catch {
  report.failure_stage = stage;
  if (live && campaign.pending) { campaign.stopped_on_uncertainty = true; saveCampaign(); }
  report.status = 'failed_or_uncertain'; process.exitCode = 1;
} finally {
  if (session?.csrf) await request('/v1/auth/logout', { method: 'POST' }).catch(() => {});
  if (live) {
    const stopped = spawnSync('pwsh', ['-NoProfile', '-File', 'scripts/nebius-runtime.ps1', '-Action', 'Stop'], { encoding: 'utf8', timeout: 60_000 });
    report.worker_stopped = stopped.status === 0;
    if (!report.worker_stopped) process.exitCode = 1;
  }
  report.finished_at = new Date().toISOString();
  const stamp = report.started_at.replaceAll(/[^0-9A-Za-z]/g, '');
  writeFileSync(`docs/suivi/preuves/nebius-${live ? 'live' : 'preflight'}-${stamp}.json`, JSON.stringify(report, null, 2)+'\n');
  closeSync(lock); unlinkSync(lockPath);
  process.stdout.write(JSON.stringify(report, null, 2)+'\n');
}
