export type ChatMessage = { role: 'user' | 'assistant'; content: string };
export type ChatInput = { key: string; messages: ChatMessage[]; contextTokens: number };
export type ChatStatus = { ready: boolean; code?: string; message: string; budget?: {spentUsd:number; reservedUsd:number; limitUsd:number} };
export type ChatEvent = { type: 'delta' | 'complete'; id?: string; data: {text?:string; status?:string; error_code?:string; effect_status?:string; output?:{text:string;truncated:boolean}} };
type Reply<T> = { value?: T; error?: string; code?:string };
type NativeChat = {
  status(): Promise<Reply<ChatStatus>>;
  send(input:ChatInput):Promise<Reply<{jobId:string}>>;
  cancel(jobId:string):Promise<Reply<unknown>>;
  watch(token:string,jobId:string,after:string,onEvent:(event:ChatEvent)=>void):Promise<Reply<unknown>>;
  unwatch():Promise<unknown>;
};
declare global { interface Window { kyroChat?:NativeChat } }
function unwrap<T>(reply:Reply<T>):T {
  if(reply.error || reply.value===undefined) throw Object.assign(new Error(reply.error||'Réponse locale invalide.'),{code:reply.code});
  return reply.value;
}
async function local<T>(action:string, value:unknown):Promise<T> {
  const response=await fetch(`/__kyro_chat/${action}`,{method:'POST',headers:{'content-type':'application/json','x-kyro-local':'1'},body:JSON.stringify(value)});
  return unwrap(await response.json());
}
export const chatApi = {
  status:()=> window.kyroChat ? window.kyroChat.status().then(unwrap) : local<ChatStatus>('status',null),
  send:(input:ChatInput)=> window.kyroChat ? window.kyroChat.send(input).then(unwrap) : local<{jobId:string}>('send',input),
  cancel:(jobId:string)=> window.kyroChat ? window.kyroChat.cancel(jobId).then(unwrap) : local('cancel',jobId),
  async watch(jobId:string,after:string,onEvent:(event:ChatEvent)=>void,signal:AbortSignal) {
    if(window.kyroChat) {
      const native=window.kyroChat;
      const abort=()=>{ void native.unwatch(); };
      signal.addEventListener('abort',abort,{once:true});
      try { unwrap(await native.watch(crypto.randomUUID(),jobId,after,onEvent)); }
      finally { signal.removeEventListener('abort',abort); }
      return;
    }
    const response=await fetch('/__kyro_chat/watch',{method:'POST',headers:{'content-type':'application/json','x-kyro-local':'1'},body:JSON.stringify({jobId,after}),signal});
    if(!response.headers.get('content-type')?.startsWith('text/event-stream')) { unwrap(await response.json()); throw new Error('Flux local invalide.'); }
    const reader=response.body!.getReader(), decoder=new TextDecoder();
    let buffer='', bytes=0;
    try {
      while(true) {
        const {done,value}=await reader.read(); if(done) return;
        bytes+=value.length; if(bytes>1_048_576) throw new Error('Flux trop volumineux.');
        buffer+=decoder.decode(value,{stream:true}); if(buffer.length>131072) throw new Error('Fragment trop volumineux.');
        let end;
        while((end=buffer.indexOf('\n\n'))>=0) {
          const frame=buffer.slice(0,end); buffer=buffer.slice(end+2);
          const data=frame.split('\n').find(l=>l.startsWith('data:'))?.slice(5);
          if(data) onEvent(JSON.parse(data));
        }
      }
    } finally { await reader.cancel(); reader.releaseLock(); }
  },
};
