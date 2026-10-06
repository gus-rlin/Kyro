// Deterministic P3 extension of the existing versioned control API document.
import {readFile,writeFile} from 'node:fs/promises';
const path=new URL('../../docs/backend/partie-1/openapi.v1.json',import.meta.url);
const doc=JSON.parse(await readFile(path,'utf8'));
const ref=name=>({$ref:`#/components/schemas/${name}`});
const text=(maxLength=8192)=>({type:'string',maxLength});
const integer=(minimum=0,maximum=Number.MAX_SAFE_INTEGER)=>({type:'integer',minimum,maximum});
const uuid={type:'string',format:'uuid'};
const nullable=s=>({anyOf:[s,{type:'null'}]});
const array=(items,maxItems)=>({type:'array',items,maxItems});
const set=(items,maxItems)=>({...array(items,maxItems),uniqueItems:true});
const object=(properties,required=Object.keys(properties))=>({type:'object',properties,required,additionalProperties:false});
const map=(schema,maxProperties)=>({type:'object',maxProperties,additionalProperties:schema});
const roles=['orchestrator','pixel','moka','kiwi','biscotte','review','security'];
const s=doc.components.schemas;
s.DataPolicy.properties.accepted_unknown_retention_purposes={type:'array',uniqueItems:true,maxItems:3,items:{type:'string',enum:['planning','generation','review']},description:'Explicit purpose-scoped acceptance of unknown provider retention. Absent/empty refuses unknown retention for agents; existing chat consent does not enable this field.'};
s.AgentRole={type:'string',enum:roles};
s.AgentStatus={type:'string',enum:['planning','planned','executing','reviewing','integrating','building','verified','blocked','cancelled']};
s.AgentLimits=object({max_calls:integer(4,128),max_tokens:integer(1024,2000000),max_output_tokens:integer(128,8192),call_timeout_ms:integer(100,120000),ttl_seconds:integer(30,7200),max_task_attempts:integer(1,3),context_bytes:integer(4096,65536)});
s.StartAgentRequest=object({request:{...text(8192),minLength:1},limits:ref('AgentLimits'),plan_only:{type:'boolean'}});
s.AgentResource={oneOf:[object({kind:{const:'node'},id:text(64)}),object({kind:{const:'property'},id:text(64),key:text(128)}),object({kind:{const:'preference'},key:text(128)})]};
s.AgentComponent=object({id:text(64),version:text(64)});
s.TaskContract=object({id:text(64),objective:text(2048),components:array(ref('AgentComponent'),16),reads:set(ref('AgentResource'),64),writes:set(ref('AgentResource'),32),dependencies:set(text(64),32),invariants:set(text(128),64),capabilities:set(text(128),256),protected_criteria:set(text(64),16),max_attempts:integer(1,3),deterministic:nullable(ref('ChangeSet'))},['id','objective','components','reads','writes','dependencies','invariants','max_attempts']);
s.TaskContract.description='Capabilities, protected criteria and shared invariants are derived or augmented by the server from signed admitted manifests. Supplied capability or criteria declarations confer no authority.';
s.AgentPlan=object({objective:text(8192),tasks:array(ref('TaskContract'),32),missing_capabilities:array(text(8192),32)});
s.AgentTaskResult=object({task_id:text(64),changes:ref('ChangeSet'),limitations:array(text(8192),32)});
s.AgentRetainedResult=object({contract:ref('TaskContract'),result:ref('AgentTaskResult')});
s.AgentReview=object({candidate_digest:text(64),approved:{type:'boolean'},findings:array(text(8192),32)});
s.AgentCall=object({id:uuid,epoch:integer(1,4294967295),role:ref('AgentRole'),task_id:nullable(text(64)),attempt:integer(1,3),job_id:uuid,registration:ref('ModelRegistrationSnapshot'),request_digest:text(64),reserved_tokens:integer(0,2000000),result_digest:nullable(text(64)),failure:nullable(text(256))});
s.AgentMemory=object({id:uuid,role:ref('AgentRole'),kind:{type:'string',enum:['fact','hypothesis','decision','diagnostic']},source:text(200),revision:integer(),text:text(4096)});
s.AgentCheckpoint=object({role:ref('AgentRole'),epoch:integer(1,4294967295),objective:text(8192),plan_digest:nullable(text(64)),task_ids:array(text(64),32),memory_ids:array(uuid,512),call_ids:array(uuid,128),remaining_calls:integer(0,128),remaining_tokens:integer(0,2000000),source_revision:integer(),run_version:integer(1),status:ref('AgentStatus'),protected_criteria_digest:text(64),result_digests:map(text(64),32),unresolved_diagnostic:nullable(text(256)),build_job_id:nullable(uuid),created_at:{type:'string',format:'date-time'}});
s.AgentRun=object({id:uuid,project_id:uuid,actor_id:uuid,version:integer(1),epoch:integer(1,4294967295),environment:{type:'string',enum:['development','production']},source_revision:integer(),snapshot:ref('AppSpec'),request:ref('StartAgentRequest'),catalogue_revision:integer(1),catalogue_digest:text(64),protected_criteria:set(text(64),1024),status:ref('AgentStatus'),plan:nullable(ref('AgentPlan')),calls:array(ref('AgentCall'),128),results:map(ref('AgentTaskResult'),32),reviews:{...map(ref('AgentReview'),2),propertyNames:{enum:['review','security']}},retained_results:map(ref('AgentRetainedResult'),32),memory:array(ref('AgentMemory'),512),checkpoints:{...map(ref('AgentCheckpoint'),7),propertyNames:{enum:roles}},reserved_tokens:integer(0,2000000),candidate_digest:nullable(text(64)),integrated_revision:nullable(integer()),build_job_id:nullable(uuid),artifact_id:nullable(uuid),diagnostic:nullable(text(256)),deadline:{type:'string',format:'date-time'}});
s.AgentRun.description='Authoritative server response only; no endpoint accepts run status, results, evidence, budget resets, provider configuration or authority. Creation currently requires development. Verified is a candidate and does not mean published.';
s.AgentMeasurement=object({call_id:uuid,role:ref('AgentRole'),job_id:uuid,epoch:integer(1),attempt:integer(1,3),task_id:nullable(text(64)),job_status:text(32),contract_failure:nullable(text(256)),effect_id:nullable(uuid),status:text(32),registration:ref('ModelRegistrationSnapshot'),usage:nullable(ref('ModelUsage')),estimated_units:nullable(integer()),reserved_tokens:integer(0,2000000),effect_elapsed_ms:nullable(integer()),invoiced_cost:nullable(integer())});
s.AgentMeasurement.description='Provider usage may be unknown. Tariff estimates are not invoiced cost; effect_elapsed_ms is the persisted effect lifecycle, not pure model latency.';
s.AgentInstruction=object({request:{...text(8192),minLength:1}});
s.AgentCompaction=object({role:ref('AgentRole')});
doc.components.parameters.AgentRunId={name:'run_id',in:'path',required:true,schema:uuid};
doc.components.parameters.AgentVersion={name:'If-Match',in:'header',required:true,schema:{type:'string',pattern:'^"rev-[1-9][0-9]*"$'},description:'Expected agent run version, independently of the AppSpec source revision.'};
const param=name=>({$ref:`#/components/parameters/${name}`});
const error={$ref:'#/components/responses/Error'};
const operation=(operationId,method,schema,{body=null,creation=false,parameters=[]}={})=>({
  operationId,security:[method==='get'?{sessionCookie:[]}:{sessionCookie:[],csrfToken:[]}],
  parameters:[...(method==='get'?[]:[param(creation?'RevisionIfMatch':'AgentVersion')]),...(creation?[param('IdempotencyKey')]:[]),...parameters],
  ...(body?{requestBody:{required:true,content:{'application/json':{schema:ref(body)}}}}:{}),
  responses:{[creation?'202':'200']:{description:'Actor-scoped result; no-store',content:{'application/json':{schema}}},...Object.fromEntries([400,401,403,404,409,412,413,428,429,503].map(code=>[code,error]))},
});
const collection='/v1/projects/{project_id}/plans';
const item=`${collection}/{run_id}`;
s.AgentCapabilities=object({configured:{type:'boolean'},code:text(64),synthetic:{type:'boolean'},roles:array(object({role:ref('AgentRole'),model:text(256)}),7),executor_count:integer(0,4),component_count:integer(),tools:array(object({id:text(64),label:text(128),available:{type:'boolean'}}),5)});
s.AgentCapabilities.description='Authenticated read-only runtime inventory. Configured tools confer no project permissions, budget or provider policy; mutations recheck these independently.';
doc.paths[`${collection}/capabilities`]={parameters:[param('ProjectId')],get:operation('getAgentCapabilities','get',ref('AgentCapabilities'))};
doc.paths[collection]={parameters:[param('ProjectId')],get:operation('listAgentRuns','get',array(ref('AgentRun'),32)),post:operation('createAgentRun','post',ref('AgentRun'),{body:'StartAgentRequest',creation:true})};
doc.paths[item]={parameters:[param('ProjectId'),param('AgentRunId')],get:operation('getAgentRun','get',ref('AgentRun')),delete:operation('cancelAgentRun','delete',ref('AgentRun'))};
for(const [suffix,id,body,method] of [['advance','advanceAgentRun',null,'post'],['execute','executeAgentPlan',null,'post'],['instructions','reviseAgentInstructions','AgentInstruction','post'],['compaction','compactAgentRole','AgentCompaction','post'],['contract','replaceAgentContract','AgentPlan','put']]) {
  doc.paths[`${item}/${suffix}`]={parameters:[param('ProjectId'),param('AgentRunId')],[method]:operation(id,method,ref('AgentRun'),{body})};
}
for(const [suffix,id,schema,parameters] of [['memory','searchAgentMemory',array(ref('AgentMemory'),16),[{name:'q',in:'query',required:true,schema:{...text(256),minLength:1}}]],['history','getAgentHistory',array(ref('AgentRun'),16),[{name:'before',in:'query',schema:{type:'integer',format:'int64'}}]],['usage','getAgentUsage',array(ref('AgentMeasurement'),128),[]]]) {
  doc.paths[`${item}/${suffix}`]={parameters:[param('ProjectId'),param('AgentRunId')],get:operation(id,'get',schema,{parameters})};
}
doc.info.description='Versioned P1 control plane, P2 signed catalogue/factory, and P3 durable planning, controlled agents, memory and verified candidate orchestration. Actor-scoped authority; model credentials are worker-only. Publishing is separate.';
await writeFile(path,`${JSON.stringify(doc,null,2)}\n`);
console.log('P3 OpenAPI extension regenerated');
