// Operator-owned probes only. This program has no credentials or project input.
import {spawn} from 'node:child_process';
import {Worker} from 'node:worker_threads';
import {connect} from 'node:net';
import {existsSync,openSync,writeSync,closeSync,statfsSync} from 'node:fs';
const mode=process.argv[2];
if(mode==='filesystem') {
  const forbidden=['/workspace','/var/run/docker.sock','/input/criteria.json','/attestor'];
  if(forbidden.some(existsSync))process.exit(1);
  const work=statfsSync('/work');
  process.stdout.write(JSON.stringify({mode,forbidden_absent:forbidden.length,workspace_capacity_bytes:work.bsize*work.blocks})+'\n');
} else if(mode==='cpu') {
  await Promise.all(Array.from({length:4},()=>new Promise((resolve,reject)=>{
    const worker=new Worker('const until=Date.now()+3000;let n=0;while(Date.now()<until)n++;',{eval:true});
    worker.once('exit',code=>code===0?resolve():reject(new Error('worker_failed')));worker.once('error',reject);
  })));
  process.stdout.write(JSON.stringify({mode,workers:4})+'\n');
} else if(mode==='network') {
  const targets=['1.1.1.1','169.254.169.254','10.245.202.1'];
  for(const host of targets) {
    const connected=await new Promise(resolve=>{
      const socket=connect({host,port:80});let settled=false;
      const done=value=>{if(!settled){settled=true;socket.destroy();resolve(value);}};
      socket.setTimeout(500,()=>done(false));socket.once('connect',()=>done(true));socket.once('error',()=>done(false));
    });
    if(connected)process.exit(1);
  }
  process.stdout.write(JSON.stringify({mode,targets:targets.length,connected:0})+'\n');
} else if(mode==='memory') {
  const retained=[];
  // Keep touching new physical pages. Virtual reservation alone is no proof.
  for(let i=0;i<192;i++)retained.push(Buffer.alloc(33554432,0x7a));
  process.stdout.write(JSON.stringify({mode,unexpected_completion:true,bytes:retained.length*33554432})+'\n');
  process.exitCode=1;
} else if(mode==='pids') {
  const children=[];let denied=false, reason;
  for(let i=0;i<512;i++) {
    let child;
    try {child=spawn('/bin/sleep',['60'],{stdio:'ignore'});}
    catch(error) {if(!['EAGAIN','ENOMEM'].includes(error.code))throw error;denied=true;reason=error.code;break;}
    const started=await new Promise(resolve=>{child.once('spawn',()=>resolve(true));child.once('error',error=>{reason=error.code;resolve(false);});});
    if(!started){denied=true;break;}children.push(child);
  }
  for(const child of children)child.kill('SIGKILL');
  await Promise.all(children.map(child=>new Promise(resolve=>child.once('exit',resolve))));
  process.stdout.write(JSON.stringify({mode,spawned:children.length,denied,reason})+'\n');
  process.exitCode=denied?0:1;
} else if(mode==='disk') {
  let denied=false, written=0;const block=Buffer.alloc(1048576,0x5a);
  for(let i=0;i<32 && !denied;i++) {
    let fd;
    try {fd=openSync('/output/part-'+i,'wx',0o600);for(let j=0;j<32;j++)written+=writeSync(fd,block);}
    catch(error){if(error.code==='ENOSPC')denied=true;else throw error;}
    finally{if(fd!==undefined)closeSync(fd);}
  }
  process.stdout.write(JSON.stringify({mode,written,denied})+'\n');process.exitCode=denied?0:1;
} else if(mode==='deadline') {
  await new Promise(resolve=>setTimeout(resolve,60000));process.exitCode=1;
} else {process.exitCode=1;}
