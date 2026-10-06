const { readFile, writeFile, rename, mkdir, unlink, rmdir } = require('node:fs/promises');
const { join } = require('node:path');
const { homedir } = require('node:os');
const { createHash } = require('node:crypto');
const { createPlansService } = require('./plans.cjs');

const MODEL = 'nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B';
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const blocked = { ready: false, code: 'zero_retention_unconfirmed', message: 'Nano attend la confirmation de zéro conservation pour votre compte Nebius.' };
const failures = {
  budget_exceeded: 'Le plafond initial de 1 € est atteint ou réservé. Aucun nouvel appel ne sera envoyé.',
  forbidden: 'Le message ou ses accès ont été refusés. Retirez les secrets éventuels avant de réessayer.',
  resource_limit: 'Le message dépasse les limites du contexte choisi.',
  gateway_unavailable: 'Le worker Nano est indisponible. Démarrez le runtime du chat puis réessayez.',
  unauthenticated: 'La session locale a expiré. Rechargez l’interface pour vous reconnecter.',
};

// This bridge handles only the local synthetic identity used by this development runtime.
// It has no provider key, arbitrary URL/HTTP API, or production-login capability.
function createChatService(options = {}) {
  const stateDir = options.stateDir || join(homedir(), '.kyro', 'nebius-chat');
  const origin = options.origin || 'http://127.0.0.1:58190';
  const oidcOrigin = options.oidcOrigin || 'http://127.0.0.1:59190';
  if (!/^http:\/\/127\.0\.0\.1:\d+$/.test(origin) || !/^http:\/\/127\.0\.0\.1:\d+$/.test(oidcOrigin)) throw new Error('Invalid local chat origin');
  const fetcher = options.fetch || fetch;
  let session, project, initPromise, renewalPromise;
  const jobs = new Set();
  const readJson = async (name) => JSON.parse(await readFile(join(stateDir, name), 'utf8'));
  const save = async (value) => { await mkdir(stateDir, { recursive: true }); await writeFile(join(stateDir,'client.json.tmp'), JSON.stringify(value)+'\n', { mode:0o600 }); await rename(join(stateDir,'client.json.tmp'),join(stateDir,'client.json')); };
  async function provision(operation) {
    // Electron and Vite can run together. They must share ONE funded project.
    const lock=join(stateDir,'provision.lock'), owner=join(lock,'pid');
    await mkdir(stateDir,{recursive:true});
    for(let attempt=0;attempt<100;attempt++) {
      try { await mkdir(lock); await writeFile(owner,String(process.pid));
        try { return await operation(); } finally { await unlink(owner); await rmdir(lock); }
      } catch(error) {
        if(error.code!=='EEXIST') throw error;
        try {
          const pid=Number(await readFile(owner,'utf8'));
          if(Number.isSafeInteger(pid) && pid>0) {
            try { process.kill(pid,0); }
            catch(dead) { if(dead.code==='ESRCH') { await unlink(owner); await rmdir(lock); } }
          }
        } catch { /* Another process may be creating/releasing its lock. */ }
        await new Promise(resolve=>setTimeout(resolve,100));
      }
    }
    throw new Error('Chat provisioning is busy');
  }
  function cookie(headers, name) { return headers.getSetCookie().map(v=>v.split(';')[0]).find(v=>v.startsWith(`${name}=`)); }
  async function request(path, { method='GET', body, headers={}, auth=true, signal, retryAuth=true }={}) {
    const url = new URL(path, origin);
    if (url.origin !== origin && !(auth===false && url.origin===oidcOrigin)) throw new Error('Forbidden local endpoint');
    if(auth && retryAuth && renewalPromise) await renewalPromise;
    const sentSession=session;
    if (auth && session) { headers.cookie=session.cookie; if(method!=='GET') { headers.origin=origin; headers['x-csrf-token']=session.csrf; } }
    if(body) headers['content-type']='application/json';
    const response=await fetcher(url,{method,headers,body:body&&JSON.stringify(body),redirect:'manual',signal:signal||AbortSignal.timeout(15000)});
    if(auth && sentSession && response.status===401 && (await response.clone().json().catch(()=>null))?.error?.code==='unauthenticated') {
      if(!retryAuth) { if(session===sentSession) {session=null;initPromise=null;} return response; }
      if(!await qualification()) throw Object.assign(new Error(blocked.message),{code:blocked.code});
      // Retry only a confirmed authentication refusal, sharing renewal across concurrent requests.
      if(!renewalPromise && session===sentSession) renewalPromise=login().catch(error=>{session=null;initPromise=null;throw error;}).finally(()=>{renewalPromise=null;});
      await renewalPromise;
      return request(path,{method,body,headers,auth,signal,retryAuth:false});
    }
    return response;
  }
  async function jsonRequest(path, opts, maxBytes = 131072) {
    const response=await request(path,opts);
    const chunks=[]; let bytes=0;
    for await (const chunk of response.body || []) {
      bytes+=chunk.length;
      if(bytes>maxBytes) throw new Error('Response too large');
      chunks.push(chunk);
    }
    const text=Buffer.concat(chunks).toString('utf8');
    const data=text?JSON.parse(text):null;
    if(!response.ok) { const error=new Error(failures[data?.error?.code] || 'Le service local a refusé la demande. Vérifiez le runtime puis réessayez.'); error.code=data?.error?.code; throw error; }
    return { response,data };
  }
  async function qualification() {
    try {
      const [proof,registry,vault]=await Promise.all([readJson('qualification.json'),readJson('models.json'),readFile(options.vaultPath||join(process.env.LOCALAPPDATA||join(homedir(),'AppData','Local'),'Kyro','secrets','nebius.dpapi'))]);
      const age=Date.now()-Date.parse(proof.checked_at); const source=new URL(proof.source);
      const dest=registry.destinations.find(d=>d.id==='nebius-chat');
      // Only the test entry point can inject a synthetic registry; production fixes the provider contract.
      const synthetic=options.syntheticTest===true;
      if(!dest || dest.provider!==(synthetic?'synthetic':'nebius') || dest.kind!==(synthetic?'synthetic':'cloud') || dest.models[0]?.output_mode!=='text_chat' || (!synthetic && dest.base_url!==proof.endpoint)) return false;
      const standard=!synthetic && proof.retention_mode==='provider_standard' && proof.standard_retention_accepted===true && proof.zero_retention_confirmed===false && typeof proof.consent_reference==='string' && !!proof.consent_reference.trim() && dest.retention_seconds===null && dest.nebius?.provider_standard_retention_accepted===true;
      const zero=dest.retention_seconds===0 && proof.zero_retention_confirmed===true && typeof proof.account_evidence==='string' && !!proof.account_evidence.trim();
      if (!dest.qualified || (!standard && !zero) || dest.models[0]?.id!==MODEL || proof.model!==MODEL || proof.endpoint!=='https://api.tokenfactory.nebius.com/v1/' || proof.max_completion_tokens_includes_reasoning!==true || !Number.isFinite(age) || age<0 || age>30*86400000 || source.protocol!=='https:' || !['nebius.com','docs.nebius.com','docs.tokenfactory.nebius.com'].includes(source.hostname) || source.username || source.password || proof.vault_sha256!==createHash('sha256').update(vault).digest('hex')) return false;
      return true;
    } catch { return false; }
  }
  async function login() {
    const initial=await request('/v1/auth/login',{auth:false});
    if(initial.status!==303) throw new Error('Local login unavailable');
    const authorization=new URL(initial.headers.get('location'));
    if(authorization.origin!==oidcOrigin) throw new Error('Unexpected identity provider');
    const authorized=await request(authorization.href,{auth:false});
    const data=await authorized.json();
    if(authorized.status!==200 || typeof data.code!=='string') throw new Error('Local login failed');
    const completed=await request(`/v1/auth/callback?code=${encodeURIComponent(data.code)}&state=${encodeURIComponent(authorization.searchParams.get('state'))}`,{auth:false,headers:{cookie:cookie(initial.headers,'kyro_oidc_binding')}});
    if(completed.status!==303) throw new Error('Local login failed');
    const cookies=[cookie(completed.headers,'kyro_session'),cookie(completed.headers,'kyro_csrf')];
    if(cookies.some(v=>!v)) throw new Error('Local login failed');
    session={cookie:cookies.join('; ')};
    session.csrf=(await jsonRequest('/v1/auth/session',{retryAuth:false})).data.csrf_token;
  }
  async function initialize() {
    await login(); const budget=await readJson('budget.json');
    const standard=(await readJson('qualification.json')).retention_mode==='provider_standard';
    if(budget.ceiling_eur!==1 || budget.currency!=='USD' || budget.unit_scale!==1e9 || !Number.isSafeInteger(budget.limit_units) || budget.limit_units<=0 || budget.limit_units>Math.floor(budget.eur_usd*.6*1e9)) throw new Error('Invalid chat budget');
    let client; try { client=await readJson('client.json'); } catch(error) { if(error.code!=='ENOENT') throw error; }
    if(client?.project_id) {
      if(!UUID.test(client.project_id)) throw new Error('Invalid chat project');
      project=client.project_id;
      const current=(await jsonRequest(`/v1/projects/${project}/budget`)).data;
      if(current.currency!=='USD' || current.unit_scale!==1e9 || current.limit_units>budget.limit_units) throw new Error('Chat budget mismatch');
      if(current.limit_units===0 && current.spent_units===0 && current.reserved_units===0) {
        const initial=await jsonRequest(`/v1/projects/${project}/budget`);
        await jsonRequest(`/v1/projects/${project}/budget`,{method:'PUT',headers:{'if-match':initial.response.headers.get('etag')},body:{limit_units:budget.limit_units,currency:'USD',unit_scale:1e9}});
      }
      const snapshot=await jsonRequest(`/v1/projects/${project}`);
      const policy=(snapshot.data.project||snapshot.data).data_policy;
      if(policy.allow_unknown_provider_retention!==standard) {
        await jsonRequest(`/v1/projects/${project}/data-policy`,{method:'PUT',headers:{'if-match':snapshot.response.headers.get('etag')},body:{...policy,allow_unknown_provider_retention:standard}});
      }
      return;
    }
    const organization=client?.organization_id || (await jsonRequest('/v1/organizations',{method:'POST',body:{name:'Kyro — Chat local'}})).data.id;
    // Save provisioning steps before moving on. A failure cannot silently create a second funded project.
    await save({organization_id:organization,provisioning:true});
    const created=(await jsonRequest('/v1/projects',{method:'POST',body:{organization_id:organization,name:'Kyro — Conversation Nano',data_policy:{allow_unknown_provider_retention:standard,allowed_destinations:['nebius-chat'],allowed_categories:['user_request'],allowed_purposes:['conversation'],limits:{max_input_bytes:32768,max_input_tokens:262144,max_output_tokens:2048,max_deadline_ms:60000,max_response_bytes:48000,max_retention_seconds:0}},limits:{max_active_jobs:1,max_queued_jobs:1,max_job_attempts:1,job_ttl_secs:90,max_revisions:10}}})).data;
    project=(created.project||created).id; await save({organization_id:organization,project_id:project});
    const current=await jsonRequest(`/v1/projects/${project}/budget`);
    await jsonRequest(`/v1/projects/${project}/budget`,{method:'PUT',headers:{'if-match':current.response.headers.get('etag')},body:{limit_units:budget.limit_units,currency:'USD',unit_scale:1e9}});
  }
  async function ensure() {
    if(!await qualification()) throw Object.assign(new Error(blocked.message),{code:blocked.code});
    if(!initPromise) initPromise=provision(initialize).catch(async error=>{
      initPromise=null;
      if(session?.csrf) {try {await request('/v1/auth/logout',{method:'POST'});} catch { /* Preserve the original error. */ }}
      session=null;throw error;
    });
    await initPromise;
  }
  return {
    ...createPlansService({ jsonRequest, async ensureSession() {
      if (!session) {
        if (!renewalPromise) renewalPromise = login().finally(() => { renewalPromise = null; });
        await renewalPromise;
      }
    } }),
    async close() {
      if(session?.csrf) {
        try {
          // Closing the bridge cancels its outstanding jobs; uncertain holds remain durable.
          await Promise.all([...jobs].map(job=>jsonRequest(`/v1/projects/${project}/jobs/${job}`,{method:'DELETE'}).catch(()=>{})));
          await request('/v1/auth/logout',{method:'POST'});
        } finally {session=null;initPromise=null;}
      }
    },
    async status() {
      if(!await qualification()) return blocked;
      try {
        await ensure(); const budget=(await jsonRequest(`/v1/projects/${project}/budget`)).data;
        const destination=(await readJson('models.json')).destinations.find(d=>d.id==='nebius-chat');
        const model=destination.models[0], price=model.pricing;
        const ceil=n=>(n+999999n)/1000000n;
        const reserve=ceil(BigInt(destination.nebius?.context_tokens||model.max_input_tokens)*BigInt(price.input_units_per_million_tokens))+ceil(2048n*BigInt(price.output_units_per_million_tokens));
        const available=BigInt(budget.limit_units)-BigInt(budget.spent_units)-BigInt(budget.reserved_units);
        return {ready:available>=reserve,code:available<reserve?'budget_exceeded':undefined,model:MODEL,budget:{spentUsd:budget.spent_units/1e9,reservedUsd:budget.reserved_units/1e9,limitUsd:budget.limit_units/1e9},message:available<reserve?failures.budget_exceeded:destination.nebius?.provider_standard_retention_accepted?'Nano · Nebius · conditions habituelles du compte':'Nano · Nebius'};
      }
      catch { return {ready:false,code:'runtime_unavailable',message:'Le runtime Nano est arrêté ou incomplet. Lancez scripts/nebius-runtime.ps1 -Profile Chat -Action Start.'}; }
    },
    async send(input) {
      await ensure();
      if(!input || Object.keys(input).some(k=>!['key','messages','contextTokens'].includes(k)) || !UUID.test(input.key) || ![4096,8192,16384].includes(input.contextTokens) || !Array.isArray(input.messages) || !input.messages.length || input.messages.length>128 || Buffer.byteLength(JSON.stringify(input))>32768 || input.messages.some((m,i)=>!m || Object.keys(m).some(k=>!['role','content'].includes(k)) || m.role!==(i%2===0?'user':'assistant') || typeof m.content!=='string' || !m.content.trim() || Buffer.byteLength(m.content)>16384) || input.messages.at(-1).role!=='user') throw Object.assign(new Error('Le message dépasse les limites du contexte choisi.'),{code:'resource_limit'});
      const snapshot=(await jsonRequest(`/v1/projects/${project}`)).data;
      const revision=(snapshot.project||snapshot).current_revision;
      let data;
      try { ({data}=await jsonRequest(`/v1/projects/${project}/jobs`,{method:'POST',headers:{'if-match':`"rev-${revision}"`,'idempotency-key':input.key},body:{payload:{kind:'model_call',request:{destination_id:'nebius-chat',model:MODEL,input:{purpose:'conversation',categories:['user_request'],content:{messages:input.messages,context_tokens:input.contextTokens}},max_output_tokens:2048,deadline_ms:60000}},max_attempts:1,ttl_seconds:90}})); }
      catch(error) { if(!error.code) error.code='transport_unknown'; throw error; }
      jobs.add(data.id); return {jobId:data.id};
    },
    async watch(jobId, after, onEvent, signal) {
      if(!UUID.test(jobId)||!jobs.has(jobId)||typeof after!=='string'||after.length>100) throw new Error('Invalid chat stream');
      if(!session || !project) throw new Error('Session locale absente.');
      const response=await request(`/v1/projects/${project}/jobs/${jobId}/stream`,{headers:after?{'last-event-id':after}:{},signal});
      if(!response.ok || !response.headers.get('content-type')?.startsWith('text/event-stream')) throw new Error('Le flux local est interrompu.');
      let buffer=''; const decoder=new TextDecoder(); let bytes=0;
      for await(const chunk of response.body) {
        bytes+=chunk.length; if(bytes>1_048_576) throw new Error('Chat stream too large');
        buffer+=decoder.decode(chunk,{stream:true}); if(buffer.length>131072) throw new Error('Chat frame too large');
        let end;
        while((end=buffer.indexOf('\n\n'))>=0) {
          const frame=buffer.slice(0,end); buffer=buffer.slice(end+2);
          const lines=frame.split('\n'); const data=lines.filter(l=>l.startsWith('data:')).map(l=>l.slice(5).trimStart()).join('\n');
          if(!data) continue;
          const type=lines.find(l=>l.startsWith('event:'))?.slice(6).trim();
          const id=lines.find(l=>l.startsWith('id:'))?.slice(3).trim();
          if(!['delta','complete'].includes(type)) throw new Error('Invalid chat event');
          await onEvent({type,id,data:JSON.parse(data)});
          if(type==='complete') return;
        }
      }
      throw new Error('Le flux local est interrompu.');
    },
    async cancel(jobId) { if(!UUID.test(jobId)||!jobs.has(jobId)||!session||!project) throw new Error('Invalid chat job'); await jsonRequest(`/v1/projects/${project}/jobs/${jobId}`,{method:'DELETE'}); return {cancelled:true}; },
  };
}
module.exports={createChatService};
