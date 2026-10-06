import { useEffect, useRef, useState } from 'react';
import { plansApi, type AgentRun, type PlanInput, type PlanStatus } from './plans-api';
export function usePlans(enabled: boolean) {
  const [projects, setProjects] = useState<{ id: string; name: string }[]>([]);
  const [projectId, setProjectId] = useState('');
  const [status, setStatus] = useState<PlanStatus | null>(null);
  const [runs, setRuns] = useState<AgentRun[]>([]);
  const [run, setRun] = useState<AgentRun | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [uncertain, setUncertain] = useState(false);
  const pending = useRef<PlanInput | null>(null);
  const generation = useRef(0);
  const working = useRef(false);
  const current = useRef({ enabled, projectId });
  current.current = { enabled, projectId };
  const message = (failure: unknown) => failure instanceof Error ? failure.message : 'Le service des agents est indisponible.';
  useEffect(() => {
    if (!enabled) return;
    let alive = true;
    void plansApi.projects().then(value => { if (alive) setProjects(value); }).catch(failure => { if (alive) setError(message(failure)); });
    return () => { alive = false; };
  }, [enabled]);
  async function refresh() {
    if (!projectId || !enabled) return;
    const epoch = generation.current;
    try {
      const value = await plansApi.status(projectId);
      const history = await plansApi.list(projectId);
      if (epoch !== generation.current || !current.current.enabled) return;
      setStatus(value); setRuns(history); setRun(old => history.find(item => item.id === old?.id) || history[0] || null); setError('');
    } catch (failure) { if (epoch === generation.current && current.current.enabled) setError(message(failure)); }
  }
  useEffect(() => {
    generation.current++; setStatus(null); setRun(null); setRuns([]); setError('');
    if (enabled && projectId) void refresh();
  }, [enabled, projectId]);
  useEffect(() => {
    if (!enabled || !projectId || !run || ['verified', 'blocked', 'cancelled', 'planned'].includes(run.status)) return;
    let alive = true, reading = false;
    const timer = setInterval(() => {
      if (working.current || reading) return;
      reading = true;
      void plansApi.read({ projectId, runId: run.id }).then(value => { if (alive) setRun(old => old?.id === value.id && value.version >= old.version ? value : old); })
        .catch(failure => { if (alive) setError(message(failure)); }).finally(() => { reading = false; });
    }, 1500);
    return () => { alive = false; clearInterval(timer); };
  }, [enabled, projectId, run?.id, run?.status]);
  async function create(input: PlanInput) {
    if (working.current) return false;
    working.current = true; setBusy(true); setError('');
    try {
      const value = await plansApi.start(input);
      pending.current = null; setUncertain(false);
      if (current.current.projectId === input.projectId && current.current.enabled) { setRun(value); setRuns(old => [value, ...old.filter(item => item.id !== value.id)]); }
      return true;
    } catch (failure) {
      if ((failure as { code?: string }).code === 'transport_unknown' || !(failure as { code?: string }).code) {
        pending.current = input; setUncertain(true); setError('Envoi incertain. Reprendre conserve la même demande, sa révision et sa clé.');
      } else { pending.current = null; setUncertain(false); setError(message(failure)); }
      return false;
    } finally { working.current = false; setBusy(false); }
  }
  async function start(text: string, contextBytes: number) {
    if (pending.current || !status?.configured || !projectId) { setError('Choisissez un projet avec une équipe et un catalogue configurés, puis revérifiez.'); return false; }
    return create({ projectId, revision: status.revision, key: crypto.randomUUID(), request: text.trim(), contextBytes });
  }
  async function mutate(action: 'execute' | 'cancel') {
    if (!run || working.current) return;
    working.current = true; setBusy(true); setError('');
    try { setRun(await plansApi[action]({ projectId, runId: run.id, version: run.version })); }
    catch (failure) { setError(`${message(failure)} Revérifiez l’état avant une nouvelle action.`); }
    finally { working.current = false; setBusy(false); }
  }
  return { projects, projectId, selectProject: setProjectId, status, runs, run, selectRun: setRun, error, busy, uncertain, start, refresh,
    retry: async () => { const input = pending.current; return input && await create(input) ? input.request : null; }, execute: () => mutate('execute'), cancel: () => mutate('cancel') };
}
