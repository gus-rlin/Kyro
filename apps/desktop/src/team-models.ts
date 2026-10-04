// UI simulation only. These keys are not provider endpoint IDs.
// Public Nebius endpoint rates, USD per million tokens, verified 2026-10-04.
// Source: https://nebius.com/services/token-factory/models/nvidia-nemotron-models-inference
export const teamModels = [
  { id: 'ultra', name: 'Nemotron 3 Ultra', short: 'Ultra', input: 1, output: 3 },
  { id: 'super', name: 'Nemotron 3 Super', short: 'Super', input: .3, output: .9 },
  { id: 'lightning', name: 'Nemotron 3.5 Lightning', short: 'Lightning', input: .06, output: .24 },
  { id: 'nano', name: 'Nemotron 3 Nano', short: 'Nano', input: .06, output: .24 },
] as const;
export type TeamModelId = typeof teamModels[number]['id'];
export type TeamConfiguration = { orchestrator: TeamModelId; worker: TeamModelId; workers: number };
export const defaultTeam: TeamConfiguration = { orchestrator: 'ultra', worker: 'lightning', workers: 4 };
export const getTeamModel = (id: TeamModelId) => teamModels.find((model) => model.id === id)!;
export function describeTeam(team: TeamConfiguration) {
  return `${getTeamModel(team.orchestrator).short} · ${team.workers} ${getTeamModel(team.worker).short}`;
}
// Identical workload: one 10k input / 2k output call per participating agent.
// This is a comparison scenario, never a prediction of a project's final bill.
export function illustrativeCallCost(id: TeamModelId) {
  const model = getTeamModel(id);
  return (10_000 * model.input + 2_000 * model.output) / 1_000_000;
}
export const relativeCost = (id: TeamModelId) => illustrativeCallCost(id) / illustrativeCallCost('lightning');
export const formatRatio = (value: number) => new Intl.NumberFormat('fr-FR', { maximumFractionDigits: 1 }).format(value);

export function modelCostNote(id: TeamModelId) {
  if (id === 'lightning') return '(tarif de référence)';
  if (id === 'nano') return '(mêmes tarifs que Lightning)';
  return `(≈ ${formatRatio(relativeCost(id))}× le coût de Lightning)`;
}

export function modelCostHint(id: TeamModelId) {
  const model = getTeamModel(id);
  const price = (value: number) => new Intl.NumberFormat('fr-FR', { minimumFractionDigits: 2 }).format(value);
  return `Nebius · ${price(model.input)} $ en entrée / ${price(model.output)} $ en sortie par million de tokens. Tarifs publics vérifiés le 04/10/2026. Comparaison : 10 000 tokens en entrée et 2 000 en sortie, hors cache. Le coût total dépend de l’usage.`;
}
