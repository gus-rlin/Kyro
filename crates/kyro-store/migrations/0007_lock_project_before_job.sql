-- Queue workers acquire locks in project -> job -> effect order. A candidate
-- lookup is deliberately non-locking; this helper serializes by project first
-- and returns only the small metadata needed for admission/revision checks.
CREATE FUNCTION public.kyro_lock_project_for_job(
    target_job UUID,
    skip_locked BOOLEAN DEFAULT FALSE
) RETURNS TABLE(project_id UUID, current_revision BIGINT)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
DECLARE
    candidate_project UUID;
    candidate_environment TEXT;
BEGIN
    IF session_user <> 'kyro_worker' THEN
        RAISE EXCEPTION 'worker role required' USING ERRCODE = '42501';
    END IF;
    IF public.kyro_environment() IS NULL
       OR public.kyro_environment() NOT IN ('development', 'production') THEN
        RAISE EXCEPTION 'worker environment required' USING ERRCODE = '42501';
    END IF;

    SELECT j.project_id, j.environment
      INTO candidate_project, candidate_environment
      FROM public.jobs j
     WHERE j.id = target_job;
    IF candidate_project IS NULL THEN
        RETURN;
    END IF;
    IF candidate_environment IS DISTINCT FROM public.kyro_environment()
       OR NOT (
           pg_catalog.current_setting('kyro.queue_claim', true) = 'on'
           OR NULLIF(pg_catalog.current_setting('kyro.accounting_job_id', true), '')::UUID = target_job
       ) THEN
        RAISE EXCEPTION 'worker job context denied' USING ERRCODE = '42501';
    END IF;

    IF skip_locked THEN
        SELECT p.id, p.current_revision
          INTO project_id, current_revision
          FROM public.projects p
         WHERE p.id = candidate_project
         FOR UPDATE SKIP LOCKED;
    ELSE
        SELECT p.id, p.current_revision
          INTO project_id, current_revision
          FROM public.projects p
         WHERE p.id = candidate_project
         FOR UPDATE;
    END IF;
    IF project_id IS NULL THEN
        RETURN;
    END IF;
    RETURN NEXT;
END
$$;
REVOKE EXECUTE ON FUNCTION public.kyro_lock_project_for_job(UUID, BOOLEAN) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.kyro_lock_project_for_job(UUID, BOOLEAN) TO kyro_worker;
