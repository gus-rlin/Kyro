import { ComposerChoice } from './ComposerChoice';
import { useEffect, useRef, useState } from 'react';
import { ArrowRight, Folder, Globe, SlidersHorizontal, Terminal, Warning, X } from '@phosphor-icons/react';

const accessChoices = [
  { value: 'ask', label: 'Demander l’approbation', description: 'Valider chaque modification et action externe.' },
  { value: 'auto', label: 'Approbation automatique', description: 'Réserver la validation aux actions sensibles.' },
  { value: 'full', label: 'Accès complet', description: 'Autoriser les actions sans demande d’approbation.', danger: true },
] as const;
const reasoningChoices = [
  { value: 'low', label: 'Faible', description: 'Pour les demandes simples et directes.' },
  { value: 'medium', label: 'Standard', description: 'Un équilibre pour les tâches courantes.' },
  { value: 'high', label: 'Élevé', description: 'Pour les problèmes qui demandent plus d’analyse.' },
  { value: 'max', label: 'Maximum', description: 'Pour les tâches les plus complexes.' },
] as const;
const contextChoices = [
  { value: 4096, label: '4k', description: '4 096 tokens · échanges courts.' },
  { value: 8192, label: '8k', description: '8 192 tokens · davantage d’historique.' },
  { value: 16384, label: '16k', description: '16 384 tokens · contexte étendu.' },
] as const;

export type ComposerPreferences = {
  access: typeof accessChoices[number]['value'];
  reasoning: typeof reasoningChoices[number]['value'];
  contextTokens: typeof contextChoices[number]['value'];
};
export const defaultPreferences: ComposerPreferences = { access: 'ask', reasoning: 'medium', contextTokens: 16384 };
export function describePreferences(preferences: ComposerPreferences) {
  return `${accessChoices.find((item) => item.value === preferences.access)!.label} · ${reasoningChoices.find((item) => item.value === preferences.reasoning)!.label} · ${contextChoices.find((item) => item.value === preferences.contextTokens)!.label}`;
}

export function ComposerSettings({ preferences, onChange }: { preferences: ComposerPreferences; onChange: (value: ComposerPreferences) => void }) {
  const container = useRef<HTMLDetailsElement>(null);
  const accessDialog = useRef<HTMLDialogElement>(null);
  const [confirmFullAccess, setConfirmFullAccess] = useState(false);
  useEffect(() => {
    const closeOutside = (event: PointerEvent) => {
      if (!container.current?.contains(event.target as Node)) container.current?.removeAttribute('open');
    };
    document.addEventListener('pointerdown', closeOutside);
    return () => document.removeEventListener('pointerdown', closeOutside);
  }, []);
  useEffect(() => {
    if (confirmFullAccess && !accessDialog.current?.open) {
      accessDialog.current?.showModal();
      accessDialog.current?.querySelector<HTMLButtonElement>('.project-secondary')?.focus();
    }
  }, [confirmFullAccess]);

  function chooseAccess(access: ComposerPreferences['access']) {
    if (access === 'full' && preferences.access !== 'full') {
      setConfirmFullAccess(true);
      return;
    }
    onChange({ ...preferences, access });
  }

  return <details ref={container} className="composer-preferences" name="application-menu" onToggle={(event) => { if (!event.currentTarget.open) event.currentTarget.querySelectorAll('details[open]').forEach((item) => item.removeAttribute('open')); }} onKeyDown={(event) => {
    if (event.key === 'Escape' && !accessDialog.current?.open) {
      container.current?.removeAttribute('open');
      container.current?.querySelector('summary')?.focus();
      event.stopPropagation();
    }
  }}>
    <summary aria-label="Réglages du message" title={describePreferences(preferences)}><SlidersHorizontal size={18} /></summary>
    <section className="composer-preferences-panel" aria-label="Réglages du message">
      <header className="team-panel-header"><div><h2>Réglages</h2><p>Les préférences de votre message.</p></div><span className="team-demo">Démo</span></header>
      <div className="preference-field"><strong>Accès</strong><ComposerChoice label="Accès" selected={preferences.access} choices={accessChoices} onSelect={chooseAccess} /></div>
      <fieldset className="preference-field"><legend>Raisonnement de l’orchestrateur</legend><div className="preference-options">{reasoningChoices.map((item) => <button type="button" key={item.value} aria-pressed={preferences.reasoning === item.value} title={item.description} onClick={() => onChange({ ...preferences, reasoning: item.value })}>{item.label}</button>)}</div></fieldset>
      <fieldset className="preference-field"><legend>Contexte par agent</legend><div className="preference-options context-options">{contextChoices.map((item) => <button type="button" key={item.value} aria-pressed={preferences.contextTokens === item.value} title={item.description} onClick={() => onChange({ ...preferences, contextTokens: item.value })}>{item.label}</button>)}</div></fieldset>
    </section>
    <dialog ref={accessDialog} className="project-dialog access-confirm-dialog" aria-labelledby="access-confirm-title" aria-describedby="access-confirm-description" onClose={() => {
      setConfirmFullAccess(false);
      container.current?.querySelector<HTMLElement>('.composer-choice > summary')?.focus();
    }}>
      <header className="project-flow-top"><span className="project-flow-brand"><span className="mini-brand">k</span> Les accès de votre équipe</span><button type="button" className="icon-button" aria-label="Fermer la confirmation" onClick={() => accessDialog.current?.close()}><X size={20} /></button></header>
      <div className="access-confirm-body">
        <div className="project-flow-copy"><h2 id="access-confirm-title">Activer l’accès complet ?</h2><p id="access-confirm-description">Votre équipe pourra agir sans vous demander confirmation à chaque étape.</p></div>
        <div className="access-capabilities">
          <div><span className="project-included-icon"><Folder size={22} aria-hidden="true" /></span><span><strong>Fichiers du projet</strong><small>Lire, créer et modifier les fichiers du dossier choisi.</small></span></div>
          <div><span className="project-included-icon"><Terminal size={22} aria-hidden="true" /></span><span><strong>Commandes et outils</strong><small>Exécuter des commandes et modifier votre projet.</small></span></div>
          <div><span className="project-included-icon"><Globe size={22} aria-hidden="true" /></span><span><strong>Internet et applications</strong><small>Consulter le web, envoyer des données et utiliser les plugins activés.</small></span></div>
        </div>
        <div className="access-confirm-risk"><Warning size={20} aria-hidden="true" /><p>Une action peut modifier ou supprimer des données, ou les partager avec un service externe.</p></div>
        <p className="project-field-hint">Vous pouvez limiter ces accès à tout moment dans les réglages.</p>
        <footer className="project-flow-footer">
          <button type="button" className="project-secondary" onClick={() => accessDialog.current?.close()}>Annuler</button>
          <button type="button" className="project-primary" onClick={() => { onChange({ ...preferences, access: 'full' }); accessDialog.current?.close(); }}>Activer l’accès complet <ArrowRight size={17} aria-hidden="true" /></button>
        </footer>
      </div>
    </dialog>
  </details>;
}
