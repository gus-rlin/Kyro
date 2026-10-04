async function local<T>(action: string, args: unknown[]): Promise<WorkspaceResult<T>> {
  try {
    const response = await fetch(`/__kyro_native/${action}`, { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Kyro-Local': '1' }, body: JSON.stringify(args) });
    if (!response.ok) throw new Error();
    return await response.json();
  } catch { return { error: 'La connexion locale est interrompue. Relancez Kyro puis réessayez.' }; }
}
export const workspaceApi: WorkspaceApi | undefined = window.kyroWorkspace?.branches ? window.kyroWorkspace : import.meta.env.DEV ? {
  choose: (purpose) => local('choose', [purpose]),
  select: (id) => local('select', [id]),
  branches: (id) => local('branches', [id]),
  list: (id) => local('list', [id]),
  create: (id, branch) => local('create', [id, branch]),
  prepareProject: (name) => local('prepareProject', [name]),
  createProject: (id, trusted) => local('createProject', [id, trusted]),
  trust: (id, trusted) => local('trust', [id, trusted]),
  discard: (id) => local('discard', [id]),
} : undefined;
