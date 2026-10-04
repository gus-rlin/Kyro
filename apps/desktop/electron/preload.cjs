const { contextBridge, ipcRenderer } = require('electron');
contextBridge.exposeInMainWorld('kyroChat', {
  status: () => ipcRenderer.invoke('chat:status'),
  send: (input) => ipcRenderer.invoke('chat:send', input),
  cancel: (jobId) => ipcRenderer.invoke('chat:cancel', jobId),
  watch: (token, jobId, after, onEvent) => {
    const listener = (_event, received, value) => { if (received === token) onEvent(value); };
    ipcRenderer.on('chat:event', listener);
    return ipcRenderer.invoke('chat:watch', token, jobId, after).finally(() => ipcRenderer.removeListener('chat:event', listener));
  },
  unwatch: () => ipcRenderer.invoke('chat:unwatch'),
});
contextBridge.exposeInMainWorld('kyroWorkspace', {
  prepareProject: (name) => ipcRenderer.invoke('workspace:prepareProject', name),
  createProject: (id, trusted) => ipcRenderer.invoke('workspace:createProject', id, trusted),
  trust: (id, trusted) => ipcRenderer.invoke('workspace:trust', id, trusted),
  discard: (id) => ipcRenderer.invoke('workspace:discard', id),
  choose: (purpose) => ipcRenderer.invoke('workspace:choose', purpose),
  select: (id) => ipcRenderer.invoke('workspace:select', id),
  branches: (id) => ipcRenderer.invoke('workspace:branches', id),
  list: (id) => ipcRenderer.invoke('workspace:list', id),
  create: (id, branch) => ipcRenderer.invoke('workspace:create', id, branch),
});
