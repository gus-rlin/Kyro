import type { ChatMessage } from './chat-api';
export type Turn = { id:string; user:string; assistant:string; state:'waiting'|'generating'|'complete'|'interrupted'|'error'; note?:string };
const bytes=(value:unknown)=>new TextEncoder().encode(JSON.stringify(value)).length;
// Reserve output, the server instruction, wire fields, and the provider template margin.
// This byte bound is intentionally conservative; the worker repeats the authoritative check.
export function conversationContext(turns:Turn[],last:string,contextTokens:number):ChatMessage[] {
  const messages:ChatMessage[]=[...turns.filter(t=>t.state==='complete').flatMap(t=>[
    {role:'user' as const,content:t.user}, {role:'assistant' as const,content:t.assistant},
  ]), {role:'user',content:last}];
  const limit=contextTokens-2048-1800;
  if(new TextEncoder().encode(last).length>16384 || bytes(messages.slice(-1))>limit) throw new Error('Ce message est trop volumineux pour le contexte choisi. Raccourcissez-le ou choisissez un contexte plus grand.');
  while(messages.length>1 && (bytes(messages)>limit || messages.length>127)) messages.splice(0,2);
  return messages;
}
