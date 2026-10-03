-- Post-send accounting may inspect exactly its loaded job after authorization
-- or lease expiry. It still cannot enumerate jobs or read project documents.
DROP POLICY jobs_visible ON public.jobs;
CREATE POLICY jobs_visible ON public.jobs FOR SELECT TO kyro_api, kyro_worker
    USING (
        public.kyro_project_visible(project_id)
        OR (
            current_user = 'kyro_worker'
            AND pg_catalog.current_setting('kyro.queue_claim', true) = 'on'
            AND environment = public.kyro_environment()
        )
        OR public.kyro_worker_accounting_job(id, project_id)
    );

-- The predicate always returns false for kyro_api, but the shared SELECT
-- policy must be executable while PostgreSQL evaluates the API's RLS branch.
GRANT EXECUTE ON FUNCTION public.kyro_worker_accounting_job(UUID, UUID) TO kyro_api;
