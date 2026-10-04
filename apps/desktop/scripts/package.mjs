import { packager } from '@electron/packager';
import { spawnSync } from 'node:child_process';
import { basename, dirname, resolve } from 'node:path';
import { readFileSync, existsSync } from 'node:fs';

const manifest = JSON.parse(readFileSync('package.json', 'utf8'));
const electronVersion = manifest.devDependencies.electron;
const paths = await packager({
  dir: '.', name: 'Kyro', platform: 'win32', arch: 'x64', electronVersion,
  out: 'out', overwrite: true, asar: true, prune: true,
  extraResource: [resolve('runtime/mingit')],
  // Reuse a locally verified official archive when present, otherwise use Packager's downloader.
  ...(existsSync(`node_modules/electron/electron-v${electronVersion}-win32-x64.zip`) ? { electronZipDir: resolve('node_modules/electron') } : {}),
  ignore: [/^\/runtime($|\/)/, /^\/node_modules($|\/)/, /^\/src($|\/)/, /^\/scripts($|\/)/, /^\/tests($|\/)/, /^\/test-results($|\/)/, /^\/playwright-report($|\/)/, /^\/playwright\.config/, /^\/vite\.config/, /^\/tsconfig/, /^\/README\.md/, /^\/package-lock\.json/],
});
for (const directory of paths) {
  const zip = resolve('out', `${basename(directory)}.zip`);
  const result = spawnSync('tar.exe', ['-a', '-cf', zip, '-C', dirname(directory), basename(directory)], { stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`Archive failed: ${result.status}`);
  console.log(`Executable: ${resolve(directory, 'Kyro.exe')}\nArchive: ${zip}`);
}
