import { ComposerChoice } from './ComposerChoice';
import { useTooltip } from './useTooltip';
import { useEffect, useRef } from 'react';
import { CaretDown, Minus, Plus, TreeStructure, Users } from '@phosphor-icons/react';
import { describeTeam, getTeamModel, modelCostHint, modelCostNote, teamModels, type TeamConfiguration } from './team-models';
import type { PlanStatus } from './plans-api';

export function TeamPicker({ team, onChange, chatOnly=false, mode, onMode, agents }: { team: TeamConfiguration; onChange: (team: TeamConfiguration) => void; chatOnly?:boolean; mode?: 'conversation' | 'agents'; onMode?: (mode: 'conversation' | 'agents') => void; agents?: PlanStatus | null }) {
  const { triggerProps, tooltip } = useTooltip('Modèles');
  const picker = useRef<HTMLDetailsElement>(null);
  useEffect(() => {
    const closeOutside = (event: PointerEvent) => {
      if (!picker.current?.contains(event.target as Node)) picker.current?.removeAttribute('open');
    };
    document.addEventListener('pointerdown', closeOutside);
    return () => document.removeEventListener('pointerdown', closeOutside);
  }, []);
  const orchestrator = getTeamModel(team.orchestrator);
  const worker = getTeamModel(team.worker);
  const modelChoices = teamModels.map((model) => ({ value: model.id, label: model.name, description: chatOnly && model.id!=='nano'?'Indisponible dans cette version':modelCostNote(model.id), hint: modelCostHint(model.id), disabled:chatOnly && model.id!=='nano' }));

  return <details ref={picker} className="team-picker" name="application-menu" onToggle={(event) => { if (!event.currentTarget.open) event.currentTarget.querySelectorAll('details[open]').forEach((item) => item.removeAttribute('open')); }} onKeyDown={(event) => {
    if (event.key === 'Escape') {
      picker.current?.removeAttribute('open');
      picker.current?.querySelector('summary')?.focus();
      event.stopPropagation();
    }
  }}>
    <summary {...triggerProps} className="team-trigger" aria-label={mode === 'agents' ? 'Équipe P3 : orchestrateur et sous-agents' : chatOnly?'Modèles : Nemotron 3 Nano':`Configurer l’équipe : ${describeTeam(team)} (démo)`}>
      <TreeStructure size={17} /><span className="team-trigger-label"><strong>{mode === 'agents' ? 'Orchestrateur' : orchestrator.short}</strong>{mode === 'agents' ? <><span className="team-trigger-dot">·</span>{agents?.executor_count || 4} agents</> : !chatOnly && <><span className="team-trigger-dot">·</span>{team.workers} {worker.short}</>}</span><CaretDown size={12} />
    </summary>
    {tooltip}
    <section className="team-panel" aria-label="Composition de l’équipe">
      <header className="team-panel-header"><div><h2>Votre équipe</h2><p>{mode === 'agents' ? 'Planifier, composer et vérifier votre application.' : chatOnly?'Conversation avec Nano via Nebius.':'Modèles et nombre de sous-agents.'}</p></div>{!chatOnly && <span className="team-demo">Démo</span>}</header>
      {onMode && <div className="agent-modes" role="group" aria-label="Mode de travail"><button type="button" aria-pressed={mode === 'conversation'} onClick={() => onMode('conversation')}>Conversation</button><button type="button" aria-pressed={mode === 'agents'} onClick={() => onMode('agents')}>Agents</button></div>}
      {mode === 'agents' ? <div className="agent-team-panel">
        {agents?.roles.length ? agents.roles.map(item => <div className="agent-role" key={item.role}><strong>{({orchestrator:'Orchestrateur',pixel:'Pixel',moka:'Moka',kiwi:'Kiwi',biscotte:'Biscotte',review:'Revue',security:'Sécurité'} as Record<string,string>)[item.role] || item.role}</strong><p>{item.model}</p></div>) : <p>Orchestrateur · Pixel · Moka · Kiwi · Biscotte · Revue · Sécurité. Choisissez un projet pour consulter son équipe configurée.</p>}
        <p>Jusqu’à {agents?.executor_count || 4} tâches en parallèle. Les modèles sont définis dans le runtime.</p>
        {agents?.synthetic && <p>Fournisseur synthétique de développement.</p>}
        <strong>Outils disponibles</strong>
        {agents?.tools.length ? <ul>{agents.tools.map(tool => <li key={tool.id}>{tool.label} · {tool.available ? 'configuré' : 'indisponible'}</li>)}</ul> : <p>Équipe P3 et catalogue à configurer dans le runtime. Le chat Nano reste disponible en mode Conversation.</p>}
      </div> : <>
      <div className="team-role">
        <span className="team-role-icon"><TreeStructure size={20} /></span>
        <div className="team-role-content"><strong>{chatOnly?'Conversation':'Orchestrateur'}</strong><p>{chatOnly?'Un seul appel, sans outils.':'Planifie, délègue et arbitre.'}</p>
          <ComposerChoice label={chatOnly?'Modèle':'Orchestrateur'} selected={team.orchestrator} choices={modelChoices} onSelect={(orchestrator) => onChange({ ...team, orchestrator })} />
        </div>
      </div>
      {chatOnly?<p className="team-role-content">Pour déléguer aux sous-agents, choisissez le mode Agents.</p>:<><div className="team-role">
        <span className="team-role-icon workers"><Users size={20} /></span>
        <div className="team-role-content"><strong>Sous-agents</strong><p>Composent et vérifient les blocs.</p>
          <ComposerChoice label="Sous-agents" selected={team.worker} choices={modelChoices} onSelect={(worker) => onChange({ ...team, worker })} />
        </div>
      </div>
      <div className="team-capacity"><div><strong>En parallèle</strong><small>Jusqu’à {team.workers} sous-agent{team.workers > 1 ? 's' : ''}</small></div><div className="team-stepper" role="group" aria-label="Nombre de sous-agents">
        <button type="button" aria-label="Moins de sous-agents" disabled={team.workers <= 1} onClick={() => onChange({ ...team, workers: Math.max(1, team.workers - 1) })}><Minus size={14} /></button>
        <output aria-live="polite">{team.workers}</output>
        <button type="button" aria-label="Plus de sous-agents" disabled={team.workers >= 32} onClick={() => onChange({ ...team, workers: Math.min(32, team.workers + 1) })}><Plus size={14} /></button>
      </div></div></>}
      </>}
    </section>
  </details>;
}
