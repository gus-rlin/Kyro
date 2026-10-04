-- Registry destination IDs are deployment configuration, not provider kinds.
-- Preserve the actor/project/environment/held-reservation boundary from 0016.
DROP POLICY jobs_admit_reconciliation ON public.jobs;
CREATE POLICY jobs_admit_reconciliation ON public.jobs FOR INSERT TO kyro_api
    WITH CHECK (
        actor_id = public.kyro_actor_id()
        AND environment = public.kyro_environment()
        AND pg_catalog.jsonb_typeof(payload) IS NOT DISTINCT FROM 'object'
        AND payload ?& ARRAY['kind', 'effect_id', 'request']
        AND (payload - ARRAY['kind', 'effect_id', 'request']) = '{}'::JSONB
        AND payload->>'kind' = 'reconcile_effect'
        AND pg_catalog.jsonb_typeof(payload->'effect_id') IS NOT DISTINCT FROM 'string'
        AND pg_catalog.jsonb_typeof(payload->'request') IS NOT DISTINCT FROM 'object'
        AND (
            public.kyro_actor_has_action(project_id, 'budget')
            OR public.kyro_actor_has_action(project_id, 'manage')
        )
        AND EXISTS (
            SELECT 1
              FROM public.effects AS e
              JOIN public.jobs AS target_job
                ON target_job.id = e.job_id
               AND target_job.project_id = e.project_id
              JOIN public.budget_reservations AS r
                ON r.id = e.reservation_id
               AND r.effect_id = e.id
               AND r.job_id = e.job_id
               AND r.project_id = e.project_id
             WHERE e.id::TEXT = jobs.payload->>'effect_id'
               AND e.project_id = jobs.project_id
               AND e.intent->'registration'->>'provider_kind' = 'synthetic'
               AND e.status = 'unknown'
               AND target_job.status = 'unknown'
               AND target_job.environment = jobs.environment
               AND r.status = 'held'
        )
    );
