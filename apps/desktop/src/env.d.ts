/// <reference types="vite/client" />

type NativeWorkspace = { id: string; name: string; path: string; kind: 'folder' | 'checkout' | 'worktree'; branch?: string; entries?: { path: string; directory: boolean }[]; truncated?: boolean };
type WorkspaceResult<T> = { value?: T; error?: string };
type WorkspacePlan = { id: string; name: string; path: string; purpose: 'project' | 'folder' | 'worktree' };
type WorkspaceBranch = { name: string; remote: boolean; current: boolean };
type WorkspaceApi = {
  branches(id: string): Promise<WorkspaceResult<WorkspaceBranch[]>>;
  prepareProject(name: string): Promise<WorkspaceResult<WorkspacePlan | null>>;
  createProject(id: string, trusted: boolean): Promise<WorkspaceResult<NativeWorkspace>>;
  trust(id: string, trusted: boolean): Promise<WorkspaceResult<NativeWorkspace>>;
  discard(id: string): Promise<WorkspaceResult<null>>;
  choose(purpose: 'folder' | 'worktree'): Promise<WorkspaceResult<WorkspacePlan | null>>;
  select(id: string): Promise<WorkspaceResult<NativeWorkspace>>;
  list(id: string): Promise<WorkspaceResult<NativeWorkspace[]>>;
  create(id: string, branch: string): Promise<WorkspaceResult<NativeWorkspace | null>>;
};
interface Window {
  kyroWorkspace?: WorkspaceApi;
}
