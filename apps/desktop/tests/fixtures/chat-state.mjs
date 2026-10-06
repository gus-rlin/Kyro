import {mkdtemp,readFile,writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import service from '../../electron/chat.cjs';
export const controlOrigin='http://127.0.0.1:59691';
export async function control(path,body) {
  const endpoint=path==='inference'?'scenario':path;
  const payload=path==='inference'?{inference:body}:body;
  const response=await fetch(`${controlOrigin}/__e2e/${endpoint}`,{method:body?'POST':'GET',headers:{authorization:'Bearer synthetic-chat-control-token-for-local-tests',...(body?{'content-type':'application/json'}:{})},body:payload&&JSON.stringify(payload)});
  if(!response.ok) throw new Error(`Synthetic control ${response.status}`);
  return response.json();
}
export async function chatState() {
  const stateDir=await mkdtemp(join(tmpdir(),'kyro-chat-fixture-'));
  const vaultPath=join(stateDir,'synthetic-vault');
  const vault=Buffer.from('synthetic encrypted vault placeholder - not a Nebius credential');
  const registry=JSON.parse(await readFile(new URL('../../../../tests/fixtures/models.chat.synthetic.json',import.meta.url),'utf8'));
  const proof={checked_at:new Date().toISOString(),model:registry.destinations[0].models[0].id,endpoint:'https://api.tokenfactory.nebius.com/v1/',source:'https://docs.nebius.com/legal/token-factory',account_evidence:'SYNTHETIC FIXTURE ONLY. No claim about any Nebius account.',zero_retention_confirmed:true,max_completion_tokens_includes_reasoning:true,vault_sha256:createHash('sha256').update(vault).digest('hex')};
  for(const [name,data] of Object.entries({'models.json':registry,'qualification.json':proof,'budget.json':{ceiling_eur:1,currency:'USD',unit_scale:1e9,limit_units:600000000,eur_usd:1,fx_date:'2026-10-04',margin_fraction:.4}})) await writeFile(join(stateDir,name),JSON.stringify(data));
  await writeFile(vaultPath,vault);
  const options={stateDir,vaultPath,origin:process.env.KYRO_CHAT_TEST_API_ORIGIN || 'http://127.0.0.1:58690',oidcOrigin:'http://127.0.0.1:59690',syntheticTest:true};
  return {stateDir,options,chat:service.createChatService(options)};
}
export function dropFirstStream(chat) {
  let drop=true;
  return {...chat,async watch(job,after,onEvent,signal) {
    const interruption=new AbortController();
    return chat.watch(job,after,async event=>{
      await onEvent(event);
      if(drop && event.type==='delta') {drop=false;interruption.abort();}
    },AbortSignal.any([signal,interruption.signal]));
  }};
}
