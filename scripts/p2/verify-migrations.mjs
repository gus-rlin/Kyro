// Destructive operations are restricted to newly created synthetic test databases.
// Existing databases are never reset; failed trials are retained for diagnosis.
import {readFileSync,readdirSync} from 'node:fs';
import {createHash,randomUUID} from 'node:crypto';
import {spawnSync} from 'node:child_process';
import {resolve} from 'node:path';
const mode=process.argv[2];
if (!['application','control'].includes(mode)||process.argv.length!==3) throw Error('choose application or control');
const base=new URL(process.env.KYRO_P2_MIGRATION_TEST_ADMIN_URL??'');
if(base.username!=='kyro_admin'||!/^\/kyro_p[12]_test_[a-z0-9_]+$/.test(base.pathname)) throw Error('dedicated admin test database required');
const root=resolve(import.meta.dirname,'../..');
const crate=mode==='application'?'kyro-app':'kyro-store';
const binary=mode==='application'?'kyro-app-migrate':'kyro-migrate';
const variable=mode==='application'?'KYRO_APP_DATABASE_ADMIN_URL':'KYRO_DATABASE_ADMIN_URL';
const directory=resolve(root,`crates/${crate}/migrations`);
const migrations=readdirSync(directory).filter(n=>/^\d+_.+\.sql$/.test(n)).sort();
function psql(url,sql){
 const result=spawnSync('psql',['-X','--set=ON_ERROR_STOP=1','--no-align','--tuples-only','--dbname',url.toString()],{input:sql,encoding:'utf8',timeout:120000,maxBuffer:4*1024*1024});
 if(result.status!==0) throw Error('test SQL execution failed; database retained');
 return result.stdout.trim();
}
function migrate(url){
 const result=spawnSync(resolve(root,`target/debug/${binary}`),[],{env:{...process.env,[variable]:url.toString()},encoding:'utf8',timeout:120000,maxBuffer:1024*1024});
 if(result.status!==0) throw Error('migrator refused test database; database retained');
}
const observed=[];
for(const scenario of ['fresh','upgrade']){
 const name=`kyro_p${mode==='application'?2:1}_test_${scenario}_${randomUUID().replaceAll('-','')}`;
 psql(base,`CREATE DATABASE ${name} OWNER kyro_admin TEMPLATE template0;`);
 const url=new URL(base);url.pathname=`/${name}`;
 if(scenario==='upgrade'){
  let sql=`CREATE TABLE _sqlx_migrations(version bigint PRIMARY KEY,description text NOT NULL,installed_on timestamptz NOT NULL DEFAULT now(),success boolean NOT NULL,checksum bytea NOT NULL,execution_time bigint NOT NULL);\n`;
  for(const file of migrations.slice(0,-1)){
   const source=readFileSync(resolve(directory,file));
   const [prefix,...parts]=file.slice(0,-4).split('_');
   const description=parts.join(' ').replaceAll("'","''");
   const checksum=createHash('sha384').update(source).digest('hex');
   sql+=`BEGIN;\n${source.toString('utf8')}\nINSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES(${Number(prefix)},'${description}',true,decode('${checksum}','hex'),0);\nCOMMIT;\n`;
  }
  psql(url,sql);
 }
 migrate(url);migrate(url);
 const rows=JSON.parse(psql(url,`SELECT json_agg(json_build_object('version',version,'checksum',encode(checksum,'hex'),'success',success) ORDER BY version) FROM _sqlx_migrations;`));
 if(rows.length!==migrations.length||rows.some((row,i)=>!row.success||row.version!==Number(migrations[i].split('_')[0])||row.checksum!==createHash('sha384').update(readFileSync(resolve(directory,migrations[i]))).digest('hex'))) throw Error('migration checksum or replay mismatch');
 const proof=mode==='application'?psql(url,`SELECT json_build_object('roles_restricted',bool_and(NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole AND NOT rolbypassrls),'runtime_roles',count(*)) FROM pg_roles WHERE rolname IN ('kyro_app_runtime','kyro_app_auth_runtime');`):psql(url,`SELECT json_build_object('roles_restricted',bool_and(NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole AND NOT rolbypassrls),'runtime_roles',count(*)) FROM pg_roles WHERE rolname IN ('kyro_api','kyro_worker');`);
 const privileges=JSON.parse(proof);
 if(!privileges.roles_restricted||privileges.runtime_roles!==2) throw Error('runtime role drift');
 const tables=JSON.parse(psql(url,`SELECT json_agg(json_build_object('name',relname,'rls',relrowsecurity,'forced',relforcerowsecurity) ORDER BY relname) FROM pg_class WHERE relnamespace='public'::regnamespace AND relkind='r' AND relname<> '_sqlx_migrations';`));
 if(mode==='application'&&tables.some(row=>row.name.startsWith('app_')&&(!row.rls||!row.forced))) throw Error('application table without forced RLS');
 if(mode==='control'&&!tables.some(row=>row.name==='factory_artifacts'&&row.rls&&row.forced)) throw Error('artifact RLS missing');
 observed.push({scenario,database:name,migrations:rows.length,checksums_exact:true,replay_unchanged:true,privileges,tables});
}
console.log(JSON.stringify({schema_version:1,kind:'migration_trials',mode,source_base:'working tree',postgres:psql(base,'SHOW server_version;'),observed}));
