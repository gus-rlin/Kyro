import {test,expect,_electron as electron} from '@playwright/test';
import {createServer} from 'vite';
import react from '@vitejs/plugin-react';
import {readFile,writeFile,rm} from 'node:fs/promises';
import {resolve,join,dirname,basename} from 'node:path';
import {tmpdir} from 'node:os';
import {randomUUID} from 'node:crypto';
import {createServer as httpServer, request as httpRequest} from 'node:http';
import {chatDevelopment} from '../scripts/chat-development.mjs';
import service from '../electron/chat.cjs';
import {chatState,control,dropFirstStream} from './fixtures/chat-state.mjs';

async function cleanup(path) {
  const target=resolve(path);
  if(dirname(target)!==resolve(tmpdir())||!basename(target).startsWith('kyro-chat-')) throw new Error('Unsafe test cleanup');
  await rm(target,{recursive:true,force:true});
}
test('missing account confirmation blocks every transport before login or inference',async()=>{
  let calls=0;
  const chat=service.createChatService({stateDir:join(tmpdir(),`kyro-chat-missing-${randomUUID()}`),fetch:()=>{calls++;throw new Error('Network forbidden');}});
  expect((await chat.status()).code).toBe('zero_retention_unconfirmed');
  await expect(chat.send({key:randomUUID(),messages:[{role:'user',content:'Bonjour'}],contextTokens:8192})).rejects.toThrow('zéro conservation');
  expect(calls).toBe(0);
});

test('blocked submit explains activation and preserves the draft without an inference',async({page})=>{
  let sends=0;
  const chat={async status(){return {ready:false,code:'zero_retention_unconfirmed',message:'Nano attend la confirmation de zéro conservation pour votre compte Nebius.'};},async send(){sends++;throw new Error('Must not send');}};
  const server=await createServer({configFile:false,root:process.cwd(),plugins:[react(),chatDevelopment(chat,'http://127.0.0.1:5176'),{name:'test-csp',transformIndexHtml:html=>html.replace("script-src 'self'","script-src 'self' 'unsafe-inline'")}],server:{host:'127.0.0.1',port:5176,strictPort:true,hmr:false}});
  try {
    await server.listen();await page.goto('http://127.0.0.1:5176');
    await expect(page.getByRole('status')).toContainText('zéro conservation');
    const input=page.getByRole('textbox',{name:'Votre message'});
    const send=page.getByRole('button',{name:'Envoyer le message'});
    await expect(send).toBeDisabled();
    await input.fill('Bonjour Nano');await expect(send).toBeEnabled();
    await send.click();await expect(page.getByRole('alert')).toContainText('Votre message reste dans le brouillon');
    await expect(input).toHaveValue('Bonjour Nano');await input.press('Enter');
    await expect(page.getByRole('alert')).toContainText('Revérifier');
    await expect(page.locator('.message-list')).toHaveCount(0);
    expect(sends).toBe(0);
  } finally {await server.close();}
});

test('Vite bridge preserves fragmented input Unicode and refuses foreign origins',async()=>{
  let middleware, received, calls=0;
  const server=httpServer((req,res)=>middleware(req,res));
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const origin=`http://127.0.0.1:${server.address().port}`;
  chatDevelopment({async send(value){calls++;received=value;return {jobId:randomUUID()};}},origin).configureServer({httpServer:server,middlewares:{use(prefix,handler){middleware=(req,res)=>{req.url=req.url.slice(prefix.length);return handler(req,res);};}}});
  async function send(body, from=origin) {
    return new Promise((resolve,reject)=>{
      const req=httpRequest(`${origin}/__kyro_chat/send`,{method:'POST',headers:{origin:from,'content-type':'application/json','x-kyro-local':'1'}},res=>{
        let text='';res.on('data',chunk=>{text+=chunk;});res.on('end',()=>resolve({status:res.statusCode,body:JSON.parse(text)}));
      });
      req.on('error',reject);
      // A multi-byte character crosses real HTTP chunk boundaries.
      const split=body.indexOf(Buffer.from('🍋'))+2;
      req.write(body.subarray(0,split),()=>setTimeout(()=>req.end(body.subarray(split)),20));
    });
  }
  try {
    const input={key:randomUUID(),messages:[{role:'user',content:'Cèdre 🍋 été'}],contextTokens:8192};
    const wire=Buffer.from(JSON.stringify(input));
    expect((await send(wire)).status).toBe(200);
    expect(received).toEqual(input);
    expect((await send(wire,'https://example.invalid')).status).toBe(403);
    expect(calls).toBe(1);
  } finally {await new Promise(resolve=>server.close(resolve));}
});
test('bridge streams, resumes, deduplicates and preserves the funded project after restart',async()=>{
  test.skip(process.env.KYRO_CHAT_INTEGRATION!=='1','Run with the dedicated synthetic chat runtime.');
  const fixture=await chatState();
  let second,reloaded;
  try {
    // Concurrent bridge instances must provision the same funded project.
    second=service.createChatService(fixture.options);
    const [status,other]=await Promise.all([fixture.chat.status(),second.status()]);
    expect(status.ready).toBe(true);expect(other.ready).toBe(true);
    await control('inference',{mode:'chat',delayMs:1500});
    const before=await control('snapshot');
    const input={key:randomUUID(),messages:[{role:'user',content:'Retenons Cèdre 🍋.'}],contextTokens:8192};
    const job=await fixture.chat.send(input);expect(await fixture.chat.send(input)).toEqual(job);
    const abort=new AbortController();let cursor='',partial='',final;
    await expect(fixture.chat.watch(job.jobId,'',e=>{if(e.type==='delta'){partial+=e.data.text;cursor=e.id;abort.abort();}},abort.signal)).rejects.toThrow();
    expect(partial.length).toBeGreaterThan(0);
    await fixture.chat.watch(job.jobId,cursor,e=>{if(e.type==='delta')partial+=e.data.text;else final=e.data;},new AbortController().signal);
    expect(final.status).toBe('succeeded');expect(final.output.text).toBe(partial);expect(final.output.text).toContain('Cèdre 🍋');expect(final.usage).toBeTruthy();
    expect((await control('snapshot')).inferenceRequests-before.inferenceRequests).toBe(1);
    reloaded=service.createChatService(fixture.options);const refreshed=await reloaded.status();
    expect(refreshed.budget.spentUsd).toBeGreaterThan(0);expect(refreshed.budget.limitUsd).toBe(status.budget.limitUsd);
    const client=JSON.parse(await readFile(join(fixture.stateDir,'client.json'),'utf8'));
    expect(client.project_id).toMatch(/^[a-f0-9-]{36}$/);
  } finally {await Promise.all([fixture.chat.close(),second?.close(),reloaded?.close()]);await cleanup(fixture.stateDir);}
});

test('provider cut and split credential keep the hold without leaking the credential',async()=>{
  test.skip(process.env.KYRO_CHAT_INTEGRATION!=='1','Run with the dedicated synthetic chat runtime.');
  const fixture=await chatState();
  try {
    expect((await fixture.chat.status()).ready).toBe(true);
    for(const mode of ['chat_cut','chat_secret']) {
      await control('inference',{mode,delayMs:100});
      const before=await fixture.chat.status(),count=(await control('snapshot')).inferenceRequests;
      const job=await fixture.chat.send({key:randomUUID(),messages:[{role:'user',content:'Test synthétique.'}],contextTokens:8192});
      let text='',complete;
      await fixture.chat.watch(job.jobId,'',event=>{if(event.type==='delta')text+=event.data.text;else complete=event.data;},new AbortController().signal);
      expect(complete.effect_status).toBe('unknown');expect(complete.output).toBeUndefined();
      expect(text).not.toContain('synthetic-e2e-model-key');
      if(mode==='chat_secret') expect(text).toBe('');
      expect((await fixture.chat.status()).budget.reservedUsd).toBeGreaterThan(before.budget.reservedUsd);
      expect((await control('snapshot')).inferenceRequests-count).toBe(1);
    }
  } finally {await fixture.chat.close();await cleanup(fixture.stateDir);}
});

for(const environment of ['browser','electron']) test(`${environment}: visible streaming, conversation memory, stop, errors and session history`,async({page})=>{
  test.skip(process.env.KYRO_CHAT_INTEGRATION!=='1','Run with the dedicated synthetic chat runtime.');
  const fixture=await chatState();let server,app;
  try {
    if(environment==='browser') {
      server=await createServer({configFile:false,root:process.cwd(),plugins:[react(),chatDevelopment(dropFirstStream(fixture.chat),'http://127.0.0.1:5176'),{name:'test-csp',transformIndexHtml:html=>html.replace("script-src 'self'","script-src 'self' 'unsafe-inline'")}],server:{host:'127.0.0.1',port:5176,strictPort:true,hmr:false}});
      await server.listen();await page.goto('http://127.0.0.1:5176');
      const denied=await fetch('http://127.0.0.1:5176/__kyro_chat/send',{method:'POST',headers:{'content-type':'application/json'},body:'{}'});
      expect(denied.status).toBe(403);
    } else {
      app=await electron.launch({args:['tests/fixtures/chat-electron.cjs'],env:{...process.env,KYRO_TEST_CHAT_STATE:fixture.stateDir}});page=await app.firstWindow();
      expect(await page.evaluate(()=>({node:typeof window.require,session:window.kyroChat.session}))).toEqual({node:'undefined',session:undefined});
    }
    await expect(page.getByRole('status')).toContainText('Nano · Nebius');
    await control('inference',{mode:'chat',delayMs:2000});
    const before=await control('snapshot');
    const textbox=page.getByRole('textbox',{name:'Votre message'});
    await textbox.fill('Retenons Cèdre 🍋.');await page.getByRole('button',{name:'Envoyer le message'}).click();
    await expect(page.locator('.assistant-message').first()).toContainText('Cèdre');
    expect((await control('snapshot')).inferenceCompleted).toBe(before.inferenceCompleted);
    await expect(page.getByRole('button',{name:'Envoyer le message'})).toBeVisible();
    await textbox.fill('Quel nom avons-nous retenu ?');await page.getByRole('button',{name:'Envoyer le message'}).click();
    await expect(page.locator('.assistant-message').last()).toContainText('Je me souviens de Cèdre');
    await expect(page.getByRole('button',{name:'Envoyer le message'})).toBeVisible();
    const snapshot=await control('snapshot');expect(snapshot.inferenceRequests-before.inferenceRequests).toBe(2);expect(snapshot.lastChatRoles).toEqual(['system','user','assistant','user']);expect(snapshot.lastChatRememberedCedar).toBe(true);
    expect(await page.locator('.message-list').innerText()).not.toContain('internal-test-reasoning');
    await control('inference',{mode:'chat_silent'});
    await textbox.fill('Interrompons cet échange.');await page.getByRole('button',{name:'Envoyer le message'}).click();
    await expect.poll(async()=>(await control('snapshot')).inferenceRequests).toBe(snapshot.inferenceRequests+1);
    await page.getByRole('button',{name:'Arrêter'}).click();
    await expect(page.locator('.assistant-message').last()).toContainText('Usage final inconnu');
    await expect(page.getByRole('button',{name:'Envoyer le message'})).toBeVisible();
    await control('inference',{mode:'chat_length'});
    await textbox.fill('Une réponse limitée.');await page.getByRole('button',{name:'Envoyer le message'}).click();
    await expect(page.locator('.assistant-message').last()).toContainText('Réponse tronquée');
    expect((await control('snapshot')).lastChatRoles).toEqual(['system','user','assistant','user','assistant','user']);
    await expect(page.getByRole('button',{name:'Envoyer le message'})).toBeVisible();
    await textbox.fill('sk-012345678901234567890123456789');await page.getByRole('button',{name:'Envoyer le message'}).click();
    await expect(page.getByRole('alert')).toContainText('Retirez les secrets');
    await page.reload();await expect(page.getByRole('status')).toContainText('Nano · Nebius');
    await expect(page.getByRole('log')).toHaveCount(0);
    await page.locator('.team-trigger').click();await page.getByLabel('Modèle : Nemotron 3 Nano',{exact:true}).click();
    await expect(page.getByRole('button',{name:/Nemotron 3 Ultra/})).toBeDisabled();
    await page.keyboard.press('Escape');await page.locator('.composer-preferences > summary').click();
    await expect(page.getByRole('button',{name:'Outils indisponibles'})).toBeDisabled();
  } finally {await app?.close();await server?.close();await fixture.chat.close();await cleanup(fixture.stateDir);}
});
