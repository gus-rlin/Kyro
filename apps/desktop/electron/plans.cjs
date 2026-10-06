const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
function invalid() { throw Object.assign(new Error('Demande de plan invalide.'), { code: 'invalid_plan_input' }); }
function object(value, keys) {
  if (!value || typeof value !== 'object' || Array.isArray(value) || Object.keys(value).some(key => !keys.includes(key))) invalid();
}
function projectPath(projectId) {
  if (typeof projectId !== 'string' || !UUID.test(projectId)) invalid();
  return `/v1/projects/${projectId}`;
}
function runPath(value, mutation = false) {
  object(value, mutation ? ['projectId', 'runId', 'version'] : ['projectId', 'runId']);
  if (typeof value.runId !== 'string' || !UUID.test(value.runId) || (mutation && (!Number.isSafeInteger(value.version) || value.version < 1))) invalid();
  return `${projectPath(value.projectId)}/plans/${value.runId}`;
}
function summary(run) {
  return {
    id: run.id, projectId: run.project_id, version: run.version, status: run.status,
    request: run.request.request, planOnly: run.request.plan_only, diagnostic: run.diagnostic,
    objective: run.plan?.objective, missingCapabilities: run.plan?.missing_capabilities || [],
    tasks: (run.plan?.tasks || []).map(task => ({ id: task.id, objective: task.objective, dependencies: task.dependencies, components: task.components, complete: !!run.results?.[task.id] })),
    calls: run.calls.map(call => ({ role: call.role, taskId: call.task_id, failure: call.failure })),
    artifactId: run.artifact_id, deadline: run.deadline,
  };
}
// Closed commands over the existing authenticated local transport. No URL, grants,
// provider configuration or model-created executable code crosses this bridge.
function createPlansService({ ensureSession, jsonRequest }) {
  async function read(path, options) { await ensureSession(); return jsonRequest(path, options, 2097152); }
  return {
    async plansProjects() {
      return (await read('/v1/projects?limit=32')).data.map(project => ({ id: project.id, name: project.name }));
    },
    async plansStatus(projectId) {
      const path = projectPath(projectId);
      const capability = (await read(`${path}/plans/capabilities`)).data;
      const snapshot = (await read(path)).data;
      const budget = (await read(`${path}/budget`)).data;
      return { ...capability, revision: snapshot.project.current_revision, budget: { currency: budget.currency, scale: budget.unit_scale, spent: budget.spent_units, reserved: budget.reserved_units, limit: budget.limit_units } };
    },
    async plansList(projectId) { return (await read(`${projectPath(projectId)}/plans`)).data.map(summary); },
    async plansStart(value) {
      object(value, ['projectId', 'key', 'revision', 'request', 'contextBytes']);
      const path = `${projectPath(value.projectId)}/plans`;
      if (typeof value.key !== 'string' || !UUID.test(value.key) || !Number.isSafeInteger(value.revision) || value.revision < 0 || typeof value.request !== 'string' || !value.request.trim() || Buffer.byteLength(value.request) > 8192 || ![4096, 8192, 16384].includes(value.contextBytes)) invalid();
      try {
        return summary((await read(path, { method: 'POST', headers: { 'if-match': `"rev-${value.revision}"`, 'idempotency-key': value.key }, body: {
          request: value.request, plan_only: true,
          limits: { max_calls: 32, max_tokens: 2000000, max_output_tokens: 4096, call_timeout_ms: 30000, ttl_seconds: 1800, max_task_attempts: 2, context_bytes: value.contextBytes },
        } })).data);
      } catch (error) {
        // An uncertain send must retain both the key AND original source revision.
        if (!error.code) error.code = 'transport_unknown';
        throw error;
      }
    },
    async plansRead(value) { return summary((await read(runPath(value))).data); },
    async plansExecute(value) { return summary((await read(`${runPath(value, true)}/execute`, { method: 'POST', headers: { 'if-match': `"rev-${value.version}"` } })).data); },
    async plansCancel(value) { return summary((await read(runPath(value, true), { method: 'DELETE', headers: { 'if-match': `"rev-${value.version}"` } })).data); },
    async plansUsage(value) {
      return (await read(`${runPath(value)}/usage`)).data.map(item => ({ role: item.role, status: item.status, usage: item.usage, estimatedUnits: item.estimated_units, currency: item.registration.pricing.currency, scale: item.registration.pricing.unit_scale }));
    },
  };
}
module.exports = { createPlansService };
