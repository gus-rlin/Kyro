-- P3 stores planning metadata only. API authority is checked again at every transition.
CREATE TABLE public.agent_runs (
 id UUID PRIMARY KEY,
 project_id UUID NOT NULL REFERENCES public.projects(id),
 actor_id UUID NOT NULL,
 environment TEXT NOT NULL CHECK(environment IN ('development','production')),
 idempotency_key TEXT NOT NULL CHECK(length(idempotency_key) BETWEEN 1 AND 200),
 fingerprint TEXT NOT NULL CHECK(fingerprint ~ '^[a-f0-9]{64}$'),
 version BIGINT NOT NULL CHECK(version>0),
 state JSONB NOT NULL CHECK(jsonb_typeof(state)='object' AND octet_length(state::TEXT)<=2097152),
 active BOOLEAN NOT NULL,
 updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
 UNIQUE(project_id,environment,idempotency_key),
 UNIQUE(id,project_id)
);
CREATE UNIQUE INDEX agent_one_active_plan ON public.agent_runs(project_id,environment) WHERE active;
ALTER TABLE public.agent_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.agent_runs FORCE ROW LEVEL SECURITY;
CREATE POLICY agent_read ON public.agent_runs FOR SELECT TO kyro_api,kyro_worker
 USING(environment=public.kyro_environment() AND public.kyro_actor_has_action(project_id,'read'));
CREATE POLICY agent_insert ON public.agent_runs FOR INSERT TO kyro_api
 WITH CHECK(environment=public.kyro_environment() AND actor_id=public.kyro_actor_id()
  AND public.kyro_actor_has_action(project_id,'read') AND public.kyro_actor_has_action(project_id,'execute'));
CREATE POLICY agent_update ON public.agent_runs FOR UPDATE TO kyro_api
 USING(environment=public.kyro_environment() AND actor_id=public.kyro_actor_id() AND public.kyro_actor_has_action(project_id,'execute'))
 WITH CHECK(environment=public.kyro_environment() AND actor_id=public.kyro_actor_id() AND public.kyro_actor_has_action(project_id,'execute'));
CREATE POLICY agent_owner ON public.agent_runs TO CURRENT_USER USING(TRUE) WITH CHECK(TRUE);
GRANT SELECT ON public.agent_runs TO kyro_api,kyro_worker;
GRANT INSERT,UPDATE ON public.agent_runs TO kyro_api;

CREATE TABLE public.agent_history (
 run_id UUID NOT NULL,
 project_id UUID NOT NULL,
 version BIGINT NOT NULL,
 state JSONB NOT NULL CHECK(jsonb_typeof(state)='object' AND octet_length(state::TEXT)<=2097152),
 created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(run_id,version),
 FOREIGN KEY(run_id,project_id) REFERENCES public.agent_runs(id,project_id)
);
ALTER TABLE public.agent_history ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.agent_history FORCE ROW LEVEL SECURITY;
CREATE POLICY agent_history_read ON public.agent_history FOR SELECT TO kyro_api,kyro_worker
 USING(EXISTS(SELECT 1 FROM public.agent_runs r WHERE r.id=run_id AND r.project_id=agent_history.project_id));
CREATE POLICY agent_history_insert ON public.agent_history FOR INSERT TO kyro_api
 WITH CHECK(EXISTS(SELECT 1 FROM public.agent_runs r WHERE r.id=run_id AND r.project_id=agent_history.project_id
  AND r.actor_id=public.kyro_actor_id() AND public.kyro_actor_has_action(r.project_id,'execute') AND r.version=agent_history.version));
CREATE POLICY agent_history_owner ON public.agent_history TO CURRENT_USER USING(TRUE) WITH CHECK(TRUE);
GRANT SELECT,INSERT ON public.agent_history TO kyro_api;
GRANT SELECT ON public.agent_history TO kyro_worker;

CREATE FUNCTION public.kyro_agent_history() RETURNS TRIGGER LANGUAGE plpgsql SET search_path=pg_catalog,public AS $$
BEGIN
 IF TG_OP='UPDATE' AND (NEW.id<>OLD.id OR NEW.project_id<>OLD.project_id OR NEW.actor_id<>OLD.actor_id
   OR NEW.environment<>OLD.environment OR NEW.fingerprint<>OLD.fingerprint OR NEW.idempotency_key<>OLD.idempotency_key OR NEW.version<>OLD.version+1) THEN
  RAISE EXCEPTION USING ERRCODE='23514', MESSAGE='agent identity immutable and version must advance';
 END IF;
 INSERT INTO public.agent_history(run_id,project_id,version,state) VALUES(NEW.id,NEW.project_id,NEW.version,NEW.state);
 RETURN NEW;
END $$;
CREATE TRIGGER agent_history AFTER INSERT OR UPDATE ON public.agent_runs FOR EACH ROW EXECUTE FUNCTION public.kyro_agent_history();
CREATE FUNCTION public.kyro_agent_history_immutable() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION USING ERRCODE='23514',MESSAGE='agent history is immutable'; END $$;
CREATE TRIGGER agent_history_immutable BEFORE UPDATE OR DELETE ON public.agent_history FOR EACH ROW EXECUTE FUNCTION public.kyro_agent_history_immutable();

-- Trusted API coordinator inventory, never granted to the worker or anonymous callers.
-- It returns metadata only; normal actor/grant fences are required to advance a run.
CREATE FUNCTION public.kyro_due_agent_runs() RETURNS TABLE(id UUID,project_id UUID,actor_id UUID)
 LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $$
BEGIN
 IF session_user<>'kyro_api' THEN RETURN; END IF;
 RETURN QUERY SELECT r.id,r.project_id,r.actor_id FROM public.agent_runs r
  WHERE r.active AND r.environment=public.kyro_environment() ORDER BY r.updated_at LIMIT 32;
END $$;
REVOKE ALL ON FUNCTION public.kyro_due_agent_runs() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.kyro_due_agent_runs() TO kyro_api;
