import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import electron from 'electron';
import service from '../electron/workspaces.cjs';
const run = promisify(execFile);
const script = fileURLToPath(new URL('./native-dialog.cjs', import.meta.url));
async function pick(kind, options) {
  // Electron must run as an application, even if the caller inherited Node mode.
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => key.toUpperCase() !== 'ELECTRON_RUN_AS_NODE'));
  const { stdout } = await run(electron, [script, kind], {
    windowsHide: true, timeout: 300000, encoding: 'utf8', maxBuffer: 16384,
    env: { ...env, KYRO_DIALOG_TITLE: options.title, KYRO_DIALOG_PATH: options.defaultPath || '', KYRO_DIALOG_BUTTON: options.buttonLabel || '' },
  });
  const result = JSON.parse(stdout);
  if (typeof result.path !== 'string') throw new Error('Réponse du sélecteur système invalide.');
  return result.path;
}
const dialog = {
  async showOpenDialog(_window, options) { const path = await pick('folder', options); return { canceled: !path, filePaths: path ? [path] : [] }; },
  async showSaveDialog(_window, options) { const path = await pick('save', options); return { canceled: !path, filePath: path }; },
};
// Development only. Never expose the filesystem service to a network interface.
async function documentsDirectory() {
  const { stdout } = await run('powershell.exe', ['-NoProfile', '-Command', "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; [Environment]::GetFolderPath('MyDocuments')"], { windowsHide: true, timeout: 10000, encoding: 'utf8' });
  const path = stdout.trim();
  if (!path) throw new Error('Le dossier Documents est introuvable sur cet ordinateur.');
  return path;
}
export function nativeDevelopment(nativeDialog = dialog, getDocuments = documentsDirectory) {
  return {
    name: 'kyro-native-development', apply: 'serve',
    configureServer(server) {
      const workspace = service.createWorkspaceService(nativeDialog, () => null, getDocuments);
      const actions = new Set(['choose', 'select', 'list', 'branches', 'create', 'prepareProject', 'createProject', 'trust', 'discard']);
      server.middlewares.use('/__kyro_native', async (req, res) => {
        res.setHeader('Cache-Control', 'no-store');
        res.setHeader('Content-Type', 'application/json; charset=utf-8');
        // Origin + custom header + strict host reject cross-site forms, fetches and DNS rebinding.
        if (req.method !== 'POST' || req.headers.host !== '127.0.0.1:5174' || req.headers.origin !== 'http://127.0.0.1:5174' || req.headers['x-kyro-local'] !== '1' || req.headers['content-type'] !== 'application/json' || !['127.0.0.1', '::1', '::ffff:127.0.0.1'].includes(req.socket.remoteAddress)) {
          res.statusCode = 403; res.end(JSON.stringify({ error: 'Accès local refusé.' })); return;
        }
        try {
          const action = req.url?.slice(1);
          if (!actions.has(action)) { res.statusCode = 404; res.end('{}'); return; }
          let body = '';
          for await (const chunk of req) {
            body += chunk;
            if (Buffer.byteLength(body) > 4096) { res.statusCode = 413; res.end('{}'); return; }
          }
          const args = JSON.parse(body);
          if (!Array.isArray(args) || args.length > 2) throw new Error('Requête invalide.');
          res.end(JSON.stringify({ value: await workspace[action](...args) }));
        } catch (error) {
          res.end(JSON.stringify({ error: error.cmd ? 'L’opération locale a échoué. Vérifiez le dossier puis réessayez.' : error.message }));
        }
      });
    },
  };
}
