// Deliberate synthetic dependency injection lives only in this test entry point.
const {app,BrowserWindow,ipcMain}=require('electron');
const {join}=require('node:path');
const {pathToFileURL}=require('node:url');
const {createChatService}=require('../../electron/chat.cjs');
const {registerChatIpc}=require('../../electron/chat-ipc.cjs');
app.setPath('userData',process.env.KYRO_TEST_CHAT_STATE);
const chat=createChatService({stateDir:process.env.KYRO_TEST_CHAT_STATE,vaultPath:join(process.env.KYRO_TEST_CHAT_STATE,'synthetic-vault'),origin:process.env.KYRO_CHAT_TEST_API_ORIGIN || 'http://127.0.0.1:58690',oidcOrigin:'http://127.0.0.1:59690',syntheticTest:true});
let drop=true;
const transport={...chat,async watch(job,after,onEvent,signal){
  const interruption=new AbortController();
  return chat.watch(job,after,async value=>{await onEvent(value);if(drop && value.type==='delta'){drop=false;interruption.abort();}},AbortSignal.any([signal,interruption.signal]));
}};
let closing=false;
app.on('before-quit',event=>{if(closing)return;event.preventDefault();closing=true;chat.close().catch(()=>{}).finally(()=>app.quit());});
app.whenReady().then(()=>{
  const win=new BrowserWindow({show:false,webPreferences:{nodeIntegration:false,contextIsolation:true,sandbox:true,preload:join(__dirname,'../../electron/preload.cjs')}});
  const target=join(__dirname,'../../dist/index.html');
  registerChatIpc(ipcMain,transport,(event)=>{
    if(event.sender!==win.webContents || event.senderFrame!==win.webContents.mainFrame || event.senderFrame.url!==pathToFileURL(target).href) throw new Error('Denied test sender');
  });
  win.loadFile(target);
});
app.on('window-all-closed',()=>app.quit());
