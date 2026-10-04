import { useEffect, useRef, useState, type FormEvent } from 'react';
import { ArrowRight, Check, Folder, HardDrives, ShieldCheck, X } from '@phosphor-icons/react';
import { workspaceApi } from './workspace-api';

export type ProjectRequest = { kind: 'new' } | { kind: 'trust'; plan: WorkspacePlan };
export function ProjectFlow({ request, onClose, onReady }: { request: ProjectRequest | null; onClose: () => void; onReady: (workspace: NativeWorkspace) => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const title = useRef<HTMLHeadingElement>(null);
  const trigger = useRef<HTMLElement | null>(null);
  const [name, setName] = useState('');
  const [plan, setPlan] = useState<WorkspacePlan | null>(null);
  const [trusted, setTrusted] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const creating = request?.kind === 'new';
  useEffect(() => {
    if (!request) { dialog.current?.close(); trigger.current?.focus(); return; }
    const active = document.activeElement as HTMLElement;
    trigger.current = active.closest('details')?.querySelector('summary') || active;
    setName(''); setPlan(request.kind === 'trust' ? request.plan : null); setTrusted(false); setError('');
    dialog.current?.showModal();
    if (request.kind === 'new') input.current?.focus();
  }, [request]);
  useEffect(() => {
    if (!request) return;
    if (plan) title.current?.focus(); else input.current?.focus();
  }, [plan, request]);
  function cancel() {
    if (busy) return;
    if (plan) void workspaceApi?.discard(plan.id);
    onClose();
  }
  async function proceed(event: FormEvent) {
    event.preventDefault();
    if (busy || (plan && !trusted)) return;
    setBusy(true); setError('');
    try {
      if (!workspaceApi) throw new Error('Ouvrez ce projet depuis Kyro pour accéder à vos dossiers.');
      if (!plan) {
        const result = await workspaceApi.prepareProject(name.trim());
        if (result.error) throw new Error(result.error);
        if (result.value) { setPlan(result.value); setTrusted(false); }
      } else {
        const result = creating ? await workspaceApi.createProject(plan.id, trusted) : await workspaceApi.trust(plan.id, trusted);
        if (result.error) throw new Error(result.error);
        if (result.value) { onReady(result.value); onClose(); }
      }
    } catch (error) { setError(error instanceof Error ? error.message : 'Impossible de préparer le projet. Réessayez.'); }
    finally { setBusy(false); }
  }
  return <dialog ref={dialog} className="project-dialog" aria-labelledby="project-flow-title" onCancel={(event) => { event.preventDefault(); cancel(); }}>
    <header className="project-flow-top"><span className="project-flow-brand"><span className="mini-brand">k</span> Votre espace de création</span><button className="icon-button" aria-label="Fermer le parcours" disabled={busy} onClick={cancel}><X size={20} /></button></header>
    <form onSubmit={proceed} aria-busy={busy}>
      {creating && <ol className="project-steps" aria-label="Étapes de création"><li aria-current={!plan ? 'step' : undefined}><span>{plan ? <Check size={13} weight="bold" /> : '1'}</span>Le projet</li><li aria-current={plan ? 'step' : undefined}><span>2</span>Confirmation</li></ol>}
      <div className="project-flow-copy"><h2 ref={title} tabIndex={-1} id="project-flow-title">{!plan ? 'Une idée. Un nouveau projet.' : creating ? 'Un espace pour ' + plan.name + '.' : 'Ouvrir ' + plan.name + ' ?'}</h2><p>{!plan ? 'Donnez-lui un nom. Kyro s’occupe de préparer son dossier et son historique.' : creating ? 'Tout commence ici, sur votre ordinateur.' : 'Vous gardez le contrôle sur les dossiers auxquels Kyro a accès.'}</p></div>
      {!plan ? <>
        <label className="project-name-field">Nom du projet<input ref={input} required maxLength={64} autoComplete="off" placeholder="Mon prochain projet" value={name} onChange={(event) => setName(event.target.value)} disabled={busy} aria-describedby="project-name-hint" /></label>
        <p id="project-name-hint" className="project-field-hint">Votre dossier sera créé automatiquement dans Documents.</p>
      </> : <>
        <div className="project-destination"><span className="project-destination-icon"><Folder size={24} /></span><div><strong>{plan.name}</strong><p title={plan.path}>{plan.path}</p></div>{creating && <button type="button" disabled={busy} onClick={() => { void workspaceApi?.discard(plan.id); setPlan(null); setError(''); }}>Modifier le nom</button>}</div>
        {creating && <div className="project-ready-list"><span><Check size={15} /> Dossier dédié</span><span><Check size={15} /> Historique Git</span><span><HardDrives size={15} /> Stockage local</span></div>}
        <label className={`project-trust${trusted ? ' checked' : ''}`}><input type="checkbox" checked={trusted} onChange={(event) => setTrusted(event.target.checked)} disabled={busy} /><span><strong>Faire confiance à ce dossier</strong><span>Kyro pourra lire ce projet et gérer son historique. Aucun script n’est lancé à l’ouverture.</span></span><ShieldCheck size={23} /></label>
        <p className="project-field-hint">{creating ? 'Le dossier sera créé après votre confirmation.' : 'Cet accès reste limité à ce projet et à cette session.'}</p>
      </>}
      {error && <p className="project-flow-error" role="alert">{error}</p>}
      <footer className="project-flow-footer"><button type="button" className="project-secondary" disabled={busy} onClick={cancel}>Annuler</button><button type="submit" className="project-primary" disabled={busy || (plan ? !trusted : !name.trim())}>{busy ? (plan ? 'Préparation en cours…' : 'Préparation…') : plan ? (creating ? 'Créer le projet' : 'Faire confiance et ouvrir') : 'Continuer'}{!busy && <ArrowRight size={17} />}</button></footer>
    </form>
  </dialog>;
}
