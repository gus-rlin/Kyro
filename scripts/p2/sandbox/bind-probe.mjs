// Synthetic gofer persistence reproduction, not a build/quotas qualification.
import {readFileSync,writeFileSync} from 'node:fs';
const file='/sandbox/config.json';
const spec=JSON.parse(readFileSync(file,'utf8'));
spec.root={path:'/tools-root',readonly:true};
spec.process={...spec.process,terminal:false,user:{uid:0,gid:0},args:['/bin/sh','-c','printf public-synthetic-bind-probe > /output/probe.txt; ls -l /output; cat /output/probe.txt'],cwd:'/',env:['PATH=/usr/bin:/bin','HOME=/tmp'],noNewPrivileges:true,capabilities:{bounding:[],effective:[],inheritable:[],permitted:[],ambient:[]}};
spec.linux={...spec.linux,namespaces:[{type:'user'},{type:'pid'},{type:'ipc'},{type:'uts'},{type:'mount'}],uidMappings:[{containerID:0,hostID:1000,size:1}],gidMappings:[{containerID:0,hostID:1000,size:1}]};
spec.mounts=spec.mounts.filter(m=>!['/sys','/sys/fs/cgroup'].includes(m.destination));
spec.mounts.push({destination:'/output',type:'bind',source:'/sandbox/output',options:['rbind','rw','nosuid','nodev','noexec']});
writeFileSync(file,JSON.stringify(spec));
