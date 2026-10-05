CREATE FUNCTION public.app_schedule_identity(schedule_id uuid)
RETURNS TABLE(principal_id uuid, session_id uuid, token_hash bytea, expires_at timestamptz)
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $f$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM public.app_memberships m
   WHERE m.tenant_id=public.kyro_app_tenant_id() AND m.application_id=public.kyro_app_application_id()
     AND m.principal_id=public.kyro_app_actor_id() AND m.role='jobs.worker' AND m.status='active')
 THEN RAISE EXCEPTION USING ERRCODE='42501', MESSAGE='worker required'; END IF;
 RETURN QUERY SELECT s.principal_id,s.id,s.token_hash,s.expires_at
 FROM public.app_job_schedules j JOIN public.app_sessions s
   ON s.tenant_id=j.tenant_id AND s.application_id=j.application_id
     AND s.id=j.session_id AND s.principal_id=j.principal_id
 WHERE j.tenant_id=public.kyro_app_tenant_id() AND j.application_id=public.kyro_app_application_id()
   AND j.id=schedule_id AND j.enabled AND j.next_at<=clock_timestamp()
   AND s.revoked_at IS NULL AND s.expires_at>clock_timestamp();
END $f$;
REVOKE ALL ON FUNCTION public.app_schedule_identity(uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.app_schedule_identity(uuid) TO kyro_app;
