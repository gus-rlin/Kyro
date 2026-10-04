import { createServer } from 'vite';
import { spawn } from 'node:child_process';
import electron from 'electron';
import { fileURLToPath } from 'node:url';

process.chdir(fileURLToPath(new URL('..', import.meta.url)));
let server;
let child;
let stopping = false;
async function stop(code = 0) {
  if (stopping) return;
  stopping = true;
  if (child && child.exitCode === null) child.kill();
  await server?.close();
  process.exitCode = code;
}
try {
  server = await createServer();
  await server.listen();
  server.printUrls();
  child = spawn(electron, ['.'], { stdio: 'inherit', env: { ...process.env, KYRO_DESKTOP_DEV: '1' } });
  child.on('error', (error) => { console.error(error.message); void stop(1); });
  child.on('exit', (code) => void stop(code ?? 1));
  process.once('SIGINT', () => void stop());
  process.once('SIGTERM', () => void stop());
} catch (error) {
  console.error(`Kyro dev: ${error.message}`);
  await stop(1);
}
