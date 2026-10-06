import { test, expect, _electron as electron } from '@playwright/test';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, dirname, basename } from 'node:path';
import { createServer } from 'vite';
import react from '@vitejs/plugin-react';
import { randomUUID } from 'node:crypto';
import { createServer as httpServer } from 'node:http';
import plans from '../electron/plans.cjs';
import { chatDevelopment } from '../scripts/chat-development.mjs';

async function fixture({ configured = true, uncertain = false } = {}) {
  const projectId = randomUUID(), calls = [], history = [];
  let pendingFailure = uncertain, denyExecute = false;
  const roleNames = ['orchestrator', 'pixel', 'moka', 'kiwi', 'biscotte', 'review', 'security'];
  const backend = plans.createPlansService({ async ensureSession() {}, async jsonRequest(path, options) {
    calls.push({ path, ...options });
    if (path === '/v1/projects?limit=32') return { data: [{ id: projectId, name: 'Bibliothèque synthétique' }] };
    if (path.endsWith('/capabilities')) return { data: { configured, code: configured ? 'configured' : 'agents_unconfigured', synthetic: true, executor_count: configured ? 4 : 0, component_count: configured ? 1 : 0, roles: configured ? roleNames.map(role => ({ role, model: role === 'orchestrator' ? 'synthetic-orchestrator' : 'synthetic-worker' })) : [], tools: configured ? [{ id: 'plan', label: 'Planifier avec le catalogue', available: true }, { id: 'publish', label: 'Publier', available: false }] : [] } };
    if (path.endsWith('/budget')) return { data: { currency: 'USD', unit_scale: 1e9, limit_units: 500000000, spent_units: 1000, reserved_units: 2000 } };
    if (path === `/v1/projects/${projectId}`) return { data: { project: { current_revision: 4 } } };
    if (path === `/v1/projects/${projectId}/plans` && !options?.method) return { data: history };
    if (path.endsWith('/execute')) {
      if (denyExecute) throw Object.assign(new Error('La version du plan a changé.'), { code: 'conflict' });
      const run = history[0]; expect(options.headers['if-match']).toBe(`"rev-${run.version}"`);
      run.status = 'building'; run.version++; run.request.plan_only = false; return { data: run };
    }
    if (options?.method === 'DELETE') { const run = history[0]; run.status = 'cancelled'; run.version++; return { data: run }; }
    if (options?.method === 'POST') {
      if (pendingFailure) { pendingFailure = false; throw new Error('Synthetic lost response'); }
      expect(options.body.plan_only).toBe(true);
      const run = { id: randomUUID(), project_id: projectId, version: 1, status: 'planning', request: options.body, results: {}, calls: [], plan: null };
      history.unshift(run); return { data: run };
    }
    const run = history.find(run => path.endsWith(run.id));
    if (run.status === 'planning') { run.status = 'planned'; run.version++; run.plan = { objective: 'Gérer les livres <script>test</script>', missing_capabilities: [], tasks: [{ id: 'books', objective: 'Composer la liste des livres', components: [{ id: 'B031', version: '0.2.0' }], dependencies: [] }] }; run.calls.push({ role: 'orchestrator' }); }
    if (run.status === 'building') { run.status = 'verified'; run.version++; run.artifact_id = randomUUID(); run.results.books = {}; }
    return { data: run };
  } });
  const chat = { ...backend, async status() { return { ready: false, message: 'Chat synthétique hors ligne.' }; }, async send() { throw new Error('Conversation forbidden in plan tests'); } };
  const probe = httpServer();
  await new Promise(resolve => probe.listen(0, '127.0.0.1', resolve));
  const port = probe.address().port, origin = `http://127.0.0.1:${port}`;
  await new Promise(resolve => probe.close(resolve));
  const server = await createServer({ configFile: false, root: process.cwd(), plugins: [react(), chatDevelopment(chat, origin), { name: 'test-csp', transformIndexHtml: html => html.replace("script-src 'self'", "script-src 'self' 'unsafe-inline'") }], server: { host: '127.0.0.1', port, strictPort: true, hmr: false } });
  try { await server.listen(); } catch (failure) { await server.close(); throw failure; }
  return { server, calls, projectId, origin, deny() { denyExecute = true; } };
}
async function chooseAgents(page, projectId) {
  await page.locator('.team-trigger').click();
  await page.getByRole('button', { name: 'Agents', exact: true }).click();
  await page.getByLabel('Projet de construction').selectOption(projectId);
  await page.getByRole('textbox', { name: 'Votre message' }).click();
}
test('composeur -> pont fermé -> P3 simulée : plan, exécution, candidat, reprise et annulation', async ({ page }) => {
  const f = await fixture();
  try {
    await page.goto(f.origin); await chooseAgents(page, f.projectId);
    await expect(page.getByRole('status')).toContainText('Équipe synthétique');
    await page.locator('.team-trigger').click();
    const panel = page.getByRole('region', { name: 'Composition de l’équipe' });
    await expect(panel).toContainText('Orchestrateur'); await expect(panel).toContainText('synthetic-orchestrator'); await expect(panel).toContainText('Biscotte'); await expect(panel).toContainText('Publier · indisponible');
    await page.getByRole('textbox', { name: 'Votre message' }).fill('Gérer mes livres <script>test</script>');
    await page.getByRole('button', { name: 'Proposer un plan' }).click();
    await expect(page.getByRole('article', { name: 'Plan des agents' })).toContainText('Plan proposé');
    expect(f.calls.some(call => call.path.endsWith('/execute'))).toBe(false);
    await expect(page.getByRole('article')).toContainText('<script>test</script>'); expect(await page.locator('.agent-plan script').count()).toBe(0);
    await page.getByRole('button', { name: 'Exécuter le plan' }).click();
    await expect(page.getByRole('article')).toContainText('Candidat vérifié');
    await page.reload(); await chooseAgents(page, f.projectId); await expect(page.getByRole('article')).toContainText('Candidat vérifié');
    await page.getByRole('textbox', { name: 'Votre message' }).fill('Deuxième plan'); await page.getByRole('button', { name: 'Proposer un plan' }).click();
    await page.getByRole('button', { name: 'Annuler le plan' }).click(); await expect(page.getByRole('article')).toContainText('Annulé');
    for (const width of [390, 320]) { await page.setViewportSize({ width, height: 900 }); expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true); }
  } finally { await f.server.close(); }
});
test('runtime non configuré : refus explicite, brouillon conservé, aucun plan envoyé', async ({ page }) => {
  const f = await fixture({ configured: false });
  try {
    await page.goto(f.origin); await chooseAgents(page, f.projectId);
    await expect(page.getByRole('status')).toContainText('n’a pas encore d’équipe P3');
    await page.getByRole('textbox', { name: 'Votre message' }).fill('Ma bibliothèque'); await page.getByRole('button', { name: 'Proposer un plan' }).click();
    await expect(page.getByRole('alert')).toContainText('catalogue configurés'); await expect(page.getByRole('textbox')).toHaveValue('Ma bibliothèque');
    expect(f.calls.some(call => call.method)).toBe(false);
  } finally { await f.server.close(); }
});
test('envoi incertain : clé/révision conservées ; conflit d’exécution visible sans répétition', async ({ page }) => {
  const f = await fixture({ uncertain: true });
  try {
    await page.goto(f.origin); await chooseAgents(page, f.projectId);
    await expect(page.getByRole('status')).toContainText('Équipe synthétique');
    await page.getByRole('textbox', { name: 'Votre message' }).fill('Un plan'); await page.getByRole('button', { name: 'Proposer un plan' }).click();
    await page.getByRole('button', { name: 'Reprendre la demande de plan' }).click();
    await expect(page.getByRole('article')).toContainText('Plan proposé');
    await expect(page.getByRole('textbox')).toHaveValue('');
    const sends = f.calls.filter(call => call.method === 'POST'); expect(sends).toHaveLength(2); expect(sends[0].headers).toEqual(sends[1].headers); expect(sends[0].body).toEqual(sends[1].body);
    f.deny(); await page.getByRole('button', { name: 'Exécuter le plan' }).click(); await expect(page.getByRole('alert')).toContainText('version du plan a changé');
    expect(f.calls.filter(call => call.path.endsWith('/execute'))).toHaveLength(1);
  } finally { await f.server.close(); }
});
test('Electron : commandes P3 exposées par le preload, sans appel fournisseur', async () => {
  const profile = await mkdtemp(join(tmpdir(), 'kyro-plans-ipc-'));
  let app;
  try {
    app = await electron.launch({ args: ['.', `--kyro-profile=${profile}`], env: { ...process.env, KYRO_DESKTOP_DEV: '0' } });
    const projectId = randomUUID(), runId = randomUUID();
    await app.evaluate(({ ipcMain }, ids) => {
      const status = { configured: true, code: 'configured', synthetic: true, roles: [{ role: 'orchestrator', model: 'synthetic' }], executor_count: 4, component_count: 1, revision: 0, tools: [], budget: { currency: 'USD', scale: 1e9, spent: 0, reserved: 0, limit: 1e8 } };
      const run = { id: ids.runId, projectId: ids.projectId, version: 1, status: 'planned', request: 'Plan IPC synthétique', planOnly: true, missingCapabilities: [], tasks: [], calls: [], deadline: new Date().toISOString() };
      const actions = { plansProjects: () => [{ id: ids.projectId, name: 'Projet IPC synthétique' }], plansStatus: () => status, plansList: () => [], plansStart: input => { if (input.revision !== 0) throw new Error('Source revision lost'); return run; }, plansRead: () => run, plansExecute: () => ({ ...run, status: 'executing' }), plansCancel: () => ({ ...run, status: 'cancelled', version: 2 }) };
      for (const [name, handle] of Object.entries(actions)) { ipcMain.removeHandler(`chat:${name}`); ipcMain.handle(`chat:${name}`, (_event, value) => ({ value: handle(value) })); }
    }, { projectId, runId });
    const page = await app.firstWindow(); await page.waitForLoadState('load'); await page.reload();
    await chooseAgents(page, projectId); await expect(page.getByRole('status')).toContainText('Équipe synthétique');
    await page.getByRole('textbox').fill('Plan IPC synthétique'); await page.getByRole('button', { name: 'Proposer un plan' }).click(); await expect(page.getByRole('article')).toContainText('Plan proposé');
    await page.getByRole('button', { name: 'Annuler le plan' }).click(); await expect(page.getByRole('article')).toContainText('Annulé');
  } finally {
    await app?.close(); const target = resolve(profile);
    if (dirname(target) !== resolve(tmpdir()) || !basename(target).startsWith('kyro-plans-ipc-')) throw new Error('Unsafe profile cleanup');
    await rm(target, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
