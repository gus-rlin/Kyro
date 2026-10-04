const { execFile } = require('node:child_process');
const { promisify } = require('node:util');
const { readdir, realpath, lstat, mkdir, writeFile } = require('node:fs/promises');
const { join, basename, dirname } = require('node:path');
const { randomUUID } = require('node:crypto');
const execute = promisify(execFile);
const { gitExecutable } = require('./git-runtime.cjs');

// Only native picker grants and Git's own worktree list become opaque capabilities.
function createWorkspaceService(dialog, getWindow, getDocuments) {
  const grants = new Map();
  const pending = new Map();
  let busy = false;
  const git = async (cwd, args) => (await execute(gitExecutable(), ['-c', 'core.fsmonitor=false', '-c', 'core.hooksPath=', '-c', 'core.attributesFile=', '-C', cwd, ...args], {
    windowsHide: true, timeout: 60000, maxBuffer: 2 * 1024 * 1024,
    env: Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.toUpperCase().startsWith('GIT_'))),
  })).stdout.trim();
  function pathFor(id) {
    if (typeof id !== 'string' || !grants.has(id)) throw new Error('Choisissez à nouveau ce dossier.');
    return grants.get(id);
  }
  function offer(path, purpose, name = basename(path), parent) {
    if (pending.size >= 16) pending.delete(pending.keys().next().value);
    const id = randomUUID();
    const item = { id, name, path, purpose, parent };
    pending.set(id, item);
    return item;
  }
  function approval(id, trusted, purpose) {
    const item = pending.get(id);
    if (trusted !== true || !item || (purpose && item.purpose !== purpose)) throw new Error('Confirmez votre confiance pour ce dossier.');
    return item;
  }
  async function describe(path) {
    path = await realpath(path);
    let id = [...grants].find(([, value]) => value === path)?.[0];
    if (!id) { id = randomUUID(); grants.set(id, path); }
    let kind = 'folder';
    let branch;
    // Do not silently treat a subdirectory as its parent repository.
    try {
      const marker = await lstat(join(path, '.git'));
      if (!marker.isSymbolicLink()) {
        await git(path, ['rev-parse', '--git-dir']);
        kind = marker.isFile() ? 'worktree' : 'checkout';
        branch = await git(path, ['symbolic-ref', '--short', '-q', 'HEAD']).catch(() => 'HEAD détachée');
      }
    } catch { /* Ordinary folders remain usable without Git installed. */ }
    return { id, name: basename(path), path, kind, branch };
  }
  async function select(id) {
    const path = pathFor(id);
    const info = await describe(path);
    const entries = [];
    let truncated = false;
    async function walk(directory, prefix, depth) {
      if (depth > 12) { truncated = true; return; }
      for (const entry of await readdir(directory, { withFileTypes: true })) {
        if (['.git', 'node_modules', 'target'].includes(entry.name) || entry.isSymbolicLink()) continue;
        if (entries.length >= 1500) { truncated = true; return; }
        const relative = `${prefix}/${entry.name}`;
        entries.push({ path: relative, directory: entry.isDirectory() });
        if (entry.isDirectory()) await walk(join(directory, entry.name), relative, depth + 1);
      }
    }
    await walk(path, info.name, 0);
    return { ...info, entries, truncated };
  }
  async function list(id) {
    const path = pathFor(id);
    const output = await git(path, ['worktree', 'list', '--porcelain', '-z']);
    const paths = output.split('\0').filter((line) => line.startsWith('worktree ')).map((line) => line.slice(9));
    const items = [];
    for (const item of paths) {
      try { items.push(await describe(item)); } catch { /* Prunable paths are not selectable. */ }
    }
    return items;
  }
  return {
    select, list,
    async branches(id) {
      const output = await git(pathFor(id), ['for-each-ref', '--sort=refname', '--format=%(refname)%00%(HEAD)%00%(symref)', 'refs/heads/', 'refs/remotes/']);
      return output.split('\n').filter(Boolean).flatMap((line) => {
        const [ref, head, symbolic] = line.split('\0');
        if (symbolic) return [];
        const remote = ref.startsWith('refs/remotes/');
        return [{ name: ref.slice(remote ? 13 : 11), remote, current: head === '*' }];
      });
    },
    discard(id) { pending.delete(id); return null; },
    async prepareProject(name) {
      if (typeof name !== 'string' || name.length > 64 || !/^[\p{L}\p{N}][\p{L}\p{N} ._-]*$/u.test(name) || /[. ]$/.test(name) || /^(con|prn|aux|nul|com[0-9]|lpt[0-9])(?:\.|$)/i.test(name)) throw new Error('Utilisez un nom de dossier valide, de 1 à 64 caractères.');
      const parent = await realpath(await getDocuments());
      const path = join(parent, name);
      try { await lstat(path); throw new Error('Un dossier porte déjà ce nom à cet emplacement.'); }
      catch (error) { if (error.code !== 'ENOENT') throw error; }
      return offer(path, 'project', name, parent);
    },
    async createProject(id, trusted) {
      const plan = approval(id, trusted, 'project');
      if (busy) throw new Error('Une création est déjà en cours.');
      busy = true;
      let created = false;
      try {
        if (await realpath(plan.parent) !== plan.parent) throw new Error('L’emplacement a changé. Choisissez-le à nouveau.');
        await git(plan.parent, ['--version']);
        await mkdir(plan.path); // Atomic: never overwrite or adopt an existing directory.
        created = true;
        pending.delete(id);
        await mkdir(join(plan.path, '.kyro'));
        await writeFile(join(plan.path, '.kyro', 'project.json'), JSON.stringify({ version: 1, name: plan.name }, null, 2) + '\n', { flag: 'wx' });
        await writeFile(join(plan.path, 'README.md'), `# ${plan.name}\n\nProjet créé avec Kyro.\n`, { flag: 'wx' });
        await writeFile(join(plan.path, '.gitignore'), 'node_modules/\ntarget/\n.env\n.env.*\n!.env.example\n', { flag: 'wx' });
        await git(plan.path, ['init', '--initial-branch=main', '--template=']);
        await git(plan.path, ['add', '--', '.kyro/project.json', 'README.md', '.gitignore']);
        await git(plan.path, ['-c', 'user.name=Kyro', '-c', 'user.email=local@kyro.invalid', '-c', 'commit.gpgsign=false', 'commit', '-m', 'Initialiser le projet Kyro']);
        const info = await describe(plan.path);
        return select(info.id);
      } catch (error) {
        if (created) throw new Error('Le dossier a été créé, mais son initialisation a échoué. Vos fichiers sont conservés ; ouvrez ce dossier pour le vérifier avant de réessayer.');
        if (error.code === 'EEXIST') throw new Error('Ce dossier existe déjà. Aucun fichier n’a été remplacé.');
        throw error;
      } finally { busy = false; }
    },
    async trust(id, trusted) {
      const plan = approval(id, trusted);
      if (plan.purpose === 'project') throw new Error('Utilisez la confirmation de création du projet.');
      if (await realpath(plan.path) !== plan.path) throw new Error('Le dossier a changé. Choisissez-le à nouveau.');
      const info = await describe(plan.path);
      if (plan.purpose === 'worktree' && info.kind !== 'worktree') { grants.delete(info.id); throw new Error('Ce dossier n’est pas un worktree Git valide.'); }
      pending.delete(id);
      return select(info.id);
    },
    async choose(purpose) {
      if (!['folder', 'worktree'].includes(purpose)) throw new Error('Choix de dossier invalide.');
      const result = await dialog.showOpenDialog(getWindow(), { title: purpose === 'worktree' ? 'Choisir un worktree' : 'Choisir un dossier', properties: ['openDirectory'] });
      if (result.canceled || !result.filePaths[0]) return null;
      return offer(await realpath(result.filePaths[0]), purpose);
    },
    async create(id, branch) {
      if (busy) throw new Error('Une création est déjà en cours.');
      busy = true;
      try {
        const path = pathFor(id);
        if (typeof branch !== 'string' || branch.length > 120 || !/^[a-zA-Z0-9][a-zA-Z0-9._/-]*$/.test(branch)) throw new Error('Nom de branche invalide.');
        await git(path, ['check-ref-format', '--branch', branch]);
        await git(path, ['rev-parse', '--verify', 'HEAD']);
        const exists = await git(path, ['show-ref', '--verify', '--quiet', `refs/heads/${branch}`]).then(() => true, () => false);
        if (exists) throw new Error('Cette branche existe déjà. Choisissez un autre nom.');
        const result = await dialog.showSaveDialog(getWindow(), { title: 'Emplacement du nouveau worktree', buttonLabel: 'Créer ici', defaultPath: join(dirname(path), branch.replaceAll('/', '-')) });
        if (result.canceled || !result.filePath) return null;
        try { await lstat(result.filePath); throw new Error('Choisissez un emplacement qui n’existe pas encore.'); }
        catch (error) { if (error.code !== 'ENOENT') throw error; }
        // Checkout must not run repository-provided hooks, filters or submodules.
        const filters = await git(path, ['config', '--name-only', '--get-regexp', '^filter\\.']).catch(() => '');
        const overrides = filters.split('\n').filter(Boolean).flatMap((key) => ['-c', `${key}=${key.endsWith('.required') ? 'false' : ''}`]);
        await git(path, [...overrides, '-c', 'submodule.recurse=false', 'worktree', 'add', '-b', branch, '--', result.filePath, 'HEAD']);
        const info = await describe(result.filePath);
        return select(info.id);
      } finally { busy = false; }
    },
  };
}
module.exports = { createWorkspaceService };
