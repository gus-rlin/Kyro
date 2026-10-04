import { useEffect, useRef, useState, type FormEvent } from 'react';
import { CaretDown, Check, Folder, FolderOpen, GitBranch, MagnifyingGlass, Plus, X } from '@phosphor-icons/react';
import type { Workspace } from './App';
import { workspaceApi } from './workspace-api';

export function WorkspacePicker({ selected, recent, openDirectory, select, acceptNative, newProject }: {
  selected: Workspace | null; recent: Workspace[];
  openDirectory: (purpose: 'folder' | 'worktree') => void;
  select: (workspace: Workspace) => void;
  acceptNative: (workspace: NativeWorkspace) => void;
  newProject: () => void;
}) {
  const container = useRef<HTMLDivElement>(null);
  const creation = useRef<HTMLDialogElement>(null);
  const [view, setView] = useState<'worktrees' | 'branches'>('worktrees');
  const [branches, setBranches] = useState<WorkspaceBranch[]>([]);
  const [revision, setRevision] = useState(0);
  const [query, setQuery] = useState('');
  const [worktrees, setWorktrees] = useState<NativeWorkspace[]>([]);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const [branch, setBranch] = useState('');
  const [createError, setCreateError] = useState('');
  const native = workspaceApi;
  const canCreate = Boolean(native && selected?.native && selected.kind !== 'folder');

  useEffect(() => {
    let active = true;
    setWorktrees([]); setBranches([]); setError('');
    if (!native || !selected?.native || selected.kind === 'folder') { setLoading(false); return; }
    setLoading(true);
    Promise.all([native.list(selected.id), native.branches(selected.id)]).then(([result, refs]) => {
      if (!active) return;
      setWorktrees(result.value || []); setBranches(refs.value || []); setError(result.error || refs.error || ''); setLoading(false);
    }).catch(() => { if (active) { setError('Impossible de charger les worktrees.'); setLoading(false); } });
    return () => { active = false; };
  }, [selected, native, revision]);

  useEffect(() => {
    const close = (event: PointerEvent) => {
      if (!container.current?.contains(event.target as Node)) container.current?.querySelectorAll('details[open]').forEach((item) => item.removeAttribute('open'));
    };
    document.addEventListener('pointerdown', close);
    return () => document.removeEventListener('pointerdown', close);
  }, []);
  function close() { container.current?.querySelectorAll('details[open]').forEach((item) => item.removeAttribute('open')); }
  async function choose(item: Workspace | NativeWorkspace) {
    if (busy) return;
    if ('tree' in item && !item.native) { select(item); close(); return; }
    setBusy(true); setError('');
    try {
      const result = await native!.select(item.id);
      if (result.error) setError(result.error);
      else if (result.value) { acceptNative(result.value); close(); }
    } catch { setError('Le dossier est inaccessible. Choisissez-le à nouveau.'); }
    finally { setBusy(false); }
  }
  async function create(event: FormEvent) {
    event.preventDefault();
    if (!canCreate || busy) return;
    setBusy(true); setCreateError('');
    try {
      const result = await native!.create(selected!.id, branch.trim());
      if (result.error) setCreateError(result.error);
      else if (result.value) { acceptNative(result.value); creation.current?.close(); }
    } catch { setCreateError('La création a échoué. Vérifiez le dépôt avant de réessayer.'); }
    finally { setBusy(false); }
  }
  const matches = (item: { name: string; branch?: string }) => `${item.name} ${item.branch || ''}`.toLocaleLowerCase().includes(query.toLocaleLowerCase());
  const choices = native && selected?.native ? worktrees : recent.filter((item) => item.kind !== 'folder');
  function rows(items: (Workspace | NativeWorkspace)[]) {
    const filtered = items.filter(matches);
    return filtered.length ? <div className="workspace-options">{filtered.map((item) => <button type="button" key={item.id} disabled={busy} aria-pressed={item.id === selected?.id} onClick={() => void choose(item)}>
      {item.kind === 'folder' ? <Folder size={18} /> : <GitBranch size={18} />}<span><strong>{item.name}</strong><small>{item.branch || (item.kind === 'worktree' ? 'Worktree Git' : item.kind === 'checkout' ? 'Dépôt Git' : 'Dossier local')}</small></span>{item.id === selected?.id && <Check size={17} className="workspace-check" />}
    </button>)}</div> : <p className="workspace-empty">{query ? 'Aucun résultat pour cette recherche.' : 'Aucun emplacement dans cette liste.'}</p>;
  }
  function search(label: string) { return <label className="workspace-search"><MagnifyingGlass size={17} /><input aria-label={label} placeholder="Rechercher…" value={query} onChange={(event) => setQuery(event.target.value)} /></label>; }
  return <div ref={container} className="checkout-bar" aria-label="Dossier et worktree" onKeyDown={(event) => {
    if (event.key === 'Escape') { const summary = event.target instanceof Element ? event.target.closest('details')?.querySelector('summary') : null; close(); summary?.focus(); event.stopPropagation(); }
  }}>
    <details className="folder-picker" name="workspace-picker" onToggle={(event) => { if (event.currentTarget.open) setQuery(''); }}>
      <summary className="checkout-folder" aria-label="Choisir un dossier"><Folder size={18} /><span>{selected?.name || 'Dossier local'}</span><CaretDown size={14} /></summary>
      <div className="checkout-menu" aria-label="Dossiers"><header><strong>Vos projets</strong><span>Un espace pour chaque idée.</span></header>{recent.length > 0 ? <>{search('Rechercher un dossier')}{rows(recent)}</> : <div className="workspace-first-project"><span><Folder size={23} /></span><strong>Faites place à votre prochaine idée.</strong><p>Créez son dossier ou retrouvez un projet existant.</p></div>}
        <div className="workspace-actions"><button type="button" className="workspace-create" onClick={() => { close(); newProject(); }}><Plus size={18} /><span>Nouveau projet…</span></button><button type="button" onClick={() => { close(); openDirectory('folder'); }}><FolderOpen size={18} /><span>Ouvrir un dossier…</span></button></div>
        {error && <p className="workspace-error" role="alert">{error}</p>}
      </div>
    </details>
    <details className="checkout-picker" name="workspace-picker" onToggle={(event) => { if (event.currentTarget.open) { setQuery(''); setRevision((value) => value + 1); } }}>
      <summary aria-label="Choisir un worktree"><GitBranch size={18} /><span>{selected?.branch || 'Worktree'}</span><CaretDown size={14} /></summary>
      <div className="checkout-menu" aria-label="Worktrees"><header><strong>Branches et worktrees</strong><span>{selected?.name || 'Les versions de votre projet'}</span></header>
        <div className="workspace-views" aria-label="Afficher"><button type="button" aria-pressed={view === 'worktrees'} onClick={() => { setView('worktrees'); setQuery(''); }}>Worktrees</button><button type="button" aria-pressed={view === 'branches'} onClick={() => { setView('branches'); setQuery(''); }}>Branches</button></div>
        {canCreate && search(view === 'branches' ? 'Rechercher une branche' : 'Rechercher un worktree')}
        {loading ? <p className="workspace-empty" role="status">Chargement…</p> : !canCreate ? <div className="workspace-first-project"><span><GitBranch size={23} /></span><strong>Ouvrez un projet pour commencer.</strong><p>Ses branches et ses espaces de travail apparaîtront ici.</p></div> : view === 'worktrees' ? rows(choices) : <><p className="workspace-branch-hint">Branches locales et distantes connues du projet.</p><div className="workspace-options" aria-label="Liste des branches">{branches.filter(matches).map((item) => <div className="workspace-branch" key={`${item.remote}:${item.name}`}><GitBranch size={18} /><span><strong>{item.name}</strong><small>{item.remote ? 'Distante' : 'Locale'}</small></span>{item.current && <span className="branch-current">Actuelle</span>}</div>)}{!branches.filter(matches).length && <p className="workspace-empty">{query ? 'Aucune branche pour cette recherche.' : 'Aucune branche disponible.'}</p>}</div></>}
        {error && <p className="workspace-error" role="alert">{error}</p>}
        <div className="workspace-actions"><button type="button" onClick={() => { close(); openDirectory('worktree'); }}><FolderOpen size={18} /><span>Choisir un worktree existant…</span></button>
          {!canCreate && <button type="button" onClick={() => { close(); openDirectory('folder'); }}><FolderOpen size={18} /><span>Ouvrir un projet…</span></button>}<button type="button" className="workspace-create" disabled={!canCreate} title={!canCreate ? 'Ouvrez un projet pour créer un worktree' : undefined} onClick={() => { close(); setBranch(''); setCreateError(''); creation.current?.showModal(); }}><Plus size={18} /><span>Créer un worktree…</span></button></div>
      </div>
    </details>
    <dialog ref={creation} className="notice-dialog worktree-dialog" aria-labelledby="worktree-title" onCancel={(event) => { if (busy) event.preventDefault(); }}>
      <div className="dialog-header"><h2 id="worktree-title">Créer un worktree</h2><button type="button" className="icon-button" aria-label="Fermer" disabled={busy} onClick={() => creation.current?.close()}><X size={19} /></button></div>
      {canCreate ? <form onSubmit={create}><p>Un dossier séparé pour votre nouvelle branche, à partir du dernier commit de <strong>{selected?.branch || selected?.name}</strong>. Vos modifications non commitées restent ici.</p>
        <label className="branch-field">Nouvelle branche<input autoFocus required maxLength={120} placeholder="ex. feature/mon-idee" value={branch} onChange={(event) => setBranch(event.target.value)} disabled={busy} /></label>
        <p className="creation-hint">Choisissez ensuite un nouvel emplacement dans le sélecteur système.</p>{createError && <p className="workspace-error" role="alert">{createError}</p>}
        <div className="dialog-actions"><button type="button" disabled={busy} onClick={() => creation.current?.close()}>Annuler</button><button className="dialog-confirm" type="submit" disabled={busy || !branch.trim()}>{busy ? 'Création en cours…' : 'Choisir l’emplacement'}</button></div>
      </form> : null}
    </dialog>
  </div>;
}
