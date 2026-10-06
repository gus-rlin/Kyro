export type AgentRole = { role: string; model: string };
export type PlanStatus = {
  configured: boolean; code: string; synthetic: boolean; roles: AgentRole[];
  executor_count: number; component_count: number; revision: number;
  tools: { id: string; label: string; available: boolean }[];
  budget: { currency: string; scale: number; spent: number; reserved: number; limit: number };
};
export type AgentRun = {
  id: string; projectId: string; version: number; status: string; request: string;
  planOnly: boolean; diagnostic?: string; objective?: string; artifactId?: string;
  missingCapabilities: string[]; deadline: string;
  tasks: { id: string; objective: string; dependencies: string[]; components: { id: string; version: string }[]; complete: boolean }[];
  calls: { role: string; taskId?: string; failure?: string }[];
};
export type PlanInput = { projectId: string; key: string; revision: number; request: string; contextBytes: number };
export type PlanRef = { projectId: string; runId: string };
export type PlanMutation = PlanRef & { version: number };
type Reply<T> = { value?: T; error?: string; code?: string };
export type PlansBridge = {
  plansProjects(): Promise<Reply<{ id: string; name: string }[]>>;
  plansStatus(projectId: string): Promise<Reply<PlanStatus>>;
  plansList(projectId: string): Promise<Reply<AgentRun[]>>;
  plansStart(input: PlanInput): Promise<Reply<AgentRun>>;
  plansRead(input: PlanRef): Promise<Reply<AgentRun>>;
  plansExecute(input: PlanMutation): Promise<Reply<AgentRun>>;
  plansCancel(input: PlanMutation): Promise<Reply<AgentRun>>;
};
async function command<T>(action: keyof PlansBridge, value?: unknown): Promise<T> {
  const native = window.kyroChat;
  const response: Reply<T> = native
    ? await (native[action] as (value?: unknown) => Promise<Reply<T>>)(value)
    : await fetch(`/__kyro_chat/${action}`, { method: 'POST', headers: { 'content-type': 'application/json', 'x-kyro-local': '1' }, body: JSON.stringify(value ?? null) }).then(response => response.json());
  if (response.error || response.value === undefined) throw Object.assign(new Error(response.error || 'Réponse de plan invalide.'), { code: response.code });
  return response.value;
}
export const plansApi = {
  projects: () => command<{ id: string; name: string }[]>('plansProjects'),
  status: (project: string) => command<PlanStatus>('plansStatus', project),
  list: (project: string) => command<AgentRun[]>('plansList', project),
  start: (input: PlanInput) => command<AgentRun>('plansStart', input),
  read: (input: PlanRef) => command<AgentRun>('plansRead', input),
  execute: (input: PlanMutation) => command<AgentRun>('plansExecute', input),
  cancel: (input: PlanMutation) => command<AgentRun>('plansCancel', input),
};
