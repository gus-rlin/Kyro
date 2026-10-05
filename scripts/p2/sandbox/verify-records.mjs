// Protected verifier input, mounted in a different Sentry from the candidate.
// A candidate never runs migrations or sees the administrator password/report.
import {createHash, createHmac, randomBytes, randomUUID} from 'node:crypto';
import {readFile, writeFile, mkdir} from 'node:fs/promises';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {connect} from 'node:net';
import assert from 'node:assert/strict';

const sha = value => createHash('sha256').update(value).digest('hex');
const receipt = value => sha(JSON.stringify(value));
const input = JSON.parse(await readFile('/input/context.json', 'utf8'));
const criteriaBytes = await readFile('/input/criteria.json');
const criteria = JSON.parse(criteriaBytes);
const report = {schema_version:1, kind:'protected_application_verification', run_id:input.run_id,
  image_digest:input.image_digest, binding_digest:input.binding_digest,
  criteria_digest:sha(criteriaBytes), checks:{}};
const pg = '/usr/lib/postgresql/15/bin/';
const adminPassword = randomBytes(32).toString('hex');
const adminEnvironment = {...process.env, PGPASSWORD:adminPassword};
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
async function command(file, args, stdin, env=adminEnvironment, tolerate=false) {
  const child = spawn(file, args, {env, stdio:['pipe','pipe','pipe']});
  let out = '', bytes = 0;
  child.stdout.on('data', chunk => { bytes += chunk.length; if (bytes <= 65536) out += chunk; });
  // The private database diagnostics are bounded and deliberately not copied to
  // receipts: a SQL error can contain fixture values or a connection password.
  child.stderr.on('data', () => {});
  if (stdin !== undefined) child.stdin.end(stdin); else child.stdin.end();
  const code = await new Promise((resolve,reject) => {
    child.once('error',reject); child.once('exit',resolve);
  });
  if ((!tolerate && code !== 0) || bytes > 65536) throw new Error('protected_command_failed');
  return {code, stdout:out.trim()};
}
async function sql(text, database='kyro_verify', env=adminEnvironment, tolerate=false) {
  return command(pg+'psql', ['-X','-v','ON_ERROR_STOP=1','-h','127.0.0.1','-p','15432',
    '-U',env === adminEnvironment ? 'verifier_owner' : 'kyro_app_runtime','-d',database,'-At'],text,env,tolerate);
}
async function check(name, inputs, test) {
  if (!criteria.required_checks.includes(name)) return;
  try {
    const observed = await test();
    report.checks[name] = {input_digest:receipt(inputs),observed_digest:receipt(observed),passed:true};
  } catch {
    report.checks[name] = {input_digest:receipt(inputs),observed_digest:receipt({result:'failed'}),passed:false};
    throw new Error('protected_check_failed:'+name);
  }
}
function jwt(claims) {
  const header = Buffer.from(JSON.stringify({alg:'HS256',typ:'JWT'})).toString('base64url');
  const payload = Buffer.from(JSON.stringify(claims)).toString('base64url');
  const signature = createHmac('sha256',input.session_key).update(header+'.'+payload).digest('base64url');
  return header+'.'+payload+'.'+signature;
}
const literal = value => "'"+String(value).replaceAll("'","''")+"'";
const nodes = input.nodes ?? {};
function nodeFor(component,action) {
  return Object.entries(nodes).find(([,node])=>node.component_id===component &&
    (!node.configuration.allowed_actions?.length || node.configuration.allowed_actions.includes(action)));
}
const entity = nodeFor('B031','create')?.[1].configuration.defaults?.entity ?? 'item';
assert.match(entity,/^[a-z][a-z0-9_]{0,63}$/);
async function session(tenant, app, roles=['admin','commerce_manager','scheduling.manage']) {
  const principal=randomUUID(), sid=randomUUID(), now=Math.floor(Date.now()/1000);
  const token=jwt({iss:'kyro-verifier',aud:'kyro-verifier',sub:principal,tenant_id:tenant,
    application_id:app,session_id:sid,iat:now,exp:now+600});
  const schema={fields:{value:{type:'string',required:true}}};
  await sql(`INSERT INTO app_tenants(id) VALUES('${tenant}') ON CONFLICT DO NOTHING;
    INSERT INTO app_applications(tenant_id,id) VALUES('${tenant}','${app}') ON CONFLICT DO NOTHING;
    INSERT INTO app_principals(tenant_id,id,display_name) VALUES('${tenant}','${principal}','synthetic');
    ${roles.map(role=>`INSERT INTO app_memberships(tenant_id,application_id,principal_id,role) VALUES('${tenant}','${app}','${principal}',${literal(role)});
      INSERT INTO app_role_permissions(tenant_id,application_id,role,permission) VALUES('${tenant}','${app}',${literal(role)},'*') ON CONFLICT DO NOTHING;`).join('\n')}
    INSERT INTO app_sessions(tenant_id,application_id,id,principal_id,token_hash,csrf_hash,expires_at,mfa_at)
      VALUES('${tenant}','${app}','${sid}','${principal}',decode('${sha(token)}','hex'),decode('${'00'.repeat(32)}','hex'),to_timestamp(${now+600}),clock_timestamp());
    INSERT INTO app_data_schemas(tenant_id,application_id,component_id,entity_kind,schema_version,schema_hash,definition,created_by)
      VALUES('${tenant}','${app}','data',${literal(entity)},1,decode('${receipt(schema)}','hex'),'${JSON.stringify(schema)}','${principal}') ON CONFLICT DO NOTHING;
    INSERT INTO app_quotas(tenant_id,application_id,quota_key,limit_value)
      SELECT '${tenant}','${app}',key,1000000 FROM unnest(ARRAY['records','storage_bytes','jobs','job_slots','effects']) key ON CONFLICT DO NOTHING;`);
  return {tenant,app,principal,sid,token};
}
async function operation(actor, action, payload, {key=randomUUID(),version=null,app=actor.app}={}) {
  return componentOperation(actor,'B031',action,payload,{key,version,app});
}
async function componentOperation(actor,component,action,payload,{key=randomUUID(),version=null,app=actor.app}={}) {
  const node = nodeFor(component,action);
  const route = node ? '/nodes/'+encodeURIComponent(node[0])+'/operations' : '/operations';
  const response = await fetch(`http://127.0.0.1:18081/v1/apps/${app}${route}`,{
    method:'POST',headers:{authorization:'Bearer '+actor.token,'content-type':'application/json'},
    body:JSON.stringify({component_id:component,action,payload,idempotency_key:key,expected_version:version}),
    signal:AbortSignal.timeout(10000)});
  const body = await response.json();
  return {status:response.status,body,no_store:response.headers.get('cache-control') === 'no-store'};
}
async function success(actor,component,action,payload,options={}) {
  const reply=await componentOperation(actor,component,action,payload,options);
  assert.equal(reply.status,200);assert.equal(reply.no_store,true);return reply.body;
}
async function booking(alice,bob) {
  if (!criteria.required_checks.includes('booking_capacity')) return;
  let slot, winners;
  const people=[alice];for(let i=0;i<3;i++)people.push(await session(alice.tenant,alice.app,['member']));
  await check('booking_capacity',{capacity:3,concurrent_requests:4},async()=>{
    const site=await success(alice,'B111','create_establishment',{name:'synthetic site',timezone:'UTC'});
    const resource=await success(alice,'B111','create_resource',{establishment_id:site.id,category:'service',name:'appointment',capacity:3});
    const when=new Date();let hour=Math.max(10,when.getUTCHours()+2);
    if(hour>17){when.setUTCDate(when.getUTCDate()+1);hour=10;}
    const day=when.toISOString().slice(0,10), weekday=(when.getUTCDay()+6)%7;
    const availability=await success(alice,'B112','set_availability',{resource_id:resource.id,weekday,timezone:'UTC',
      local_start:'09:00:00',local_end:'18:00:00',valid_from:day,valid_until:day});
    slot=await success(alice,'B113','create_slot',{resource_id:resource.id,availability_id:availability.id,
      starts_at:`${day}T${hour}:00:00Z`,ends_at:`${day}T${hour+1}:00:00Z`,capacity:3});
    const replies=await Promise.all(people.map(actor=>componentOperation(actor,'B114','reserve',{slot_id:slot.id,units:1})));
    winners=replies.map((reply,index)=>({reply,actor:people[index]})).filter(v=>v.reply.status===200);
    assert.equal(winners.length,3);assert.equal(replies.filter(v=>v.status===409).length,1);
    const db=await sql(`SELECT reserved_units FROM app_sched_slots WHERE tenant_id='${alice.tenant}' AND application_id='${alice.app}' AND id='${slot.id}'`);
    assert.equal(db.stdout,'3');return {confirmed:3,rejected:1,reserved:3};
  });
  await check('booking_cancel_scan',{repeated_cancel:true,repeated_scan:true},async()=>{
    const [scan,cancel]=winners;
    const key=randomUUID(),payload={booking_id:scan.reply.body.booking_id};
    const first=await success(scan.actor,'B119','check_in',payload,{key});
    assert.deepEqual(await success(scan.actor,'B119','check_in',payload,{key}),first);
    const cancelled={booking_id:cancel.reply.body.booking_id},cancelKey=randomUUID();
    const result=await success(cancel.actor,'B115','cancel_booking',cancelled,{key:cancelKey});
    assert.deepEqual(await success(cancel.actor,'B115','cancel_booking',cancelled,{key:cancelKey}),result);
    const db=await sql(`SELECT reserved_units FROM app_sched_slots WHERE tenant_id='${alice.tenant}' AND application_id='${alice.app}' AND id='${slot.id}'`);
    const count=await sql(`SELECT count(*) FROM app_sched_attendance WHERE tenant_id='${alice.tenant}' AND application_id='${alice.app}' AND booking_id='${payload.booking_id}'`);
    assert.equal(db.stdout,'2');assert.equal(count.stdout,'1');return {remaining_units:2,attendance_rows:1};
  });
  await check('booking_access',{other_tenant:true},async()=>{
    const denied=await componentOperation(bob,'B114','get_booking',{booking_id:winners[0].reply.body.booking_id});
    assert.equal(denied.status,404);return {status:404};
  });
}
async function support(alice) {
  if (!criteria.required_checks.includes('support_notes')) return;
  const requester=await session(alice.tenant,alice.app,['member']);
  const agent=await session(alice.tenant,alice.app,['member','support.manage']);
  const other=await session(alice.tenant,alice.app,['member','support.manage']);
  let team,ticket;
  await check('support_notes',{internal_projection:true},async()=>{
    team=await success(alice,'B014','create',{name:'synthetic team A'});
    const otherTeam=await success(alice,'B014','create',{name:'synthetic team B'});
    for(const [person,group] of [[agent,team],[other,otherTeam]]) {
      const invite=await success(alice,'B013','invite',{target_principal_id:person.principal,role:'member',expires_in_hours:1});
      await success(person,'B013','accept',{token:invite.token});
      await success(alice,'B014','member_add',{group_id:group.id,principal_id:person.principal});
    }
    ticket=await success(requester,'B133','ticket.create',{title:'synthetic ticket',description:'public question',priority:'normal',team_id:team.id});
    const privateNote=randomBytes(16).toString('hex');
    const note=await success(agent,'B133','ticket.add_internal_note',{id:ticket.id,body:privateNote},{version:1});
    assert.equal(note.data.messages.length,1);
    const customer=await success(requester,'B133','ticket.get',{id:ticket.id});
    assert.equal(customer.data.messages.length,0);assert(!JSON.stringify(customer).includes(privateNote));
    return {private_messages:1,requester_messages:0,private_note_digest:sha(privateNote)};
  });
  await check('support_team_scope',{foreign_team:true},async()=>{
    const read=await componentOperation(other,'B133','ticket.get',{id:ticket.id});
    const assign=await componentOperation(other,'B133','ticket.assign',{id:ticket.id,assignee_id:other.principal},{version:2});
    const list=await success(other,'B133','ticket.list',{limit:1});
    assert.equal(read.status,404);assert.equal(assign.status,404);assert.equal(list.items.length,0);
    const db=await sql(`SELECT version FROM app_records WHERE tenant_id='${alice.tenant}' AND application_id='${alice.app}' AND id='${ticket.id}'`);
    assert.equal(db.stdout,'2');return {read:404,assign:404,visible:0,version:2};
  });
  await check('support_revocation',{live_group_revocation:true},async()=>{
    await success(alice,'B014','member_remove',{group_id:team.id,principal_id:agent.principal});
    const denied=await componentOperation(agent,'B133','ticket.get',{id:ticket.id});assert.equal(denied.status,404);
    await success(requester,'B133','ticket.get',{id:ticket.id});return {agent:404,requester:200};
  });
}
async function stock(alice,bob) {
  if (!criteria.required_checks.includes('stock_capacity')) return;
  let product;
  await check('stock_capacity',{initial_units:5,concurrent_orders:8},async()=>{
    product=await success(alice,'B121','create',{sku:'synthetic-'+randomBytes(8).toString('hex'),name:'synthetic item',inventory_tracked:true});
    await success(alice,'B130','adjust',{product_id:product.id,delta_on_hand:5,reason:'initial stock'},{version:1});
    await success(alice,'B122','set_price',{product_id:product.id,currency:'EUR',amount_minor:2500,interval_unit:'one_time',interval_count:1,effective_at:new Date(Date.now()-1000).toISOString()});
    await success(alice,'B121','publish',{id:product.id},{version:1});
    const people=[];for(let i=0;i<8;i++)people.push(await session(alice.tenant,alice.app,['member']));
    const quotes=await Promise.all(people.map(p=>success(p,'B123','quote',{currency:'EUR',items:[{product_id:product.id,quantity:1}]})));
    const orders=await Promise.all(people.map((p,i)=>componentOperation(p,'B124','create',{quote_id:quotes[i].id})));
    assert.equal(orders.filter(v=>v.status===200).length,5);
    assert.equal(orders.filter(v=>v.status===429 && v.body.error==='quota_exceeded').length,3);
    const db=await sql(`SELECT on_hand||':'||reserved FROM app_commerce_inventory WHERE tenant_id='${alice.tenant}' AND application_id='${alice.app}' AND product_id='${product.id}'`);
    assert.equal(db.stdout,'5:5');return {accepted:5,rejected:3,on_hand:5,reserved:5};
  });
  await check('stock_receipt_replay',{repeated_receipt:true},async()=>{
    const current=await success(alice,'B130','get',{id:product.id});
    const key=randomUUID(),payload={product_id:product.id,delta_on_hand:2,reason:'synthetic receipt'};
    const options={key,version:current.version};
    const first=await success(alice,'B130','adjust',payload,options);
    assert.deepEqual(await success(alice,'B130','adjust',payload,options),first);
    const rows=await sql(`SELECT count(*) FROM app_commerce_inventory_ledger WHERE tenant_id='${alice.tenant}' AND application_id='${alice.app}' AND product_id='${product.id}' AND delta_on_hand=2`);
    const after=await success(alice,'B130','get',{id:product.id});assert.equal(rows.stdout,'1');assert.equal(after.on_hand,7);
    return {receipt_entries:1,on_hand:7};
  });
  await check('stock_access',{other_tenant:true},async()=>{
    const denied=await componentOperation(bob,'B130','get',{id:product.id});assert.equal(denied.status,404);return {status:404};
  });
}
async function networkDenied() {
  // Numeric destinations avoid DNS dependence and exercise the outer namespace.
  const targets=['1.1.1.1','169.254.169.254','10.245.202.1'];
  for (const host of targets) {
    const allowed=await new Promise(resolve => {
      const socket=connect({host,port:80});
      let settled=false;
      const done=value=>{if (!settled) {settled=true;socket.destroy();resolve(value);}};
      socket.setTimeout(500,()=>done(false));socket.once('connect',()=>done(true));socket.once('error',()=>done(false));
    });
    assert.equal(allowed,false);
  }
  return {targets:targets.length,connected:0};
}
let ready;
let failed = false;
try {
  assert.equal(process.getuid(),1000);
  assert(['records','composition'].includes(criteria.suite));
  await mkdir('/work/socket',{mode:0o700});
  await writeFile('/work/admin.pwd',adminPassword,{mode:0o600});
  await command(pg+'initdb',['-D','/work/db','-U','verifier_owner','--auth-local=reject',
    '--auth-host=scram-sha-256','--pwfile=/work/admin.pwd','--no-locale']);
  await command(pg+'pg_ctl',['-D','/work/db','-l','/work/database.log','-w','start','-o',
    '-h 127.0.0.1 -p 15432 -k /work/socket -cshared_buffers=16MB -cmax_connections=16 -clog_statement=none']);
  await sql('CREATE DATABASE kyro_verify;','postgres');
  await check('fresh_database',{database:'ephemeral'},async()=>{
    const empty=await sql("SELECT count(*) FROM pg_tables WHERE schemaname='public'");
    assert.equal(empty.stdout,'0');
    for (const path of input.migrations) await command(pg+'psql',['-X','-v','ON_ERROR_STOP=1',
      '-h','127.0.0.1','-p','15432','-U','verifier_owner','-d','kyro_verify','--single-transaction','-f',path]);
    await sql(`ALTER ROLE kyro_app_runtime PASSWORD '${input.runtime_password}';`);
    return {empty_tables:0,migrations:input.migrations.length};
  });
  await check('runtime_privileges',{role:'kyro_app_runtime'},async()=>{
    const safe=await sql(`SELECT NOT (rolsuper OR rolcreatedb OR rolcreaterole OR rolinherit OR rolreplication OR rolbypassrls)
      FROM pg_roles WHERE rolname IN ('kyro_app_runtime','kyro_app') ORDER BY rolname`);
    assert.equal(safe.stdout,'t\nt');
    const forced=await sql("SELECT bool_and(relrowsecurity AND relforcerowsecurity) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relname LIKE 'app_%' AND c.relkind='r'");
    assert.equal(forced.stdout,'t');
    const runtimeEnv={...process.env,PGPASSWORD:input.runtime_password};
    const adminDenied=await sql('SET ROLE verifier_owner;', 'kyro_verify',runtimeEnv,true);
    assert.notEqual(adminDenied.code,0);
    const ddlDenied=await sql('SET ROLE kyro_app; CREATE TABLE public.verifier_forbidden(id int);','kyro_verify',runtimeEnv,true);
    assert.notEqual(ddlDenied.code,0);
    return {safe_roles:2,forced_rls:true,admin_denied:true,ddl_denied:true};
  });
  const alice=await session(randomUUID(),input.application_id);
  const bob=await session(randomUUID(),randomUUID());
  const otherApp=await session(alice.tenant,randomUUID());
  ready=createServer((_request,response)=>{response.writeHead(200);response.end('ready');});
  await new Promise(resolve=>ready.listen(18082,'127.0.0.1',resolve));
  await check('http_health',{route:'/healthz'},async()=>{
    let response;
    for (let attempt=0;attempt<100;attempt++) {
      try {response=await fetch('http://127.0.0.1:18081/healthz',{signal:AbortSignal.timeout(500)});if(response.status===200)break;}catch{}
      await sleep(100);
    }
    assert.equal(response?.status,200);return {status:200};
  });
  const record=randomUUID(), key=randomUUID(), value=randomBytes(16).toString('hex');
  const payload={entity,id:record,values:{value}};
  let created;
  await check('record_nominal',{record,value_digest:sha(value)},async()=>{
    created=await operation(alice,'create',payload,{key});
    assert.equal(created.status,200);assert.equal(created.body.id,record);
    assert.equal(created.no_store,true);
    const db=await sql(`SELECT data->>'value' FROM app_records WHERE tenant_id='${alice.tenant}' AND application_id='${alice.app}' AND kind=${literal('data.'+entity)} AND id='${record}'`);
    assert.equal(db.stdout,value);return {persisted_digest:sha(db.stdout),status:created.status};
  });
  await check('idempotency',{key_digest:sha(key)},async()=>{
    const replay=await operation(alice,'create',payload,{key});
    assert.deepEqual(replay,created);
    const count=await sql(`SELECT count(*) FROM app_records WHERE tenant_id='${alice.tenant}' AND application_id='${alice.app}' AND kind=${literal('data.'+entity)} AND id='${record}'`);
    assert.equal(count.stdout,'1');return {count:1,replay_digest:receipt(replay)};
  });
  await check('record_validation',{extra_field:true},async()=>{
    const bad=await operation(alice,'create',{entity,values:{value,undeclared:'reject'}});
    assert.equal(bad.status,400);
    const count=await sql(`SELECT count(*) FROM app_records WHERE tenant_id='${alice.tenant}' AND application_id='${alice.app}' AND kind=${literal('data.'+entity)}`);
    assert.equal(count.stdout,'1');return {status:bad.status,count:1};
  });
  await check('foreign_tenant',{record},async()=>{
    const denied=await operation(bob,'get',{entity,id:record});
    assert.equal(denied.status,404);assert.equal(denied.body.error,'not_found');return {status:404};
  });
  await check('cross_application',{record},async()=>{
    const scoped=await operation(otherApp,'get',{entity,id:record});assert.equal(scoped.status,404);
    const forgedRoute=await operation(alice,'get',{entity,id:record},{app:otherApp.app});
    assert.equal(forgedRoute.status,404);return {scope:404,route:404};
  });
  await check('stale_version',{record,expected_version:99},async()=>{
    const conflict=await operation(alice,'update',{entity,id:record,values:{value:'wrong'}},{version:99});
    assert.equal(conflict.status,409);
    const db=await sql(`SELECT version||':'||(data->>'value') FROM app_records WHERE tenant_id='${alice.tenant}' AND application_id='${alice.app}' AND kind=${literal('data.'+entity)} AND id='${record}'`);
    assert.equal(db.stdout,'1:'+value);return {status:409,persisted_digest:sha(db.stdout)};
  });
  await booking(alice,bob);await support(alice);await stock(alice,bob);
  await check('session_revocation',{session_id:alice.sid},async()=>{
    await sql(`UPDATE app_sessions SET revoked_at=clock_timestamp() WHERE tenant_id='${alice.tenant}' AND application_id='${alice.app}' AND id='${alice.sid}'`);
    const denied=await operation(alice,'get',{entity,id:record});assert.equal(denied.status,401);return {status:401};
  });
  await check('network_denied',{network:'outer-loopback-only'},networkDenied);
  assert.deepEqual(Object.keys(report.checks).sort(),[...criteria.required_checks].sort());
} catch(error) {
  failed=true;
  // Only a fixed check identifier is printed, never SQL, tokens or an exception.
  const name=String(error?.message).match(/^protected_check_failed:([a-z_]+)$/)?.[1] ?? 'setup';
  process.stderr.write('verification_failed:'+name+'\n');
} finally {
  ready?.close();
  await command(pg+'pg_ctl',['-D','/work/db','-m','immediate','-w','stop'],undefined,adminEnvironment,true).catch(()=>{});
  process.stdout.write(JSON.stringify(report)+'\n');
  process.exitCode=failed ? 1 : 0;
}
