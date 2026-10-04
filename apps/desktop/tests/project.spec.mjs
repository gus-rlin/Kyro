import { test, expect, _electron as electron } from '@playwright/test';
import { mkdtemp, readdir, readFile, rm, mkdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, dirname, basename } from 'node:path';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { createServer } from 'vite';
import { request } from 'node:http';
import { nativeDevelopment } from '../scripts/native-development.mjs';
const require = createRequire(import.meta.url);
const { createWorkspaceService } = require('../electron/workspaces.cjs');
const { gitExecutable } = require('../electron/git-runtime.cjs');
async function clean(path) {
  if (dirname(resolve(path)) !== resolve(tmpdir()) || !basename(path).startsWith('kyro-')) throw new Error('Unsafe test cleanup');
  await rm(path, { recursive: true, force: true });
}

test('new project flow creates a real local repository only after trust', async () => {
  const profile = await mkdtemp(join(tmpdir(), 'kyro-onboarding-profile-'));
  const parent = await mkdtemp(join(tmpdir(), 'kyro-onboarding-fixture-'));
  let app;
  try {
    const packaged = process.env.KYRO_TEST_EXE;
    const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => key.toLowerCase() !== 'path'));
    app = await electron.launch({ ...(packaged ? { executablePath: resolve(packaged) } : {}), args: [...(packaged ? [] : ['.']), `--kyro-profile=${profile}`], env: { ...env, PATH: join(process.env.SystemRoot || 'C:/Windows', 'System32'), KYRO_DESKTOP_DEV: '0' } });
    if (packaged) expect(await app.evaluate(({ app }) => app.isPackaged)).toBe(true);
    const page = await app.firstWindow();
    await page.locator('.checkout-picker summary').click();
    await expect(page.locator('.checkout-picker').getByRole('button', { name: 'Créer un worktree…' })).toBeDisabled();
    await expect(page.locator('.checkout-picker')).not.toContainText('Nouveau projet');
    await page.keyboard.press('Escape');
    await page.locator('.folder-picker summary').click();
    await page.getByRole('button', { name: 'Nouveau projet…' }).click();
    await expect(page.getByRole('dialog')).toContainText('automatiquement dans Documents');
    await expect(page.getByRole('textbox', { name: 'Nom du projet' })).toBeFocused();
    await expect(page.getByRole('button', { name: 'Continuer' })).toBeDisabled();
    await page.getByRole('textbox', { name: 'Nom du projet' }).fill('Atelier');
    await expect(page.getByRole('button', { name: 'Continuer' })).toBeEnabled();
    await page.screenshot({ path: 'test-results/project-name.png', animations: 'disabled' });
    await app.evaluate(({ app, dialog }, parent) => { app.setPath('documents', parent); dialog.showOpenDialog = async () => { throw new Error('Unexpected folder picker'); }; }, parent);
    await page.getByRole('button', { name: 'Continuer' }).click();
    await expect(page.getByRole('heading', { name: 'Un espace pour Atelier.' })).toBeFocused();
    await expect(page.getByRole('button', { name: 'Créer le projet' })).toBeDisabled();
    await page.screenshot({ path: 'test-results/project-trust.png', animations: 'disabled', mask: [page.locator('.project-destination p')], maskColor: '#eeefea' });
    expect(await readdir(parent)).toEqual([]);
    await page.keyboard.press('Escape');
    expect(await readdir(parent)).toEqual([]);
    await expect(page.locator('.folder-picker summary')).toBeFocused();
    await page.locator('.folder-picker summary').click();
    await page.getByRole('button', { name: 'Nouveau projet…' }).click();
    await page.getByRole('textbox', { name: 'Nom du projet' }).fill('Atelier');
    await page.getByRole('button', { name: 'Continuer' }).click();
    await page.getByRole('checkbox', { name: 'Faire confiance à ce dossier', exact: false }).check();
    await page.getByRole('button', { name: 'Créer le projet' }).click();
    await expect(page.getByRole('dialog')).not.toBeVisible();
    await expect(page.locator('.checkout-folder')).toContainText('Atelier');
    await expect(page.locator('.checkout-picker summary')).toContainText('main');
    const project = join(parent, 'Atelier');
    expect(JSON.parse(await readFile(join(project, '.kyro/project.json'), 'utf8')).name).toBe('Atelier');
    const git = (...args) => execFileSync(gitExecutable(), ['-C', project, ...args], { encoding: 'utf8', windowsHide: true });
    expect(git('rev-list', '--count', 'HEAD').trim()).toBe('1');
    expect(git('status', '--porcelain').trim()).toBe('');
    expect(gitExecutable()).toContain('mingit');
    git('branch', 'feature/sans-worktree');
    git('update-ref', 'refs/remotes/origin/design', 'HEAD');
    await page.locator('.checkout-picker summary').click();
    await page.getByRole('button', { name: 'Branches', exact: true }).click();
    await expect(page.getByLabel('Liste des branches')).toContainText('feature/sans-worktree');
    await expect(page.getByLabel('Liste des branches')).toContainText('origin/design');
    await expect(page.getByLabel('Liste des branches')).toContainText('Actuelle');
    await page.screenshot({ path: 'test-results/project-branches.png', animations: 'disabled' });
    await page.getByRole('button', { name: 'Worktrees', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Créer un worktree…' })).toBeVisible();
    await page.getByRole('button', { name: 'Créer un worktree…' }).click();
    await expect(page.getByRole('textbox', { name: 'Nouvelle branche' })).toBeVisible();
    await page.keyboard.press('Escape');
    await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setMinimumSize(320, 500));
    await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setSize(390, 844));
    await page.locator('.app').evaluate((element) => element.classList.remove('desktop'));
    await page.locator('.folder-picker summary').click();
    await page.getByRole('button', { name: 'Nouveau projet…' }).click();
    await expect(page.getByRole('textbox', { name: 'Nom du projet' })).toBeVisible();
    await expect.poll(() => page.locator('.project-dialog[open]').evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
    await page.screenshot({ path: 'test-results/project-mobile.png', animations: 'disabled' });
  } finally { await app?.close(); await clean(profile); await clean(parent); }
});

test('native service refuses untrusted, reused, traversing and occupied plans', async () => {
  const parent = await mkdtemp(join(tmpdir(), 'kyro-service-fixture-'));
  const service = createWorkspaceService({ showOpenDialog: async () => ({ canceled: false, filePaths: [parent] }) }, () => null, () => parent);
  try {
    for (const name of ['../outside', 'CON', 'trail.', 'A/B', '']) await expect(service.prepareProject(name)).rejects.toThrow('nom de dossier');
    const plan = await service.prepareProject('Atelier');
    await expect(service.createProject(plan.id, false)).rejects.toThrow('confiance');
    await expect(service.select(plan.id)).rejects.toThrow('Choisissez');
    expect(await readdir(parent)).toEqual([]);
    await service.discard(plan.id);
    await expect(service.createProject(plan.id, true)).rejects.toThrow('confiance');
    const race = await service.prepareProject('Atelier');
    await mkdir(join(parent, 'Atelier'));
    await expect(service.createProject(race.id, true)).rejects.toThrow('existe déjà');
    expect(await readdir(join(parent, 'Atelier'))).toEqual([]);
    await expect(service.prepareProject('Atelier')).rejects.toThrow('déjà');
    const opening = await service.choose('folder');
    await expect(service.select(opening.id)).rejects.toThrow('Choisissez');
    await expect(service.trust(opening.id, false)).rejects.toThrow('confiance');
    const selected = await service.trust(opening.id, true);
    expect(selected.name).toBe(basename(parent));
    await expect(service.trust(opening.id, true)).rejects.toThrow('confiance');
    const a = await service.prepareProject('Premier');
    const b = await service.prepareProject('Second');
    const results = await Promise.allSettled([service.createProject(a.id, true), service.createProject(b.id, true)]);
    expect(results.map((item) => item.status)).toEqual(['fulfilled', 'rejected']);
    expect(await readdir(parent)).not.toContain('Second');
  } finally { await clean(parent); }
});

test('local preview bridge creates through the same trust boundary', async () => {
  const parent = await mkdtemp(join(tmpdir(), 'kyro-preview-fixture-'));
  let cancelPicker = true;
  const server = await createServer({ configFile: false, logLevel: 'error', server: { host: '127.0.0.1', port: 0 }, plugins: [nativeDevelopment({ showOpenDialog: async () => cancelPicker ? { canceled: true, filePaths: [] } : { canceled: false, filePaths: [parent] } }, () => parent)] });
  try {
    await server.listen();
    const port = server.httpServer.address().port;
    const call = (action, args, origin = 'http://127.0.0.1:5174') => new Promise((resolve, reject) => {
      const req = request(`http://127.0.0.1:${port}/__kyro_native/${action}`, { method: 'POST', headers: { Host: '127.0.0.1:5174', Origin: origin, 'X-Kyro-Local': '1', 'Content-Type': 'application/json' } }, (res) => {
        let body = ''; res.setEncoding('utf8'); res.on('data', (chunk) => { body += chunk; });
        res.on('end', () => resolve({ status: res.statusCode, ...JSON.parse(body) }));
      });
      req.on('error', reject); req.end(JSON.stringify(args));
    });
    expect((await call('prepareProject', ['Preview'], 'https://example.com')).status).toBe(403);
    expect((await call('choose', ['folder'])).value).toBeNull();
    expect(await readdir(parent)).toEqual([]);
    cancelPicker = false;
    const folderPlan = (await call('choose', ['folder'])).value;
    expect(folderPlan.path).toBe(parent);
    expect((await call('select', [folderPlan.id])).error).toContain('Choisissez');
    expect((await call('trust', [folderPlan.id, false])).error).toContain('confiance');
    await call('discard', [folderPlan.id]);
    const plan = (await call('prepareProject', ['Preview'])).value;
    expect(plan.purpose).toBe('project');
    expect(await readdir(parent)).toEqual([]);
    expect((await call('createProject', [plan.id, false])).error).toContain('confiance');
    const result = await call('createProject', [plan.id, true]);
    expect(result.value.branch).toBe('main');
    expect(await readFile(join(parent, 'Preview/README.md'), 'utf8')).toContain('Preview');
    expect((await call('list', [result.value.id])).value).toHaveLength(1);
  } finally { await server.close(); await clean(parent); }
});
