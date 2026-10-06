-- Planning metadata does not require application write authority. Derive every
-- event field from the actor's exact persisted run; no arbitrary payload/type.
CREATE FUNCTION public.kyro_append_agent_event(target_run UUID, target_version BIGINT) RETURNS VOID
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $$
DECLARE r public.agent_runs%ROWTYPE; target_project UUID; next_sequence BIGINT;
BEGIN
 IF session_user<>'kyro_api' THEN
  RAISE EXCEPTION 'agent event role denied' USING ERRCODE='42501';
 END IF;
 SELECT a.project_id INTO target_project FROM public.agent_runs a
  WHERE a.id=target_run AND a.version=target_version AND a.actor_id=public.kyro_actor_id()
   AND a.environment=public.kyro_environment();
 IF NOT FOUND THEN RAISE EXCEPTION 'agent event run not found' USING ERRCODE='P0002'; END IF;
 -- Both rights are cumulative, protected by the existing project/membership/grant locks.
 PERFORM 1 FROM public.kyro_lock_project_for_actor(target_project,ARRAY['read']::TEXT[]);
 IF NOT FOUND THEN RAISE EXCEPTION 'agent event read denied' USING ERRCODE='42501'; END IF;
 PERFORM 1 FROM public.kyro_lock_project_for_actor(target_project,ARRAY['execute']::TEXT[]);
 IF NOT FOUND THEN RAISE EXCEPTION 'agent event execute denied' USING ERRCODE='42501'; END IF;
 SELECT * INTO r FROM public.agent_runs a WHERE a.id=target_run AND a.version=target_version
  AND a.project_id=target_project AND a.actor_id=public.kyro_actor_id()
  AND a.environment=public.kyro_environment() FOR SHARE;
 IF NOT FOUND THEN RAISE EXCEPTION 'agent event run changed' USING ERRCODE='P0002'; END IF;
 UPDATE public.projects SET event_sequence=event_sequence+1,updated_at=clock_timestamp()
  WHERE id=target_project RETURNING event_sequence INTO next_sequence;
 INSERT INTO public.events(project_id,sequence,type,payload,actor_id)
  VALUES(target_project,next_sequence,'agent.run.changed',
   jsonb_build_object('run_id',r.id,'version',r.version,'status',r.state->>'status'),r.actor_id);
 INSERT INTO public.outbox_events(project_id,event_sequence,topic,payload)
  VALUES(target_project,next_sequence,'project.event',
   jsonb_build_object('project_id',target_project,'sequence',next_sequence,'type','agent.run.changed'));
END $$;
REVOKE ALL ON FUNCTION public.kyro_append_agent_event(UUID,BIGINT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.kyro_append_agent_event(UUID,BIGINT) TO kyro_api;
