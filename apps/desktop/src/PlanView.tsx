import type { AgentRun } from './plans-api';
const names: Record<string, string> = { planning: 'Planification', planned: 'Plan proposé', executing: 'Exécution des tâches', reviewing: 'Revue et sécurité', integrating: 'Intégration', building: 'Construction et vérification', verified: 'Candidat vérifié', blocked: 'Bloqué', cancelled: 'Annulé' };
export function PlanView({ run, busy, execute, cancel }: { run: AgentRun; busy: boolean; execute: () => void; cancel: () => void }) {
  return <article className="agent-plan" aria-label="Plan des agents">
    <header><strong>{names[run.status] || run.status}</strong><span>Version {run.version}</span></header>
    <p className="agent-request">{run.request}</p>
    {run.objective && <p>{run.objective}</p>}
    {run.diagnostic && <p role="alert">Diagnostic : {run.diagnostic}</p>}
    {run.missingCapabilities.length > 0 && <p>Fonctions manquantes : {run.missingCapabilities.join(', ')}</p>}
    <ol>{run.tasks.map(task => <li key={task.id}><strong>{task.complete ? '✓ ' : ''}{task.objective}</strong><small>{task.components.map(component => `${component.id} ${component.version}`).join(', ')}{task.dependencies.length > 0 && ` · Après ${task.dependencies.join(', ')}`}</small></li>)}</ol>
    {run.calls.length > 0 && <p>{run.calls.length} appel{run.calls.length > 1 ? 's' : ''} enregistré{run.calls.length > 1 ? 's' : ''} · {Array.from(new Set(run.calls.map(call => call.role))).join(', ')}</p>}
    {run.artifactId && <p>Artefact vérifié : {run.artifactId}. La publication reste une étape distincte.</p>}
    <footer>{run.status === 'planned' && run.planOnly && <button type="button" disabled={busy} onClick={execute}>Exécuter le plan</button>}{!['verified', 'blocked', 'cancelled'].includes(run.status) && <button type="button" disabled={busy} onClick={cancel}>Annuler le plan</button>}</footer>
  </article>;
}
