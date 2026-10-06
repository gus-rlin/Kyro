-- Polling metadata is distinct from versioned logical progress and immutable history.
ALTER TABLE public.agent_runs ADD COLUMN last_polled_at TIMESTAMPTZ NOT NULL DEFAULT '-infinity';
CREATE INDEX agent_poll_order ON public.agent_runs(environment,last_polled_at,id) WHERE active;
DROP TRIGGER agent_history ON public.agent_runs;
CREATE TRIGGER agent_history AFTER INSERT OR UPDATE OF id,project_id,actor_id,environment,idempotency_key,fingerprint,version,state,active
 ON public.agent_runs FOR EACH ROW EXECUTE FUNCTION public.kyro_agent_history();

CREATE OR REPLACE FUNCTION public.kyro_due_agent_runs() RETURNS TABLE(id UUID,project_id UUID,actor_id UUID)
 LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $$
BEGIN
 IF session_user<>'kyro_api' THEN RETURN; END IF;
 RETURN QUERY WITH selected AS (
  SELECT r.id FROM public.agent_runs r WHERE r.active AND r.environment=public.kyro_environment()
   ORDER BY r.last_polled_at,r.id FOR UPDATE SKIP LOCKED LIMIT 32
 ), rotated AS (
  UPDATE public.agent_runs r SET last_polled_at=clock_timestamp() FROM selected s WHERE r.id=s.id
   RETURNING r.id,r.project_id,r.actor_id
 ) SELECT rotated.id,rotated.project_id,rotated.actor_id FROM rotated;
END $$;
REVOKE ALL ON FUNCTION public.kyro_due_agent_runs() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.kyro_due_agent_runs() TO kyro_api;
