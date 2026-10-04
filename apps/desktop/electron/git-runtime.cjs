const { join } = require('node:path');
const { existsSync } = require('node:fs');
function gitExecutable() {
  const bundled = process.resourcesPath && join(process.resourcesPath, 'mingit', 'cmd', 'git.exe');
  const development = join(__dirname, '../runtime/mingit/cmd/git.exe');
  if (bundled && existsSync(bundled)) return bundled;
  if (existsSync(development)) return development;
  throw new Error('Git intégré est incomplet. Réparez ou reconstruisez Kyro.');
}
module.exports = { gitExecutable };
