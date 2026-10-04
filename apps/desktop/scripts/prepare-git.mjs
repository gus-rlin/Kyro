import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
const run = promisify(execFile);
const root = fileURLToPath(new URL('../', import.meta.url));
const manifest = JSON.parse(await readFile(new URL('../git-runtime.json', import.meta.url), 'utf8'));
const destination = `${root}runtime/mingit`;
const archive = `${root}runtime/mingit.zip`;
await mkdir(`${root}runtime`, { recursive: true });
const env = { ...process.env, KYRO_GIT_URL: manifest.url, KYRO_GIT_ARCHIVE: archive, KYRO_GIT_DESTINATION: destination };
let bytes = await readFile(archive).catch(() => null);
if (!bytes || createHash('sha256').update(bytes).digest('hex') !== manifest.sha256) {
  await run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', "$ErrorActionPreference='Stop'; Invoke-WebRequest -Uri $env:KYRO_GIT_URL -OutFile $env:KYRO_GIT_ARCHIVE"], { env, windowsHide: true, timeout: 180000 });
  bytes = await readFile(archive);
}
if (createHash('sha256').update(bytes).digest('hex') !== manifest.sha256) throw new Error('MinGit checksum mismatch; archive not extracted.');
await run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', "$ErrorActionPreference='Stop'; Expand-Archive -LiteralPath $env:KYRO_GIT_ARCHIVE -DestinationPath $env:KYRO_GIT_DESTINATION -Force"], { env, windowsHide: true, timeout: 180000 });
const { stdout } = await run(`${destination}/cmd/git.exe`, ['--version'], { windowsHide: true });
if (stdout.trim() !== `git version ${manifest.version}`) throw new Error('Unexpected bundled Git version');
await writeFile(`${destination}/kyro-runtime.json`, JSON.stringify(manifest, null, 2));
console.log(`${stdout.trim()} — archive SHA-256 verified; private Kyro runtime ready.`);
