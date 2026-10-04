import { useEffect, useRef, useState } from 'react';
import { chatApi, type ChatEvent, type ChatInput, type ChatStatus } from './chat-api';
import { conversationContext, type Turn } from './chat-context';

const failures:Record<string,string>={
  budget_exceeded:'Le plafond de 1 € est atteint ou réservé. Aucun nouvel appel ne sera envoyé.',
  forbidden:'Le message a été refusé. Retirez les secrets éventuels avant de réessayer.',
  invalid_provider_response:'La réponse du fournisseur est invalide. La réservation peut rester bloquée.',
  resource_limit:'Le contexte dépasse les limites. Réduisez le message ou l’historique.',
};
export function useChat() {
  const [status,setStatus]=useState<ChatStatus>({ready:false,message:'Connexion au chat…'});
  const [turns,setTurns]=useState<Turn[]>([]);
  const [busy,setBusy]=useState(false), [error,setError]=useState(''), [recoverable,setRecoverable]=useState(false);
  const [canStop,setCanStop]=useState(false);
  const active=useRef<{input:ChatInput;jobId?:string;cursor:string;turn:string;controller:AbortController;done:boolean}|null>(null);
  const mounted=useRef(true);
  async function refresh() {
    try { const value=await chatApi.status(); if(mounted.current) setStatus(value); }
    catch { if(mounted.current) setStatus({ready:false,message:'Le pont local est indisponible. Redémarrez Kyro ou son aperçu.'}); }
  }
  useEffect(()=>{ mounted.current=true; void refresh(); return()=>{mounted.current=false;active.current?.controller.abort();}; },[]);
  function update(id:string,change:Partial<Turn>) { if(mounted.current) setTurns(old=>old.map(t=>t.id===id?{...t,...change}:t)); }
  async function follow() {
    const current=active.current; if(!current) return;
    setError(''); setRecoverable(false);
    try {
      if(!current.jobId) current.jobId=(await chatApi.send(current.input)).jobId;
      if(mounted.current) setCanStop(true);
      let attempts=0;
      while(!current.done && !current.controller.signal.aborted) {
        try {
          await chatApi.watch(current.jobId,current.cursor,(event:ChatEvent)=>{
            if(!mounted.current || current.done) return;
            if(event.type==='delta' && typeof event.data.text==='string' && event.id) {
              const parts=event.id.split(':'); const previous=current.cursor.split(':');
              if(parts[0]!==current.jobId || parts.length!==3 || !/^\d+$/.test(parts[1]) || !/^\d+$/.test(parts[2]) || Number(parts[2])>1024) throw new Error('Curseur invalide.');
              if(current.cursor && parts[1]===previous[1] && Number(parts[2])<=Number(previous[2])) return;
              if(current.cursor && (parts[1]!==previous[1] || Number(parts[2])!==Number(previous[2])+1)) throw new Error('Fragment manquant.');
              current.cursor=event.id;
              setTurns(old=>old.map(t=>t.id===current.turn?{...t,state:'generating',assistant:t.assistant+event.data.text}:t));
            } else if(event.type==='complete') {
              const data=event.data;
              if(data.status==='succeeded' && data.effect_status==='succeeded' && typeof data.output?.text==='string') {
                update(current.turn,{state:'complete',assistant:data.output.text,note:data.output.truncated?'Réponse tronquée : limite de 2 048 tokens atteinte.':undefined});
              } else {
                const uncertain=data.status==='unknown' || data.effect_status==='unknown';
                update(current.turn,{state:'interrupted',note:uncertain?'Réponse interrompue. Usage final inconnu : la réservation est conservée.':failures[data.error_code||'']||'Réponse arrêtée ou refusée. Cet échange est exclu du prochain contexte.'});
              }
              current.done=true;
            }
          },current.controller.signal);
          if(!current.done) throw new Error('Flux interrompu.');
        } catch (failure) {
          if(current.controller.signal.aborted || current.done) break;
          if(++attempts>=3) throw failure;
          await new Promise<void>(resolve=>{ const timer=setTimeout(resolve,500); current.controller.signal.addEventListener('abort',()=>{clearTimeout(timer);resolve();},{once:true}); });
        }
      }
      if(current.done) { active.current=null; if(mounted.current) {setBusy(false);setCanStop(false);void refresh();} }
    } catch (failure) {
      if(!mounted.current || current.controller.signal.aborted) return;
      const message=failure instanceof Error?failure.message:'Le chat local est indisponible.';
      const code=(failure as {code?:string}).code;
      if(!current.jobId && code && code!=='transport_unknown') {
        update(current.turn,{state:'error',note:message}); active.current=null; setBusy(false);setCanStop(false);void refresh();
      } else setRecoverable(true);
      setError(current.jobId?`${message} Reprendre le flux conserve le même appel.`:code==='transport_unknown'? 'Envoi incertain. Réessayer retrouve la même demande sans la doubler.':message);
    }
  }
  function send(text:string,contextTokens:number):boolean {
    if(active.current || !text.trim()) return false;
    if(!status.ready) {
      setError(status.code==='zero_retention_unconfirmed'
        ? 'Votre message reste dans le brouillon. Pour activer Nano, la confirmation Nebius de non-conservation des messages manque encore. Une fois l’activation effectuée, cliquez sur Revérifier puis envoyez votre message.'
        : `${status.message} Votre brouillon est conservé. Cliquez sur Revérifier une fois le problème résolu.`);
      return false;
    }
    try {
      const input={key:crypto.randomUUID(),messages:conversationContext(turns,text.trim(),contextTokens),contextTokens};
      const id=crypto.randomUUID(); active.current={input,turn:id,cursor:'',controller:new AbortController(),done:false};
      setTurns(old=>[...old,{id,user:text.trim(),assistant:'',state:'waiting'}]); setBusy(true); void follow(); return true;
    } catch(failure) {setError((failure as Error).message); return false;}
  }
  async function stop() {
    const current=active.current; if(!current?.jobId) return;
    try { await chatApi.cancel(current.jobId); update(current.turn,{note:'Arrêt demandé…'}); if(recoverable) void follow(); }
    catch(failure) {setError((failure as Error).message);}
  }
  return {status,turns,busy,error,recoverable,send,stop,refresh,retry:follow,canStop};
}
