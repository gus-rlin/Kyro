// A bounded, synthetic preflight. This file is copied to a private tools
// container; it never mounts the shared checkout into the gVisor workload.
import {readFileSync,writeFileSync} from 'node:fs';
const file='/sandbox/config.json';
const value=JSON.parse(readFileSync(file,'utf8'));
value.root={path:'rootfs',readonly:true};
value.process={...value.process,terminal:false,args:['/bin/sh','-c','id; uname -a; test ! -e /workspace; test ! -e /var/run/docker.sock; echo KYRO_GVISOR_PREFLIGHT_OK'],env:['PATH=/usr/local/bin:/usr/bin:/bin','HOME=/tmp'],cwd:'/',user:{uid:1000,gid:1000},noNewPrivileges:true,capabilities:{bounding:[],effective:[],inheritable:[],permitted:[],ambient:[]},rlimits:[{type:'RLIMIT_NOFILE',hard:512,soft:512},{type:'RLIMIT_NPROC',hard:128,soft:128},{type:'RLIMIT_FSIZE',hard:67108864,soft:67108864}]};
value.linux={...value.linux,namespaces:[{type:'user'},{type:'pid'},{type:'ipc'},{type:'uts'},{type:'mount'}],uidMappings:[{containerID:0,hostID:1000,size:1}],gidMappings:[{containerID:0,hostID:1000,size:1}]};
value.mounts=value.mounts.filter(m=>!['/sys','/sys/fs/cgroup'].includes(m.destination));
writeFileSync(file,JSON.stringify(value));
