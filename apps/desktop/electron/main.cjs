const { app, BrowserWindow, nativeTheme, session, dialog, ipcMain } = require('electron');
const { join } = require('node:path');
const { pathToFileURL } = require('node:url');
const { createWorkspaceService } = require('./workspaces.cjs');
const { createChatService } = require('./chat.cjs');
const { registerChatIpc } = require('./chat-ipc.cjs');

// Profiles supplied by the test runner keep smoke tests out of the user's preferences.
const profile = process.argv.find((value) => value.startsWith('--kyro-profile='));
if (profile) app.setPath('userData', profile.slice('--kyro-profile='.length));
app.setName('Kyro');
nativeTheme.themeSource = 'light';

const devUrl = !app.isPackaged && process.env.KYRO_DESKTOP_DEV === '1'
  ? 'http://127.0.0.1:5174' : null;
let window;
const chat = createChatService();
let closingChat = false;
app.on('before-quit', (event) => {
  if (closingChat) return;
  event.preventDefault(); closingChat = true;
  chat.close().catch(()=>{}).finally(()=>app.quit());
});

if (!app.requestSingleInstanceLock()) app.quit();
else {
  app.on('second-instance', () => {
    if (!window) return;
    if (window.isMinimized()) window.restore();
    window.focus();
  });

  app.whenReady().then(() => {
    registerChatIpc(ipcMain, chat, (event) => {
      const expected = devUrl ? `${devUrl}/` : pathToFileURL(join(__dirname, '../dist/index.html')).href;
      if (!window || event.sender !== window.webContents || event.senderFrame !== window.webContents.mainFrame || event.senderFrame.url.split('#')[0] !== expected) throw new Error('Accès refusé.');
    });
    const workspaces = createWorkspaceService(dialog, () => window, () => app.getPath('documents'));
    for (const action of ['choose', 'select', 'list', 'branches', 'create', 'prepareProject', 'createProject', 'trust', 'discard']) {
      ipcMain.handle(`workspace:${action}`, async (event, ...args) => {
        const expected = devUrl ? `${devUrl}/` : pathToFileURL(join(__dirname, '../dist/index.html')).href;
        if (!window || event.sender !== window.webContents || event.senderFrame !== window.webContents.mainFrame || event.senderFrame.url.split('#')[0] !== expected) throw new Error('Accès refusé.');
        try { return { value: await workspaces[action](...args) }; }
        catch (error) {
          // Git stderr can contain paths and config values; expose an actionable summary only.
          return { error: error.code === 'ENOENT' ? 'Git ou le dossier est introuvable.' : error.cmd ? 'Git a refusé l’opération. Vérifiez le dépôt, son premier commit, la branche et les droits du dossier. Si une création a été interrompue, vérifiez les worktrees avant de réessayer.' : error.message };
        }
      });
    }
    session.defaultSession.setPermissionRequestHandler((_contents, _permission, callback) => callback(false));
    session.defaultSession.setPermissionCheckHandler(() => false);
    window = new BrowserWindow({
      width: 1440, height: 900, minWidth: 1000, minHeight: 680,
      title: 'Kyro', show: false, backgroundColor: '#f0f1ed',
      backgroundMaterial: 'none', transparent: false,
      titleBarStyle: 'hidden',
      titleBarOverlay: { color: '#fafbf8', symbolColor: '#222627', height: 64 },
      webPreferences: {
        nodeIntegration: false, contextIsolation: true, sandbox: true,
        preload: join(__dirname, 'preload.cjs'),
      },
    });
    window.removeMenu();
    window.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
    window.webContents.on('will-navigate', (event) => event.preventDefault());
    window.webContents.on('will-attach-webview', (event) => event.preventDefault());
    window.webContents.on('will-prevent-unload', (event) => event.preventDefault());
    window.once('ready-to-show', () => window.show());
    window.on('closed', () => { window = null; });
    (devUrl ? window.loadURL(devUrl) : window.loadFile(join(__dirname, '../dist/index.html')))
      // Load completion provides a deterministic fallback when ready-to-show is not emitted.
      .then(() => { if (window && !window.isDestroyed()) window.show(); })
      .catch((error) => { console.error('Kyro failed to load:', error.message); app.exit(1); });
  });
  app.on('window-all-closed', () => app.quit());
}
