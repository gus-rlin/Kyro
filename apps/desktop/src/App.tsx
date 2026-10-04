import { useEffect, useRef, useState, type ChangeEvent, type FormEvent, type MouseEvent } from 'react';
import { ArrowRight, ArrowUp, ArrowUpRight, Database, File, Files, FlowArrow, Folder, FolderOpen, House, Microphone, Monitor, Plus, ShieldCheck, Sparkle, Users, X } from '@phosphor-icons/react';
import { WorkspacePicker } from './WorkspacePicker';
import { TeamPicker } from './TeamPicker';
import { defaultTeam, describeTeam, type TeamConfiguration } from './team-models';
import { ComposerSettings, defaultPreferences, describePreferences, type ComposerPreferences } from './ComposerSettings';
import { ProjectFlow, type ProjectRequest } from './ProjectFlow';
import { workspaceApi } from './workspace-api';
import mascot from './assets/kyro-spark.png';
import '@fontsource-variable/manrope';
import './style.css';
import './project-flow.css';
import './creative-welcome.css';
import './team-picker.css';

type Notice = { title: string; body: string };
type MenuAction = { label: string; action: () => void };
const sections = [
  { label: 'Application', icon: House, color: 'blue', hint: '' },
  { label: 'Pages', icon: Files, color: 'blue', hint: 'Imaginez les pages et les parcours de votre application.' },
  { label: 'Utilisateurs', icon: Users, color: 'peach', hint: 'Pensez aux personnes qui feront vivre votre application.' },
  { label: 'Données', icon: Database, color: 'mint', hint: 'Donnez une place à toutes les informations utiles.' },
  { label: 'Workflows', icon: FlowArrow, color: 'lemon', hint: 'Reliez les étapes pour simplifier le quotidien.' },
  { label: 'Permissions', icon: ShieldCheck, color: 'blue', hint: 'Définissez simplement qui peut faire quoi.' },
];

type FileNode = { name: string; directory?: boolean; children: Map<string, FileNode> };
export type Workspace = { id: string; name: string; kind: 'folder' | 'checkout' | 'worktree'; branch?: string; tree: FileNode; native?: boolean; truncated?: boolean };

function fileTree(files: FileList): FileNode {
  const root: FileNode = { name: '', children: new Map() };
  for (const file of Array.from(files)) {
    let parent = root;
    for (const name of file.webkitRelativePath.split('/').filter(Boolean)) {
      if (!parent.children.has(name)) parent.children.set(name, { name, children: new Map() });
      parent = parent.children.get(name)!;
    }
  }
  return root;
}

async function describeDirectory(files: FileList): Promise<Pick<Workspace, 'name' | 'kind' | 'branch'>> {
  const entries = Array.from(files);
  const name = entries[0]?.webkitRelativePath.split('/')[0] || 'Dossier local';
  const gitFile = entries.find((file) => file.webkitRelativePath === `${name}/.git`);
  if (gitFile) {
    const marker = await gitFile.slice(0, 4096).text();
    if (/^gitdir:\s*.+[/\\]worktrees[/\\][^/\\\s]+\s*$/im.test(marker)) return { name, kind: 'worktree' };
    if (/^gitdir:\s*\S+/im.test(marker)) return { name, kind: 'checkout' };
  }
  const head = entries.find((file) => file.webkitRelativePath === `${name}/.git/HEAD`);
  if (head) {
    const branch = /^ref:\s*refs\/heads\/(.+)\s*$/m.exec(await head.slice(0, 4096).text())?.[1].trim();
    return { name, kind: 'checkout', branch };
  }
  return { name, kind: 'folder' };
}

function TreeBranch({ node, depth = 0 }: { node: FileNode; depth?: number }) {
  const children = [...node.children.values()].sort((a, b) => Number(b.children.size > 0) - Number(a.children.size > 0) || a.name.localeCompare(b.name, 'fr'));
  return <ul className="file-tree" role="group">{children.map((child) => <li key={child.name}>
    {child.directory || child.children.size ? <details open={depth < 2}><summary><Folder size={17} /><span>{child.name}</span></summary><TreeBranch node={child} depth={depth + 1} /></details>
      : <span className="file-leaf"><File size={16} /><span>{child.name}</span></span>}
  </li>)}</ul>;
}

function Menu({ label, actions }: { label: string; actions: MenuAction[] }) {
  function choose(event: MouseEvent<HTMLButtonElement>, action: () => void) {
    event.currentTarget.closest('details')?.removeAttribute('open');
    action();
  }
  return (
    <details className="app-menu" name="application-menu">
      <summary>{label}</summary>
      <div className="menu-popup">
        {actions.map((item) => <button key={item.label} onClick={(event) => choose(event, item.action)}>{item.label}</button>)}
      </div>
    </details>
  );
}

export function App() {
  const [section, setSection] = useState('Application');
  const [selectedWorkspace, setSelectedWorkspace] = useState<Workspace | null>(null);
  const [projectRequest, setProjectRequest] = useState<ProjectRequest | null>(null);
  const [recentWorkspaces, setRecentWorkspaces] = useState<Workspace[]>([]);
  const [explorerVisible, setExplorerVisible] = useState(false);
  const filePicker = useRef<HTMLInputElement>(null);
  const pickerPurpose = useRef<'folder' | 'worktree'>('folder');
  const [draft, setDraft] = useState('');
  const [team, setTeam] = useState<TeamConfiguration>(defaultTeam);
  const [preferences, setPreferences] = useState<ComposerPreferences>(defaultPreferences);
  const [messages, setMessages] = useState<{ text: string; team: TeamConfiguration; preferences: ComposerPreferences }[]>([]);
  const [notice, setNotice] = useState<Notice | null>(null);
  const composer = useRef<HTMLTextAreaElement>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  const conversation = useRef<HTMLDivElement>(null);
  const desktop = navigator.userAgent.includes('Electron/');
  const activeSection = sections.find((item) => item.label === section)!;
  const SectionIcon = activeSection.icon;

  useEffect(() => {
    if (notice && !dialog.current?.open) dialog.current?.showModal();
  }, [notice]);
  useEffect(() => {
    conversation.current?.scrollTo({ top: conversation.current.scrollHeight });
  }, [messages]);
  useEffect(() => {
    const closeMenus = (event: KeyboardEvent) => {
      if (event.key === 'Escape') document.querySelectorAll('.app-menu[open], .profile[open], .checkout-bar details[open]').forEach((item) => item.removeAttribute('open'));
    };
    const closeOnOutsideClick = (event: PointerEvent) => {
      if (event.target instanceof Element && !event.target.closest('.app-menu, .profile')) {
        document.querySelectorAll('.app-menu[open], .profile[open]').forEach((item) => item.removeAttribute('open'));
      }
    };
    document.addEventListener('keydown', closeMenus);
    document.addEventListener('pointerdown', closeOnOutsideClick);
    return () => {
      document.removeEventListener('keydown', closeMenus);
      document.removeEventListener('pointerdown', closeOnOutsideClick);
    };
  }, []);

  function send(event: FormEvent) {
    event.preventDefault();
    if (!draft.trim()) return;
    setMessages((previous) => [...previous, { text: draft.trim(), team: { ...team }, preferences: { ...preferences } }]);
    setDraft('');
    composer.current?.focus();
  }
  function prepareDraft(prompt: string) {
    setDraft(prompt);
    focusComposer();
  }
  function focusComposer() {
    setSection('Application');
    setExplorerVisible(false);
    requestAnimationFrame(() => composer.current?.focus());
  }
  function remember(workspace: Workspace) {
    setSelectedWorkspace(workspace);
    setRecentWorkspaces((previous) => [workspace, ...previous.filter((item) => item.id !== workspace.id)].slice(0, 8));
    setExplorerVisible(true);
  }
  function acceptNative(info: NativeWorkspace) {
    const tree: FileNode = { name: '', children: new Map([[info.name, { name: info.name, directory: true, children: new Map() }]]) };
    for (const entry of info.entries || []) {
      let parent = tree;
      const parts = entry.path.split('/');
      parts.forEach((name, index) => {
        if (!parent.children.has(name)) parent.children.set(name, { name, directory: index < parts.length - 1 || entry.directory, children: new Map() });
        parent = parent.children.get(name)!;
      });
    }
    remember({ ...info, tree, native: true });
  }
  async function openDirectory(purpose: 'folder' | 'worktree') {
    if (workspaceApi) {
      try {
        const result = await workspaceApi.choose(purpose);
        if (result.error) setNotice({ title: 'Dossier inaccessible', body: result.error });
        else if (result.value) setProjectRequest({ kind: 'trust', plan: result.value });
      } catch { setNotice({ title: 'Dossier inaccessible', body: 'Le sélecteur de dossier n’a pas pu être ouvert.' }); }
      return;
    }
    pickerPurpose.current = purpose;
    filePicker.current?.click();
  }
  async function chooseProject(event: ChangeEvent<HTMLInputElement>) {
    const files = event.target.files;
    if (!files?.length) return;
    try {
      const info = await describeDirectory(files);
      if (pickerPurpose.current === 'worktree' && info.kind !== 'worktree') {
        setNotice({ title: 'Worktree non reconnu', body: 'Choisissez un worktree Git existant. Le navigateur doit pouvoir lire son fichier .git ; aucun dossier n’a été remplacé.' });
        return;
      }
      const workspace: Workspace = { ...info, id: crypto.randomUUID(), tree: fileTree(files) };
      remember(workspace);
    } catch {
      setNotice({ title: 'Dossier illisible', body: 'Le navigateur n’a pas pu lire les informations du dossier choisi.' });
    } finally {
      event.target.value = '';
    }
  }
  const menus = [
    { label: 'Fichier', actions: [
      { label: 'Nouveau projet', action: () => setProjectRequest({ kind: 'new' }) },
      { label: 'Ouvrir un dossier', action: () => openDirectory('folder') },
      { label: explorerVisible ? 'Masquer l’explorateur' : 'Afficher l’explorateur', action: () => setExplorerVisible(!explorerVisible) },
    ] },
    { label: 'Modifier', actions: [{ label: 'Effacer le brouillon', action: () => setDraft('') }] },
    { label: 'Aide', actions: [{ label: 'À propos de Kyro', action: () => setNotice({ title: 'Kyro', body: 'Votre espace pour construire et maintenir des applications. Cette interface locale est en cours de préparation.' }) }] },
  ];

  return (
    <div className={`app ide-shell${desktop ? ' desktop' : ''}${section === 'Application' ? ' home' : ''}`}>
      <header className="topbar">
        <a className="wordmark" href="#" aria-label="Kyro accueil" onClick={(event) => { event.preventDefault(); setSection('Application'); setExplorerVisible(false); }}><span className="brand-symbol">k</span><span>kyro</span></a>
        <nav className="menubar" aria-label="Menu de l’application">
          {menus.map((menu) => <Menu key={menu.label} {...menu} />)}
        </nav>
        <div className="title-drag"><span className="project-name">{selectedWorkspace?.name || 'Mon application'}</span></div>
        <button className="publish-button" onClick={() => setNotice({ title: 'Publier l’application', body: 'La publication sera disponible lorsqu’une application et sa destination seront configurées.' })}>Publier <ArrowUpRight size={15} weight="bold" /></button>
      </header>

      <div className="workspace">
        <aside className="activity-rail" aria-label="Navigation principale">
          <nav aria-label="Pages de Kyro">
            {sections.map(({ label, icon: Icon, color }) => <button key={label} aria-label={label} title={label} data-color={color} className={`rail-button${section === label ? ' active' : ''}`} aria-current={section === label ? 'page' : undefined} onClick={() => { setSection(label); setExplorerVisible(false); }}><Icon size={22} weight={section === label ? 'fill' : 'regular'} /><span className="rail-label" aria-hidden="true">{label}</span></button>)}
          </nav>
          <details className="profile" name="application-menu"><summary aria-label="Profil" title="Profil"><span className="profile-avatar">K</span></summary><div className="menu-popup profile-popup"><strong>Profil local</strong><span>Aucun compte connecté</span></div></details>
        </aside>
        <aside className={`project-sidebar${explorerVisible ? ' mobile-open' : ''}`} aria-label="Explorateur de fichiers">
          <div className="explorer-heading"><h2>Explorateur</h2><button className="icon-button" aria-label="Ouvrir un dossier" title="Ouvrir un dossier" onClick={() => openDirectory('folder')}><FolderOpen size={19} /></button></div>
          <input ref={filePicker} className="directory-input" type="file" {...{ webkitdirectory: '' }} multiple onChange={chooseProject} aria-label="Choisir un dossier local" />
          {selectedWorkspace ? <div className="explorer-tree"><TreeBranch node={selectedWorkspace.tree} />{selectedWorkspace.truncated && <p className="workspace-empty">Arborescence limitée à 1 500 entrées et 12 niveaux.</p>}</div> : <div className="explorer-empty"><span className="folder-illustration"><FolderOpen size={35} weight="duotone" /></span><p><strong>À vous de jouer.</strong><span>Aucun dossier ouvert</span></p><button className="explorer-create" onClick={() => setProjectRequest({ kind: 'new' })}><Plus size={16} /> Créer un projet</button><button onClick={() => openDirectory('folder')}>Choisir un dossier <ArrowUpRight size={14} /></button></div>}
          <div className="explorer-bottom"><span className="local-icon"><Folder size={15} /></span> Votre espace.</div>
        </aside>

        <main className="window-surface preview-panel" aria-label="Surface Kyro" tabIndex={-1}>
          <header className="preview-heading"><span><Monitor size={19} /> {section === 'Application' ? 'Aperçu de l’application' : section}</span></header>
          <div className="preview-canvas">
            {section !== 'Application' && <div className="preview-empty" data-color={activeSection.color}><span className="empty-frame"><SectionIcon size={42} weight="duotone" /></span><h2>{section}</h2><p className="section-hint">{activeSection.hint}</p><p>Aucune donnée de projet chargée.</p><button className="text-button" onClick={() => prepareDraft(`Je souhaite définir la section ${section.toLowerCase()} de mon application.`)}>Préparer avec Kyro <ArrowRight size={18} /></button></div>}
          </div>
        </main>

        <section className="chat-panel" aria-label="Chat AI">
          <header className="panel-heading"><h1><span className="assistant-avatar"><img src={mascot} alt="" width="40" height="40" /></span> Kyro</h1></header>
          <div className="conversation" ref={conversation}>
            {messages.length === 0 ? <div className="chat-welcome"><span className="welcome-mark"><Sparkle size={34} weight="fill" /></span><h2>Et si on le<br />construisait ?</h2><p>Une idée en vrac, une envie précise…<br />Écrivez comme vous pensez.</p><button className="chat-suggestion" onClick={() => prepareDraft('Aide-moi à structurer mon idée d’application.')}><span>Structurer mon idée</span><ArrowUpRight size={18} /></button><span className="welcome-footnote">Les belles idées commencent par un échange.</span></div> : <div className="message-list" role="log" aria-label="Conversation"><p className="connection-note">Messages locaux · IA non connectée</p>{messages.map((message, index) => <article className="user-message" key={index}><span>Vous · {describeTeam(message.team)} · démo</span><small className="message-preferences">{describePreferences(message.preferences)}</small><p>{message.text}</p></article>)}</div>}
          </div>
          <div className="composer-area">
            <form className="composer" onSubmit={send}>
              <textarea ref={composer} aria-label="Votre message" placeholder="Qu’aimeriez-vous créer ?" value={draft} onChange={(event) => setDraft(event.target.value)} onKeyDown={(event) => { if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) { event.preventDefault(); event.currentTarget.form?.requestSubmit(); } }} />
              <div className="composer-controls"><TeamPicker team={team} onChange={setTeam} /><div className="composer-actions"><ComposerSettings preferences={preferences} onChange={setPreferences} /><button className="icon-button voice-button" type="button" aria-label="Saisie vocale" title="Saisie vocale" onClick={() => setNotice({ title: 'Saisie vocale', body: 'La saisie vocale n’est pas encore connectée. Vous pouvez écrire votre message dans le chat.' })}><Microphone size={18} /></button><button className="send-button" type="submit" aria-label="Envoyer le message" title="Envoyer le message" disabled={!draft.trim()}><ArrowUp size={18} weight="bold" /></button></div></div>
            </form>
            <WorkspacePicker selected={selectedWorkspace} recent={recentWorkspaces} openDirectory={openDirectory} select={remember} acceptNative={acceptNative} newProject={() => setProjectRequest({ kind: 'new' })} />
          </div>
        </section>

      </div>

      <ProjectFlow request={projectRequest} onClose={() => setProjectRequest(null)} onReady={(workspace) => { acceptNative(workspace); setExplorerVisible(false); }} />
      <footer className="verification-bar" aria-label="État du projet">
        <span>{selectedWorkspace ? 'Dossier local ouvert' : 'Prêt à démarrer'}</span>
        <span>{selectedWorkspace ? 'Décrivez la prochaine étape dans le chat' : 'Créez un projet ou ouvrez un dossier'}</span>
        <button className="status-details" onClick={() => setNotice({ title: 'Docs · Premiers pas', body: 'Créez un projet ou choisissez un dossier pour afficher ses fichiers dans l’explorateur. Sous le chat, les menus Dossier et Worktree permettent de changer d’espace de travail. Le chat accueille vos idées pendant la session ; ses messages ne sont pas conservés après rechargement. L’assistant IA, l’aperçu de votre application et la publication ne sont pas encore connectés.' })}>Docs <ArrowUpRight size={13} /></button>
      </footer>

      <dialog ref={dialog} className="notice-dialog" onClose={() => setNotice(null)} onClick={(event) => { if (event.target === event.currentTarget) dialog.current?.close(); }} aria-labelledby="notice-title">
        <div className="dialog-header"><h2 id="notice-title">{notice?.title}</h2><button className="icon-button" aria-label="Fermer" onClick={() => dialog.current?.close()}><X size={19} /></button></div><p>{notice?.body}</p><button className="dialog-confirm" onClick={() => dialog.current?.close()}>Compris</button>
      </dialog>
    </div>
  );
}
