// Narrow transport shared by the application and the Electron integration fixture.
function registerChatIpc(ipcMain, chat, authorize) {
  const streams = new Map();
  for (const action of ['status', 'send', 'cancel', 'plansProjects', 'plansStatus', 'plansList', 'plansStart', 'plansRead', 'plansExecute', 'plansCancel', 'plansUsage']) {
    ipcMain.handle(`chat:${action}`, async (event, value) => {
      authorize(event);
      try { return { value: await chat[action](value) }; }
      catch (error) { return { error: error.message, code:error.code }; }
    });
  }
  ipcMain.handle('chat:watch', async (event, token, jobId, after) => {
    authorize(event);
    if (typeof token !== 'string' || !/^[0-9a-f-]{36}$/.test(token)) throw new Error('Flux invalide.');
    const sender = event.sender;
    streams.get(sender.id)?.abort();
    const controller = new AbortController();
    streams.set(sender.id, controller);
    const close = () => controller.abort();
    sender.once('destroyed', close);
    sender.once('did-start-navigation', close);
    try {
      await chat.watch(jobId, after, (value) => {
        if (!sender.isDestroyed() && !controller.signal.aborted) sender.send('chat:event', token, value);
      }, controller.signal);
      return { value: true };
    } catch (error) { return { error: controller.signal.aborted ? 'Flux interrompu.' : error.message }; }
    finally {
      sender.removeListener('destroyed', close);
      sender.removeListener('did-start-navigation', close);
      if (streams.get(sender.id) === controller) streams.delete(sender.id);
    }
  });
  ipcMain.handle('chat:unwatch', (event) => { authorize(event); streams.get(event.sender.id)?.abort(); });
}
module.exports = { registerChatIpc };
