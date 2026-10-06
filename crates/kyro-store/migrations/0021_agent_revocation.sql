-- Trusted metadata closeout after execute/model authority disappears.
-- No effect is replayed or financially released here; P1 reconciles sending effects.
CREATE FUNCTION public.kyro_block_revoked_agent_run(target_run UUID) RETURNS BOOLEAN
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $$
DECLARE r public.agent_runs%ROWTYPE; previous_actor TEXT; previous_env TEXT;
BEGIN
 IF session_user<>'kyro_api' THEN RETURN FALSE; END IF;
 SELECT * INTO r FROM public.agent_runs WHERE id=target_run;
 IF NOT FOUND OR r.environment<>public.kyro_environment() OR NOT r.active THEN RETURN FALSE; END IF;
 PERFORM 1 FROM public.projects WHERE id=r.project_id FOR UPDATE;
 SELECT * INTO r FROM public.agent_runs WHERE id=target_run FOR UPDATE;
 IF NOT r.active THEN RETURN FALSE; END IF;
 previous_actor:=current_setting('kyro.actor_id',TRUE); previous_env:=current_setting('kyro.environment',TRUE);
 PERFORM set_config('kyro.actor_id',r.actor_id::TEXT,TRUE);
 PERFORM set_config('kyro.environment',r.environment,TRUE);
 IF public.kyro_actor_has_action(r.project_id,'read') AND public.kyro_actor_has_action(r.project_id,'execute') AND public.kyro_actor_has_action(r.project_id,'model') THEN
  PERFORM set_config('kyro.actor_id',COALESCE(previous_actor,''),TRUE);
  PERFORM set_config('kyro.environment',COALESCE(previous_env,''),TRUE);
  RETURN FALSE;
 END IF;
 UPDATE public.jobs j SET cancel_requested=TRUE WHERE j.project_id=r.project_id AND j.actor_id=r.actor_id
  AND j.environment=r.environment AND j.status IN ('pending','running')
  AND (EXISTS(SELECT 1 FROM jsonb_array_elements(r.state->'calls') c WHERE (c->>'job_id')::UUID=j.id)
    OR r.state->>'build_job_id'=j.id::TEXT);
 UPDATE public.agent_runs SET active=FALSE,version=r.version+1,updated_at=clock_timestamp(),
  state=jsonb_set(jsonb_set(jsonb_set(r.state,'{status}','"blocked"'),'{diagnostic}','"permission_revoked"'),'{version}',to_jsonb(r.version+1)) WHERE id=r.id;
 PERFORM set_config('kyro.actor_id',COALESCE(previous_actor,''),TRUE);
 PERFORM set_config('kyro.environment',COALESCE(previous_env,''),TRUE);
 RETURN TRUE;
END $$;
REVOKE ALL ON FUNCTION public.kyro_block_revoked_agent_run(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.kyro_block_revoked_agent_run(UUID) TO kyro_api;
