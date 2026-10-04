import { test, expect, _electron as electron } from '@playwright/test';
import { mkdtemp, mkdir, rm, writeFile, readFile } from 'node:fs/promises';
import { createServer } from 'vite';
import { tmpdir } from 'node:os';
import { resolve, join, dirname, basename } from 'node:path';
import { execFileSync } from 'node:child_process';

async function launch(profile, dev = false, scale) {
  const packaged = process.env.KYRO_TEST_EXE;
  return electron.launch({
    ...(packaged ? { executablePath: resolve(packaged) } : {}),
    args: [...(packaged ? [] : ['.']), `--kyro-profile=${profile}`, ...(scale ? [`--force-device-scale-factor=${scale}`] : [])],
    env: { ...process.env, KYRO_DESKTOP_DEV: dev ? '1' : '0' },
  });
}

async function removeTestProfile(profile) {
  const target = resolve(profile);
  if (dirname(target) !== resolve(tmpdir()) || !basename(target).startsWith('kyro-')) throw new Error('Refusing to remove a profile outside the test temp directory');
  await rm(target, { recursive: true, force: true });
}

async function resizeWindow(app, page, width, height) {
  await app.evaluate(({ BrowserWindow }, size) => BrowserWindow.getAllWindows()[0].setSize(...size), [width, height]);
  await expect.poll(async () => {
    const size = await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].getSize());
    const content = await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].getContentSize());
    const viewport = await page.evaluate(() => [window.innerWidth, window.innerHeight]);
    return Math.abs(size[0] - width) <= 2 && Math.abs(size[1] - height) <= 2 && Math.abs(content[0] - viewport[0]) <= 2 && Math.abs(content[1] - viewport[1]) <= 2;
  }).toBe(true);
}
async function expectOpaque(page, app) {
  await expect(page.getByRole('main', { name: 'Surface Kyro' })).toBeVisible();
  expect(await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].getBackgroundColor())).toMatch(/^#(?:ff)?f0f1ed$/i);
  expect(await page.evaluate(() => {
    const style = getComputedStyle(document.querySelector('.app'));
    return { background: style.backgroundColor, blur: style.backdropFilter, bridge: typeof window.kyroDesktop };
  })).toEqual({ background: 'rgb(240, 241, 237)', blur: 'none', bridge: 'undefined' });
  await expect(page.getByRole('button', { name: 'Apparence' })).toHaveCount(0);
}

test('opaque window, legacy preferences ignored and sandbox', async () => {
  const profile = await mkdtemp(join(tmpdir(), 'kyro-desktop-smoke-'));
  let app;
  try {
    app = await launch(profile);
    let page = await app.firstWindow();
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));
    await expectOpaque(page, app);
    await expect.poll(() => app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].isVisible())).toBe(true);
    expect(page.url()).toMatch(/^file:/);
    const configuration = await app.evaluate(({ BrowserWindow, app, screen }) => {
      const win = BrowserWindow.getAllWindows()[0];
      const preferences = win.webContents.getLastWebPreferences();
      return { size: win.getSize(), minimum: win.getMinimumSize(), scaleFactor: screen.getDisplayMatching(win.getBounds()).scaleFactor, sandbox: preferences.sandbox, node: preferences.nodeIntegration, isolation: preferences.contextIsolation, packaged: app.isPackaged };
    });
    // Windows rounds logical bounds at fractional display scale factors.
    expect(Math.abs(configuration.size[0] - 1440)).toBeLessThanOrEqual(2);
    expect(Math.abs(configuration.size[1] - 900)).toBeLessThanOrEqual(2);
    expect(configuration.minimum).toEqual([1000, 680]);
    expect(configuration).toMatchObject({ sandbox: true, node: false, isolation: true });
    if (process.env.KYRO_TEST_EXE) expect(configuration.packaged).toBe(true);
    expect(await page.evaluate(() => ({ node: typeof window.require, process: typeof window.process }))).toEqual({ node: 'undefined', process: 'undefined' });
    await expect(page.getByRole('region', { name: 'Chat AI', exact: true })).toBeVisible();
    await expect(page.getByRole('region', { name: 'Terminal', exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Paramètres', exact: true })).toHaveCount(0);
    await page.getByRole('button', { name: 'Une interface à imaginer', exact: false }).click();
    await expect(page.getByRole('textbox', { name: 'Votre message' })).toHaveValue(/ses pages, sa navigation/);
    await expect(page.getByRole('textbox', { name: 'Votre message' })).toBeFocused();
    await expect(page.getByRole('log')).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Explorateur', exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Application', exact: true }).locator('svg')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Masquer le chat', exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Nouvelle conversation', exact: true })).toHaveCount(0);
    await page.getByRole('button', { name: 'Décrire mon idée', exact: true }).click();
    await expect(page.getByRole('textbox', { name: 'Votre message' })).toBeFocused();
    await page.getByRole('button', { name: 'Utilisateurs', exact: true }).click();
    await expect(page.getByRole('main').getByRole('heading', { name: 'Utilisateurs', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Utilisateurs', exact: true })).toHaveAttribute('aria-current', 'page');
    await page.getByRole('button', { name: 'Application', exact: true }).click();
    await expect(page.getByRole('textbox', { name: 'Votre message' })).toHaveValue(/ses pages, sa navigation/);
    await expect(page.getByRole('complementary', { name: 'Explorateur de fichiers' })).toContainText('Aucun dossier ouvert');
    await expect(page.getByText('Aucun aperçu chargé')).toHaveCount(0);
    await expect(page.locator('.composer-context')).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Chat AI', exact: true })).toHaveCount(0);
    await page.locator('.directory-input').evaluate((input) => {
      const paths = ['demo/src/App.tsx', 'demo/src/style.css', 'demo/README.md'];
      const transfer = new DataTransfer();
      for (const path of paths) {
        const file = new File(['example'], path.split('/').at(-1));
        Object.defineProperty(file, 'webkitRelativePath', { value: path });
        transfer.items.add(file);
      }
      input.files = transfer.files;
      input.dispatchEvent(new Event('change', { bubbles: true }));
    });
    await expect(page.getByRole('complementary', { name: 'Explorateur de fichiers' })).toContainText('App.tsx');
    await expect(page.getByRole('contentinfo', { name: 'État du projet' })).toContainText('Dossier local ouvert');
    await page.getByRole('button', { name: 'Détails', exact: true }).click();
    await expect(page.getByRole('dialog', { name: 'État du projet' })).toContainText('vérifications du projet ne sont pas encore connectés');
    await page.keyboard.press('Escape');
    await page.locator('summary').filter({ hasText: /^Modifier$/ }).click();
    await page.getByRole('button', { name: 'Effacer le brouillon' }).click();
    await expect(page.getByRole('button', { name: 'Envoyer le message' })).toBeDisabled();
    await page.getByRole('textbox', { name: 'Votre message' }).fill('Créer une application <script>test</script>');
    await page.getByRole('textbox', { name: 'Votre message' }).press('Shift+Enter');
    await page.getByRole('textbox', { name: 'Votre message' }).press('Enter');
    await expect(page.getByRole('log', { name: 'Conversation' })).toContainText('Créer une application <script>test</script>');
    await expect(page.getByRole('textbox', { name: 'Votre message' })).toHaveValue('');
    await page.getByRole('button', { name: 'Saisie vocale', exact: true }).click();
    await expect(page.getByRole('dialog', { name: 'Saisie vocale' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.getByRole('dialog')).not.toBeVisible();
    await page.getByRole('button', { name: 'Publier', exact: true }).click();
    await expect(page.getByRole('dialog', { name: 'Publier l’application' })).toContainText('destination');
    await page.getByRole('button', { name: 'Compris' }).click();
    await page.getByRole('button', { name: 'Pages', exact: true }).click();
    await page.getByRole('button', { name: 'Application', exact: true }).click();
    await expect(page.getByRole('region', { name: 'Chat AI', exact: true })).toBeVisible();
    await expect(page.getByRole('log', { name: 'Conversation' })).toContainText('Créer une application');
    await expect(page.getByRole('region', { name: 'Terminal', exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Paramètres', exact: true })).toHaveCount(0);
    await page.locator('summary').filter({ hasText: /^Fichier$/ }).click();
    await expect(page.locator('.app-menu[open]')).toContainText('Ouvrir un dossier');
    await expect(page.locator('.app-menu[open]')).not.toContainText('Nouvelle conversation');
    await page.keyboard.press('Escape');
    await expect(page.locator('.app-menu[open]')).toHaveCount(0);
    await page.evaluate(() => {
      localStorage.setItem('kyro.glass-plate.v1', JSON.stringify({ glass: true, opacity: 0, blur: 32 }));
      localStorage.setItem('kyro.polished-glass.v1', 'historical-settings');
    });
    await app.close();
    app = await launch(profile);
    page = await app.firstWindow();
    page.on('pageerror', (error) => errors.push(error.message));
    await expectOpaque(page, app);
    expect(await page.evaluate(() => localStorage.getItem('kyro.polished-glass.v1'))).toBe('historical-settings');
    expect(await page.evaluate(() => JSON.parse(localStorage.getItem('kyro.glass-plate.v1')))).toEqual({ glass: true, opacity: 0, blur: 32 });
    await resizeWindow(app, page, 1000, 680);
    await expectOpaque(page, app);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await resizeWindow(app, page, 1440, 900);
    await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].minimize());
    await expect.poll(() => app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].isMinimized())).toBe(true);
    await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].restore());
    await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].maximize());
    await expect.poll(() => app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].isMaximized())).toBe(true);
    await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].unmaximize());
    await expect.poll(() => app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].isMaximized())).toBe(false);
    await resizeWindow(app, page, 1440, 900);
    await page.screenshot({ path: 'test-results/interface.png' });
    await page.evaluate(() => window.open('https://example.com'));
    expect(app.windows()).toHaveLength(1);
    await page.evaluate(() => { window.location.href = 'https://example.com'; });
    expect(page.url()).toMatch(/^file:/);
    expect(errors).toEqual([]);
    await writeFile('test-results/configuration.json', JSON.stringify(configuration, null, 2));
  } finally {
    await app?.close();
    await removeTestProfile(profile);
  }
});

test('minimum viewport at emulated 100% and 200% display scale', async () => {
  for (const scale of [1, 2]) {
    const profile = await mkdtemp(join(tmpdir(), 'kyro-scale-proof-'));
    let app;
    try {
      app = await launch(profile, false, scale);
      const page = await app.firstWindow();
      await page.emulateMedia({ colorScheme: 'light', reducedMotion: 'reduce' });
      await expectOpaque(page, app);
      await resizeWindow(app, page, 1000, 680);
      await expectOpaque(page, app);
      await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
      await expect(page.getByRole('textbox', { name: 'Votre message' })).toBeVisible();
      await expect(page.getByRole('button', { name: 'Saisie vocale', exact: true })).toBeVisible();
      expect(await page.evaluate(() => [...document.querySelectorAll('body *')].filter((element) => {
        const hasText = [...element.childNodes].some((node) => node.nodeType === Node.TEXT_NODE && node.textContent.trim());
        return (hasText || element.matches('textarea')) && element.checkVisibility() && parseFloat(getComputedStyle(element).fontSize) < 14;
      }).map((element) => element.textContent))).toEqual([]);
      await expect(page.getByRole('complementary', { name: 'Explorateur de fichiers' })).not.toBeVisible();
      const railBounds = await page.getByRole('complementary', { name: 'Navigation principale' }).boundingBox();
      const chatBounds = await page.getByRole('region', { name: 'Chat AI', exact: true }).boundingBox();
      const previewBounds = await page.getByRole('main', { name: 'Surface Kyro' }).boundingBox();
      expect(railBounds.x + railBounds.width).toBeLessThanOrEqual(previewBounds.x + 1);
      expect(previewBounds.x + previewBounds.width).toBeLessThanOrEqual(chatBounds.x + 1);
      expect(previewBounds.width).toBeGreaterThan(300);
      const contentHeight = await page.evaluate(() => window.innerHeight);
      const statusBounds = await page.getByRole('contentinfo', { name: 'État du projet' }).boundingBox();
      expect(Math.abs(previewBounds.y + previewBounds.height - statusBounds.y)).toBeLessThanOrEqual(2);
      expect(Math.abs(statusBounds.y + statusBounds.height - contentHeight)).toBeLessThanOrEqual(2);
      const publishBounds = await page.getByRole('button', { name: 'Publier', exact: true }).boundingBox();
      const contentWidth = await page.evaluate(() => window.innerWidth);
      expect(publishBounds.x + publishBounds.width).toBeLessThanOrEqual(contentWidth - 138);
      const startBounds = await page.getByRole('button', { name: 'Décrire mon idée' }).boundingBox();
      expect(startBounds.y + startBounds.height).toBeLessThan(previewBounds.y + previewBounds.height);
      expect(await page.locator('.preview-canvas').evaluate((canvas) => canvas.scrollWidth <= canvas.clientWidth)).toBe(true);
      if (scale === 1) await page.screenshot({ path: '../../docs/suivi/preuves/desktop-refined-compact-20261004.png' });
      await page.locator('summary').filter({ hasText: /^Fichier$/ }).click();
      await page.getByRole('button', { name: 'Afficher l’explorateur' }).click();
      await expect(page.getByRole('complementary', { name: 'Explorateur de fichiers' })).toBeVisible();
      await page.getByRole('button', { name: 'Pages', exact: true }).click();
      await expect(page.getByRole('complementary', { name: 'Explorateur de fichiers' })).not.toBeVisible();
    } finally {
      await app?.close();
      await removeTestProfile(profile);
    }
  }
});

test('checkout menus select folders, create real worktrees and preserve failures', async () => {
  const profile = await mkdtemp(join(tmpdir(), 'kyro-checkout-profile-'));
  const fixture = await mkdtemp(join(tmpdir(), 'kyro-checkout-fixture-'));
  let app;
  try {
    const checkout = join(fixture, 'checkout');
    const worktree = join(fixture, 'feature');
    const ordinary = join(fixture, 'ordinary');
    await mkdir(checkout);
    await mkdir(ordinary);
    await writeFile(join(checkout, 'README.md'), 'main');
    const git = (...args) => execFileSync('git', ['-C', checkout, ...args], { windowsHide: true, encoding: 'utf8' });
    git('init', '-b', 'main');
    git('add', 'README.md');
    git('-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '-m', 'fixture');
    await writeFile(join(checkout, '.git', 'hooks', 'post-checkout'), '#!/bin/sh\nexit 1\n');
    await writeFile(join(checkout, '.gitattributes'), 'README.md filter=proof\n');
    git('add', '.gitattributes');
    git('-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '-m', 'filter fixture');
    git('config', 'filter.proof.smudge', 'exit 1');
    git('config', 'filter.proof.required', 'true');
    await writeFile(join(checkout, 'uncommitted.txt'), 'stay here');
    await writeFile(join(ordinary, 'ordinary.txt'), 'ordinary');
    app = await launch(profile);
    const page = await app.firstWindow();
    expect(await page.locator('.composer textarea').evaluate((element) => getComputedStyle(element).height)).toBe('68px');
    await app.evaluate(({ dialog }, path) => { dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] }); }, checkout);
    await page.locator('.folder-picker summary').click();
    await expect(page.locator('.folder-picker')).toContainText('Nouveau projet…');
    await page.getByRole('button', { name: 'Ouvrir un dossier…', exact: true }).click();
    await page.getByRole('checkbox', { name: 'Faire confiance à ce dossier', exact: false }).check();
    await page.getByRole('button', { name: 'Faire confiance et ouvrir' }).click();
    await expect(page.locator('.checkout-bar')).toContainText('main');
    await expect(page.getByRole('complementary', { name: 'Explorateur de fichiers' })).toContainText('README.md');
    await page.locator('.checkout-picker summary').click();
    await page.getByRole('button', { name: 'Créer un worktree…' }).click();
    await page.getByRole('textbox', { name: 'Nouvelle branche' }).fill('main');
    await page.getByRole('button', { name: 'Choisir l’emplacement' }).click();
    await expect(page.getByRole('alert')).toContainText('existe déjà');
    await page.getByRole('textbox', { name: 'Nouvelle branche' }).fill('../invalid');
    await page.getByRole('button', { name: 'Choisir l’emplacement' }).click();
    await expect(page.getByRole('alert')).toContainText('invalide');
    await page.getByRole('textbox', { name: 'Nouvelle branche' }).fill('feature');
    await app.evaluate(({ dialog }) => { dialog.showSaveDialog = async () => ({ canceled: true }); });
    await page.getByRole('button', { name: 'Choisir l’emplacement' }).click();
    await expect(page.getByRole('button', { name: 'Choisir l’emplacement' })).toBeEnabled();
    expect(git('branch', '--list', 'feature')).toBe('');
    await app.evaluate(({ dialog }, path) => { dialog.showSaveDialog = async () => ({ canceled: false, filePath: path }); }, ordinary);
    await page.getByRole('button', { name: 'Choisir l’emplacement' }).click();
    await expect(page.getByRole('alert')).toContainText('n’existe pas encore');
    await app.evaluate(({ dialog }, path) => { dialog.showSaveDialog = async () => ({ canceled: false, filePath: path }); }, worktree);
    await page.getByRole('button', { name: 'Choisir l’emplacement' }).click();
    await expect(page.getByRole('dialog', { name: 'Créer un worktree' })).not.toBeVisible();
    await expect(page.locator('.checkout-bar')).toContainText('feature');
    expect(git('worktree', 'list', '--porcelain')).toContain('refs/heads/feature');
    expect(await readFile(join(worktree, 'README.md'), 'utf8')).toBe('main');
    await expect(page.getByRole('complementary', { name: 'Explorateur de fichiers' })).not.toContainText('uncommitted.txt');
    await page.locator('.checkout-picker summary').click();
    await page.getByRole('button', { name: 'checkout', exact: false }).click();
    await expect(page.locator('.checkout-picker summary')).toContainText('main');
    await expect(page.locator('.checkout-picker')).not.toHaveAttribute('open');
    await expect(page.getByRole('complementary', { name: 'Explorateur de fichiers' })).toContainText('README.md');
    await page.locator('.checkout-picker summary').click();
    await app.evaluate(({ dialog }, path) => { dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] }); }, ordinary);
    await page.getByRole('button', { name: 'Choisir un worktree existant…' }).click();
    await page.getByRole('checkbox', { name: 'Faire confiance à ce dossier', exact: false }).check();
    await page.getByRole('button', { name: 'Faire confiance et ouvrir' }).click();
    await expect(page.getByRole('alert')).toContainText('pas un worktree Git valide');
    await expect(page.locator('.checkout-bar')).toContainText('main');
    await page.keyboard.press('Escape');
    expect(await page.evaluate(() => window.kyroWorkspace.select('ungranted'))).toMatchObject({ error: 'Choisissez à nouveau ce dossier.' });
    await page.locator('.folder-picker summary').click();
    await page.getByRole('textbox', { name: 'Rechercher un dossier' }).fill('not-found');
    await expect(page.locator('.folder-picker')).toContainText('Aucun résultat');
    await page.keyboard.press('Escape');
    await expect(page.locator('.folder-picker summary')).toBeFocused();
    await expect(page.getByRole('complementary', { name: 'Explorateur de fichiers' })).toContainText('README.md');
    await page.locator('.checkout-picker summary').click();
    await expect(page.getByRole('textbox', { name: 'Rechercher un worktree' })).toHaveValue('');
    await expect(page.locator('.checkout-picker .workspace-options')).toContainText('feature');
    await page.screenshot({ path: 'test-results/worktree-menu.png' });
    await page.getByRole('heading', { name: 'Vos idées, en grand.' }).click();
    await expect(page.locator('.checkout-bar details[open]')).toHaveCount(0);
    expect(await page.locator('button').evaluateAll((buttons) => buttons.filter((button) => button.checkVisibility()).every((button) => getComputedStyle(button).boxShadow.includes('inset')))).toBe(true);
  } finally {
    await app?.close();
    await removeTestProfile(profile);
    await removeTestProfile(fixture);
  }
});

test('development CSS hot reload and occupied port rejection', async () => {
  test.skip(Boolean(process.env.KYRO_TEST_EXE), 'Packaged app never loads a Vite server');
  const profile = await mkdtemp(join(tmpdir(), 'kyro-hmr-proof-'));
  const tokensPath = resolve('src/tokens.css');
  const original = await readFile(tokensPath, 'utf8');
  let app;
  let server;
  let competing;
  try {
    server = await createServer({ logLevel: 'error' });
    await server.listen();
    competing = await createServer({ logLevel: 'silent' });
    await expect(competing.listen()).rejects.toThrow(/Port 5174 is already in use/);
    await competing.close();
    app = await launch(profile, true);
    const page = await app.firstWindow();
    await expectOpaque(page, app);
    expect(page.url()).toBe('http://127.0.0.1:5174/');
    await writeFile(tokensPath, original.replace('--ink: #222627;', '--ink: #112233;'));
    await expect.poll(() => page.evaluate(() => getComputedStyle(document.body).color)).toBe('rgb(17, 34, 51)');
    // Vite's watcher throttles writes on one path for 50 ms.
    await page.waitForTimeout(100);
    await writeFile(tokensPath, original);
    await expect.poll(() => page.evaluate(() => getComputedStyle(document.body).color)).toBe('rgb(34, 38, 39)');
    const appPath = resolve('src/App.tsx');
    const component = await readFile(appPath, 'utf8');
    try {
      await writeFile(appPath, component.replace('aria-label="Surface Kyro"', 'aria-label="Surface Kyro · retouche"'));
      await expect(page.getByRole('main', { name: 'Surface Kyro · retouche' })).toBeVisible();
      await page.waitForTimeout(100);
    } finally { await writeFile(appPath, component); }
    await expectOpaque(page, app);
  } finally {
    await writeFile(tokensPath, original);
    await app?.close();
    await competing?.close();
    await server?.close();
    await removeTestProfile(profile);
  }
});

test('redesign suggestions, mobile navigation, themes and reduced motion', async () => {
  const profile = await mkdtemp(join(tmpdir(), 'kyro-redesign-'));
  let app;
  try {
    app = await launch(profile);
    const page = await app.firstWindow();
    await expectOpaque(page, app);
    const assertTypography = async () => {
      expect(await page.evaluate(() => [...document.querySelectorAll('body *')].filter((element) => {
        const text = [...element.childNodes].some((node) => node.nodeType === Node.TEXT_NODE && node.textContent.trim());
        return (text || element.matches('textarea')) && element.checkVisibility() && parseFloat(getComputedStyle(element).fontSize) < 14;
      }).map((element) => element.textContent))).toEqual([]);
    };
    await assertTypography();
    await page.locator('summary').filter({ hasText: /^Fichier$/ }).click();
    await assertTypography();
    await page.keyboard.press('Escape');
    await page.getByRole('button', { name: 'Publier', exact: true }).click();
    await assertTypography();
    await page.keyboard.press('Escape');
    await page.getByRole('button', { name: 'Un processus à simplifier', exact: false }).click();
    await expect(page.getByRole('textbox', { name: 'Votre message' })).toHaveValue(/les données, les rôles/);
    await expect(page.getByRole('button', { name: 'Nouvelle conversation', exact: true })).toHaveCount(0);
    await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' });
    await expect(page.locator('.app')).toHaveCSS('background-color', 'rgb(27, 32, 29)');
    await expect(page.locator('.start-intro')).toHaveCSS('animation-name', 'none');
    await assertTypography();
    await page.screenshot({ path: '../../docs/suivi/preuves/desktop-refined-dark-20261004.png' });
    await page.emulateMedia({ colorScheme: 'light', reducedMotion: 'reduce' });
    // Isolated test window: lower native minimum only to exercise web breakpoints.
    await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setMinimumSize(320, 500));
    await resizeWindow(app, page, 390, 844);
    // Keep the browser variant through React rerenders; removing a class once is insufficient.
    await page.addInitScript(() => Object.defineProperty(navigator, 'userAgent', { value: navigator.userAgent.replace(/Electron\/\S+\s?/, '') }));
    await page.reload();
    await assertTypography();
    await page.locator('summary').filter({ hasText: /^Fichier$/ }).click();
    await page.getByRole('button', { name: 'Afficher l’explorateur' }).click();
    await expect(page.getByRole('complementary', { name: 'Explorateur de fichiers' })).toBeVisible();
    await page.getByRole('button', { name: 'Pages', exact: true }).click();
    await expect(page.getByRole('complementary', { name: 'Explorateur de fichiers' })).not.toBeVisible();
    await expect(page.getByRole('region', { name: 'Chat AI', exact: true })).toHaveCount(0);
    await expect(page.getByRole('heading', { name: 'Pages', exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Application', exact: true }).click();
    await expect(page.getByRole('region', { name: 'Chat AI', exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    const mobileWidth = await page.evaluate(() => innerWidth);
    const mobileChat = await page.getByRole('region', { name: 'Chat AI', exact: true }).boundingBox();
    expect(mobileChat.x + mobileChat.width).toBeLessThanOrEqual(mobileWidth + 1);
    const mobileComposer = await page.getByRole('textbox', { name: 'Votre message' }).boundingBox();
    expect(mobileComposer.x + mobileComposer.width).toBeLessThanOrEqual(mobileWidth);
    const mobileSend = await page.getByRole('button', { name: 'Envoyer le message' }).boundingBox();
    expect(mobileSend.x + mobileSend.width).toBeLessThanOrEqual(mobileWidth);
    await page.mouse.move(0, 0);
    await page.screenshot({ path: '../../docs/suivi/preuves/desktop-refined-mobile-20261004.png' });
    await page.getByRole('button', { name: 'Structurer mon idée', exact: true }).click();
    await expect(page.getByRole('textbox', { name: 'Votre message' })).toBeFocused();
    await expect(page.getByRole('textbox', { name: 'Votre message' })).toHaveValue('Aide-moi à structurer mon idée d’application.');
    await page.getByRole('textbox', { name: 'Votre message' }).press('Enter');
    await expect(page.getByRole('log')).toContainText('Aide-moi à structurer');
    await assertTypography();
    await page.screenshot({ path: '../../docs/suivi/preuves/desktop-refined-mobile-chat-20261004.png' });
    await resizeWindow(app, page, 320, 700);
    for (const name of ['Publier', 'Envoyer le message']) {
      const control = page.getByRole('button', { name, exact: true });
      const bounds = await control.boundingBox();
      expect(bounds.x).toBeGreaterThanOrEqual(0);
      expect(bounds.x + bounds.width).toBeLessThanOrEqual(321);
      expect(bounds.width).toBeGreaterThanOrEqual(30);
    }
  } finally {
    await app?.close();
    await removeTestProfile(profile);
  }
});

test('creative atelier system reduction and home navigation', async () => {
  const profile = await mkdtemp(join(tmpdir(), 'kyro-creative-motion-'));
  let app;
  try {
    app = await launch(profile);
    const page = await app.firstWindow();
    await page.emulateMedia({ colorScheme: 'light', reducedMotion: 'no-preference' });
    await expect(page.getByRole('button', { name: /animations/i })).toHaveCount(0);
    await expect(page.locator('.panel-heading h1')).toHaveText('Kyro');
    const mascot = page.locator('.idea-mascot');
    await expect.poll(() => mascot.evaluate((image) => image.complete && image.naturalWidth > 0)).toBe(true);
    await expect(mascot).toHaveCSS('animation-name', 'idea-float');
    await page.getByRole('textbox', { name: 'Votre message' }).fill('Une idée de test');
    await page.getByRole('button', { name: 'Données', exact: true }).click();
    await page.getByRole('link', { name: 'Kyro accueil' }).click();
    await expect(page.getByRole('heading', { name: 'Vos idées, en grand.' })).toBeVisible();
    await expect(page.getByRole('textbox', { name: 'Votre message' })).toHaveValue('Une idée de test');
    await page.emulateMedia({ reducedMotion: 'reduce' });
    await expect(mascot).toHaveCSS('animation-name', 'none');
    await page.emulateMedia({ reducedMotion: 'no-preference' });
    await expect(mascot).toHaveCSS('animation-name', 'idea-float');
    await page.getByRole('textbox', { name: 'Votre message' }).fill('');
    await page.screenshot({ path: '../../docs/suivi/preuves/desktop-refined-electron-20261004.png' });
  } finally {
    await app?.close();
    await removeTestProfile(profile);
  }
});
