// Operator evaluator: bind successful, retained recipes to the exact registry.
// This evaluates local synthetic integration, never production providers.
import fs from 'node:fs';
import path from 'node:path';
import { createHash, randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const canonical = value => JSON.stringify(sort(value));
function sort(value) {
  if (Array.isArray(value)) return value.map(sort);
  if (value && typeof value === 'object') return Object.fromEntries(Object.keys(value).sort().map(k => [k, sort(value[k])]));
  return value;
}
function read(relative, max = 16777216) {
  if (!/^[a-zA-Z0-9._/-]+$/.test(relative) || relative.split('/').some(p => !p || p === '.' || p === '..')) throw Error('unsafe report path');
  let current = root;
  for (const segment of relative.split('/')) {
    current = path.join(current, segment);
    if (fs.lstatSync(current).isSymbolicLink()) throw Error('symlink refused');
  }
  const stat = fs.statSync(current);
  if (!stat.isFile() || stat.size > max) throw Error('report size refused');
  return fs.readFileSync(current);
}
function tests(source) {
  return [...source.matchAll(/#\[(?:tokio::)?test\][\s\S]*?\b(?:async\s+)?fn\s+(\w+)\s*\(/g)].map(m => m[1]);
}
function checkReport(descriptor) {
  if (descriptor.exit_code !== 0 || !/^[a-f0-9]{64}$/.test(descriptor.sha256)) throw Error('unsuccessful run');
  const bytes = read(descriptor.path);
  if (hash(bytes) !== descriptor.sha256) throw Error('report changed');
  const text = bytes.toString('utf8');
  if (/test result: FAILED|\bpanicked at\b|^error:/m.test(text) || !/test result: ok\. [1-9][0-9]* passed; 0 failed;/m.test(text)) throw Error('incomplete or failed test report');
  const passed=new Set();
  for(const match of text.matchAll(/^test (\S+) \.\.\. ([\s\S]*?)(?=^test \S+ \.\.\. |^test result:|$(?![\s\S]))/gm)) {
    if(/(?:^|\n)ok\s*$/m.test(match[2].trimEnd())) passed.add(match[1]);
  }
  return { path: descriptor.path, sha256: descriptor.sha256, text, passed };
}
const ids = [...Array.from({length:60},(_,i)=>i+1), ...Array.from({length:78},(_,i)=>i+81),160,161,162,163,164,169,171,172,173].map(n=>`B${String(n).padStart(3,'0')}`);
const familyFiles = n => n<=10 ? ['identity'] : n<=20 ? ['collaboration','composition_runtime'] : n<=30 ? ['governance','retention'] : n<=50 ? ['foundations','jobs'] : n<=60 ? ['jobs','connectors','webhooks'] : n<=90 ? ['documents','document_formats','search'] : n<=100 ? ['search','ai'] : n<=110 ? ['notifications','connectors','realtime'] : n<=120 ? ['compositions','scheduling','connectors'] : n<=130 ? ['compositions','persistence','stripe_ingress','webhooks','connectors'] : n<=140 ? ['business','compositions'] : n<=150 ? ['analytics'] : ['connectors','external_postgres'];
const special = { B001:['identity'], B084:['media'], B085:['media'], B107:['realtime'], B108:['realtime'] };

function main() {
  const [cataloguePath, indexPath, outputPath, matrixPath] = process.argv.slice(2);
  if (!matrixPath || process.argv.length!==6) throw Error('usage: pending.json run-index.json decisions.json matrix.json (repository relative paths)');
  const catalogue = JSON.parse(read(cataloguePath,8388608)).catalogue;
  const index = JSON.parse(read(indexPath,1048576));
  if (index.schema_version!==1 || index.validation_environment!=='synthetic_integration' || !/^[a-f0-9]{64}$/.test(index.source_digest)) throw Error('explicit synthetic run required');
  const actualIds = Object.keys(catalogue.entries).sort();
  if (canonical(actualIds)!==canonical([...ids].sort())) throw Error('incomplete 147 component registry');
  const reports = Object.fromEntries(['app','kernel','factory','factory_booking','factory_support','factory_stock','factory_all','isolation','media','store'].map(key=>[key,checkReport(index.reports[key])]));
  if (!index.test_inputs || Object.keys(index.test_inputs).length > 100) throw Error('test input binding absent');
  for (const [relative, digest] of Object.entries(index.test_inputs)) {
    if (!/^[a-f0-9]{64}$/.test(digest) || hash(read(relative,2097152)) !== digest) throw Error(`test input changed: ${relative}`);
  }
  const supportingInputs = [
    'crates/kyro-app/tests/support/mod.rs', 'crates/kyro-factory/tests/support/mod.rs',
    'crates/kyro-app/tests/fixtures/oidc-public-test-key.json', 'tests/fixtures/models.synthetic.e2e.json',
  ].map(relative => {
    if (!index.test_inputs[relative]) throw Error(`test support absent: ${relative}`);
    return {path:relative,sha256:index.test_inputs[relative]};
  });
  const kernelLine = reports.kernel.text.split(/\r?\n/).find(line=>line.includes('"component_kernel_observations"'));
  const kernel = JSON.parse(kernelLine?.slice(kernelLine.indexOf('{')) ?? 'null');
  if (!kernel?.durable_counts_unchanged || Object.keys(kernel.components).length!==139) throw Error('kernel observations absent');
  const sourceCache = new Map();
  function recipe(relative, reportKey, selected=null) {
    const bytes = read(relative,2097152), source=bytes.toString('utf8');
    if (index.test_inputs[relative] !== hash(bytes)) throw Error(`recipe source unbound: ${relative}`);
    const names = selected ?? tests(source);
    if (!names.length || names.some(name=>!reports[reportKey].passed.has(name))) throw Error(`recipe not executed: ${relative}`);
    return {path:relative,sha256:hash(bytes),tests:names,report:reports[reportKey].path,report_sha256:reports[reportKey].sha256};
  }
  const shared = recipe('crates/kyro-app/tests/qualification_kernel.rs','kernel');
  const factoryNominal = recipe('crates/kyro-worker/tests/factory_real.rs','factory_all',['durable_worker_builds_and_separate_attestor_commits_only_observed_release']);
  const familyNominals = ['booking','support','stock'].map(family => {
    const report=reports[`factory_${family}`];
    const line=report.text.split(/\r?\n/).find(value=>value.includes('"kind":"p1_worker_separate_attestor_real_build"'));
    const observed=JSON.parse(line?.slice(line.indexOf('{')) ?? 'null');
    if(observed?.composition!==family || observed.status!=='succeeded' || observed.providers!=='none' || observed.generation!==2) throw Error(`composition observation absent: ${family}`);
    return recipe('crates/kyro-worker/tests/factory_real.rs',`factory_${family}`,['durable_worker_builds_and_separate_attestor_commits_only_observed_release']);
  });
  const factoryContracts = recipe('crates/kyro-factory/tests/contracts.rs','factory');
  const factoryArtifacts = recipe('crates/kyro-factory/tests/artifacts.rs','factory');
  const factoryRegistry = recipe('crates/kyro-factory/tests/registry.rs','factory');
  const isolation = recipe('crates/kyro-factory/tests/isolation_real.rs','isolation');
  const store = recipe('crates/kyro-store/tests/factory.rs','store');
  const runId = randomUUID();
  const decisions=[], matrix=[];
  for (const id of actualIds) {
    const versions = Object.values(catalogue.entries[id]);
    if (versions.length!==1 || versions[0].admission!=='pending' || versions[0].qualification) throw Error('fresh pending registry required');
    const manifest=versions[0].component.manifest;
    if (manifest.source_digest!==index.source_digest || hash(Buffer.from(canonical(manifest.source_files)))!==index.source_digest) throw Error('source digest mismatch');
    for(const [relative,digest] of Object.entries(manifest.source_files)) {
      if(!sourceCache.has(relative)) sourceCache.set(relative,hash(read(relative,2097152)));
      if(sourceCache.get(relative)!==digest) throw Error(`source changed: ${relative}`);
    }
    const number=Number(id.slice(1));
    let nominal;
    if(number>160) nominal=[...familyNominals,factoryNominal];
    else {
      nominal=(special[id]??familyFiles(number)).flatMap(name=>{
        const relative=name==='media'?'crates/kyro-factory/tests/media_real.rs':`crates/kyro-app/tests/${name}.rs`;
        const bytes=read(relative,2097152);
        // Route-only capabilities are observed by their dedicated HTTP/parser
        // recipes; other bindings require the actual component literal.
        if(!special[id] && !bytes.toString('utf8').includes(`"${id}"`)) return [];
        return [recipe(relative,name==='media'?'media':'app')];
      });
      if(!nominal.length) throw Error(`nominal recipe absent: ${id}`);
      const observation=kernel.components[id];
      if(observation?.refusal!=='forbidden'||observation?.database_failure!=='unavailable') throw Error(`kernel verdict absent: ${id}`);
    }
    const cases = number>160 ? {
      nominal, refusal:[factoryContracts,factoryArtifacts,factoryRegistry],
      failure:[factoryContracts,factoryArtifacts,store], invariant:[factoryContracts,store,...(id==='B171'?[isolation]:[])]
    } : {nominal,refusal:[shared,...nominal],failure:[shared,...(id==='B084'||id==='B085'?nominal:[])],invariant:[shared,...nominal]};
    const scope=number>160?'protected_local_factory':'local_application_with_synthetic_providers';
    const detail={schema_version:1,component_id:id,version:manifest.version,scope,source_digest:index.source_digest,
      environment:index.environment,test_support:supportingInputs,cases,limitations:number>160?['Catalogue admissions in composition recipes are explicit signature fixtures; this decision uses the retained application recipes.','Docker daemon/controller are trusted; gVisor systrap, no KVM claim.']:['Database-failure case exercises admission before the handler; provider-specific failures are reported separately by family recipes.','No paid NVIDIA/Nebius or production provider qualification.']};
    const subject={...manifest,qualification_digest:'0'.repeat(64)};
    const receipts=Object.fromEntries(Object.entries(cases).map(([kind,recipes])=> {
      const input={id,kind,criteria_digest:manifest.criteria_digest,recipes:recipes.map(({path,sha256,tests})=>({path,sha256,tests}))};
      const observed=recipes.map(({report,report_sha256,tests})=>({report,report_sha256,tests}));
      return [kind,{passed:true,input_digest:hash(Buffer.from(canonical(input))),observed_digest:hash(Buffer.from(canonical(observed)))}];
    }));
    decisions.push({kind:'component_qualification',schema_version:1,component_id:id,version:manifest.version,
      // Rust serializes this struct in the manifest's declared field order.
      subject_digest:hash(Buffer.from(JSON.stringify(subject))),source_digest:index.source_digest,criteria_digest:manifest.criteria_digest,
      verifier_version:'kyro-component-verifier-1',run_id:runId,report_digest:hash(Buffer.from(canonical(detail))),validation_environment:'synthetic_integration',
      cases:receipts});
    matrix.push(detail);
  }
  for(const [relative,value] of [[outputPath,decisions],[matrixPath,{schema_version:1,run_id:runId,source_digest:index.source_digest,validation_environment:'synthetic_integration',entries:matrix}]]) {
    // Resolve through the same strict traversal checks used for inputs.
    if(!/^[a-zA-Z0-9._/-]+$/.test(relative)||relative.split('/').some(p=>!p||p==='.'||p==='..')) throw Error('unsafe output');
    const parent=path.dirname(relative);
    let current=root;
    for(const segment of parent.split('/')) {
      current=path.join(current,segment);
      if(fs.lstatSync(current).isSymbolicLink()) throw Error('output symlink refused');
    }
    const file=path.join(root,relative); fs.writeFileSync(file,canonical(value)+'\n',{flag:'wx'});
  }
  console.log(JSON.stringify({status:'prepared',components:decisions.length,source_digest:index.source_digest,run_id:runId,scope:'synthetic_integration',sealed:false}));
}
try { main(); } catch(error) { console.error(`qualification refused: ${error.message}`); process.exitCode=1; }
