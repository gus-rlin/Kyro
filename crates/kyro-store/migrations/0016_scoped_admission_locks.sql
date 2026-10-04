-- Admission and reconciliation need a project row lock for serialization, but
-- SELECT FOR UPDATE would apply the project UPDATE policy and require write or
-- manage. Keep the lock behind an actor/action-scoped helper instead.
CREATE FUNCTION public.kyro_lock_project_for_actor(
    target_project UUID,
    target_actions TEXT[]
)
RETURNS TABLE (current_revision BIGINT, limits JSONB)
LANGUAGE plpgsql VOLATILE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
DECLARE
    runtime_actor UUID;
    runtime_environment TEXT;
    locked_revision BIGINT;
    locked_limits JSONB;
    requested_action TEXT;
    has_requested_action BOOLEAN := FALSE;
BEGIN
    IF session_user NOT IN ('kyro_api', 'kyro_worker')
       OR target_project IS NULL
       OR target_actions IS NULL
       OR pg_catalog.cardinality(target_actions) NOT BETWEEN 1 AND 6
       OR pg_catalog.array_position(target_actions, NULL) IS NOT NULL
       OR (target_actions <@ ARRAY['read', 'write', 'execute', 'model', 'manage', 'budget']::TEXT[]) IS NOT TRUE THEN
        RETURN;
    END IF;

    runtime_environment := public.kyro_environment();
    IF runtime_environment IS NULL OR runtime_environment NOT IN ('development', 'production') THEN
        RETURN;
    END IF;

    BEGIN
        runtime_actor := public.kyro_actor_id();
    EXCEPTION WHEN invalid_text_representation THEN
        RETURN;
    END;
    IF runtime_actor IS NULL
       OR public.kyro_project_visible(target_project) IS NOT TRUE
       OR public.has_project_org_member(target_project, runtime_actor) IS NOT TRUE THEN
        RETURN;
    END IF;

    -- Actions are alternatives. The Store still checks every cumulative
    -- requirement separately, while this gate prevents unrelated actors from
    -- taking project locks.
    FOREACH requested_action IN ARRAY target_actions LOOP
        IF public.kyro_actor_has_action(target_project, requested_action) IS TRUE THEN
            has_requested_action := TRUE;
            EXIT;
        END IF;
    END LOOP;
    IF NOT has_requested_action THEN
        RETURN;
    END IF;

    -- Preserve the project -> membership -> grant lock order used by writers.
    SELECT p.current_revision, p.limits
      INTO locked_revision, locked_limits
      FROM public.projects AS p
     WHERE p.id = target_project
     FOR UPDATE;
    IF NOT FOUND THEN
        RETURN;
    END IF;

    -- Recheck under stable shared locks. This closes revocation/removal races
    -- between the precheck and the project row lock.
    IF NOT EXISTS (
        SELECT 1
          FROM public.kyro_lock_actor_grants(runtime_actor, target_project, target_actions)
    ) THEN
        RETURN;
    END IF;

    current_revision := locked_revision;
    limits := locked_limits;
    RETURN NEXT;
END
$$;

REVOKE ALL ON FUNCTION public.kyro_lock_project_for_actor(UUID, TEXT[]) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.kyro_lock_project_for_actor(UUID, TEXT[])
    TO kyro_api, kyro_worker;

-- A reconciliation is a durable accounting command, not an execution grant.
-- It may be admitted with Budget or Manage, but only for an unknown effect
-- whose linked unknown job and held reservation share the project/environment.
DROP POLICY jobs_admit ON public.jobs;
CREATE POLICY jobs_admit ON public.jobs FOR INSERT TO kyro_api
    WITH CHECK (
        actor_id = public.kyro_actor_id()
        AND environment = public.kyro_environment()
        AND payload->>'kind' IS DISTINCT FROM 'reconcile_effect'
        AND public.kyro_actor_has_action(project_id, 'execute')
    );

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
               AND e.destination = 'synthetic-local'
               AND e.status = 'unknown'
               AND target_job.status = 'unknown'
               AND target_job.environment = jobs.environment
               AND r.status = 'held'
        )
    );

-- Job-backed idempotency records must reference the pending job admitted in
-- this transaction, with matching actor/project/environment and its specific
-- capability requirements. Non-job project commands retain Write/Manage.
DROP POLICY change_commands_insert ON public.change_commands;
CREATE POLICY change_commands_insert ON public.change_commands FOR INSERT TO kyro_api, kyro_worker
    WITH CHECK (
        (
            result ? 'job_id'
            AND pg_catalog.jsonb_typeof(result->'job_id') IS NOT DISTINCT FROM 'string'
            AND (result - 'job_id') = '{}'::JSONB
            AND EXISTS (
                SELECT 1
                  FROM public.jobs AS admitted_job
                 WHERE admitted_job.id::TEXT = change_commands.result->>'job_id'
                   AND admitted_job.project_id = change_commands.project_id
                   AND admitted_job.actor_id = public.kyro_actor_id()
                   AND admitted_job.environment = public.kyro_environment()
                   AND admitted_job.status = 'pending'
                   AND CASE admitted_job.payload->>'kind'
                       WHEN 'apply_changes' THEN
                           public.kyro_actor_has_action(change_commands.project_id, 'execute')
                           AND public.kyro_actor_has_action(change_commands.project_id, 'write')
                       WHEN 'model_call' THEN
                           public.kyro_actor_has_action(change_commands.project_id, 'execute')
                           AND public.kyro_actor_has_action(change_commands.project_id, 'model')
                       WHEN 'reconcile_effect' THEN
                           public.kyro_actor_has_action(change_commands.project_id, 'budget')
                           OR public.kyro_actor_has_action(change_commands.project_id, 'manage')
                       ELSE FALSE
                   END
            )
        )
        OR (
            NOT (result ? 'job_id')
            AND (
                public.kyro_actor_has_action(change_commands.project_id, 'write')
                OR public.kyro_actor_has_action(change_commands.project_id, 'manage')
            )
        )
    );

-- Settlement must lock the already-loaded model job even after the original
-- actor context/grants expire. Match the same exact worker-only accounting
-- boundary as SELECT, without granting API updates or queue-wide traversal.
CREATE POLICY jobs_update_accounting ON public.jobs FOR UPDATE TO kyro_worker
    USING (
        environment = public.kyro_environment()
        AND payload->>'kind' IN ('model_call', 'reconcile_effect')
        AND public.kyro_worker_accounting_job(id, project_id)
    )
    WITH CHECK (
        environment = public.kyro_environment()
        AND payload->>'kind' IN ('model_call', 'reconcile_effect')
        AND public.kyro_worker_accounting_job(id, project_id)
    );

-- Admission events obey the command authority, including Budget-only reconciliation.
CREATE OR REPLACE FUNCTION public.kyro_append_event(project_id UUID, kind TEXT, payload JSONB) RETURNS public.events
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
#variable_conflict use_variable
DECLARE
    event_actor UUID;
    event_job UUID;
    linked_job UUID;
    next_sequence BIGINT;
    event_row public.events;
    service_event BOOLEAN;
    action_name TEXT;
    allowed_payload_keys TEXT[] := ARRAY['job_id', 'effect_id', 'reservation_id', 'status', 'generation', 'attempts', 'units', 'error_code', 'revision'];
BEGIN
    IF kind IS NULL OR kind !~ '^[a-z][a-z0-9_.-]{0,79}$'
       OR payload IS NULL OR jsonb_typeof(payload) <> 'object'
       OR octet_length(payload::text) > 32768 THEN
        RAISE EXCEPTION 'invalid event' USING ERRCODE = '22023';
    END IF;

    service_event := kind LIKE 'job.%' OR kind LIKE 'effect.%' OR kind LIKE 'budget.%';
    IF session_user = 'kyro_api' THEN
        IF NOT public.kyro_project_visible(project_id) THEN
            RAISE EXCEPTION 'project not found' USING ERRCODE = 'P0002';
        END IF;
        event_actor := public.kyro_actor_id();
        IF kind = 'job.queued' THEN
            IF payload <> jsonb_build_object('job_id', payload->>'job_id', 'status', 'pending')
               OR NOT EXISTS (
                   SELECT 1 FROM public.jobs j
                   WHERE j.id::TEXT = payload->>'job_id' AND j.project_id = project_id
                     AND j.actor_id = event_actor AND j.environment = public.kyro_environment()
                     AND j.status = 'pending'
               ) THEN
                RAISE EXCEPTION 'queued event job mismatch' USING ERRCODE = '42501';
            END IF;
        END IF;
        action_name := CASE
            WHEN kind = 'job.queued' AND EXISTS (
                SELECT 1 FROM public.jobs j
                WHERE j.id::TEXT = payload->>'job_id' AND j.project_id = project_id
                  AND j.actor_id = event_actor AND j.environment = public.kyro_environment()
                  AND j.payload->>'kind' = 'reconcile_effect' AND j.status = 'pending'
            ) THEN 'budget'
            WHEN kind LIKE 'job.%' THEN 'execute'
            WHEN kind LIKE 'effect.%' THEN 'model'
            ELSE 'write'
        END;
        IF kind = 'job.cancel_requested' THEN
            IF NOT payload ? 'job_id' OR payload - ARRAY['job_id', 'status', 'generation'] <> '{}'::JSONB THEN
                RAISE EXCEPTION 'cancel event payload denied' USING ERRCODE = '42501';
            END IF;
            event_job := (payload->>'job_id')::UUID;
            IF payload ? 'status' AND payload->>'status' NOT IN ('pending', 'running') THEN
                RAISE EXCEPTION 'cancel event status denied' USING ERRCODE = '42501';
            END IF;
            IF payload ? 'generation' AND (jsonb_typeof(payload->'generation') <> 'number'
               OR payload->>'generation' !~ '^(0|[1-9][0-9]{0,18})$') THEN
                RAISE EXCEPTION 'cancel event generation denied' USING ERRCODE = '42501';
            END IF;
            IF NOT EXISTS (
                SELECT 1 FROM public.jobs j WHERE j.id = event_job AND j.project_id = project_id
                  AND j.environment = public.kyro_environment()
                  AND j.status IN ('pending', 'running') AND j.cancel_requested IS TRUE
                  AND (NOT payload ? 'status' OR j.status = payload->>'status')
                  AND (j.actor_id = event_actor OR public.kyro_actor_has_action(project_id, 'manage'))
            ) THEN
                RAISE EXCEPTION 'cancel event job mismatch' USING ERRCODE = '42501';
            END IF;
        END IF;

        PERFORM 1 FROM public.memberships m
        JOIN public.projects p ON p.organization_id = m.organization_id
        WHERE p.id = project_id AND m.actor_id = event_actor
        FOR SHARE OF m;
        PERFORM 1 FROM public.capability_grants g
        WHERE g.project_id = project_id AND g.actor_id = event_actor
          AND g.environment = public.kyro_environment()
          AND (g.expires_at IS NULL OR g.expires_at > clock_timestamp())
          AND g.revoked_at IS NULL
          AND (g.resources @> ARRAY['*']::TEXT[] OR g.resources @> ARRAY[project_id::TEXT]::TEXT[])
          AND (
              (kind = 'job.cancel_requested' AND (
                  g.actions @> ARRAY['execute']::TEXT[] OR g.actions @> ARRAY['manage']::TEXT[]
              ))
              OR ((kind LIKE 'budget.%' OR action_name = 'budget') AND (
                  g.actions @> ARRAY['budget']::TEXT[] OR g.actions @> ARRAY['manage']::TEXT[]
              ))
              OR (kind <> 'job.cancel_requested' AND kind NOT LIKE 'budget.%' AND action_name <> 'budget' AND action_name = ANY(g.actions))
          )
        FOR SHARE OF g;
        IF NOT FOUND THEN
            IF NOT public.kyro_project_visible(project_id) THEN
                RAISE EXCEPTION 'project not found' USING ERRCODE = 'P0002';
            END IF;
            RAISE EXCEPTION 'event action denied' USING ERRCODE = '42501';
        END IF;
    ELSIF session_user = 'kyro_worker' THEN
        IF public.kyro_environment() IS NULL
           OR public.kyro_environment() NOT IN ('development', 'production') THEN
            RAISE EXCEPTION 'worker environment required' USING ERRCODE = '42501';
        END IF;

        IF service_event THEN
            IF payload - allowed_payload_keys <> '{}'::JSONB OR jsonb_object_length(payload) > 9 THEN
                RAISE EXCEPTION 'worker event payload denied' USING ERRCODE = '42501';
            END IF;
            IF kind LIKE 'job.%' AND NOT payload ? 'job_id' THEN
                RAISE EXCEPTION 'job event requires job_id' USING ERRCODE = '42501';
            ELSIF kind LIKE 'effect.%' AND NOT payload ? 'effect_id' THEN
                RAISE EXCEPTION 'effect event requires effect_id' USING ERRCODE = '42501';
            ELSIF kind LIKE 'budget.%' AND NOT (payload ? 'reservation_id' OR payload ? 'effect_id') THEN
                RAISE EXCEPTION 'budget event requires effect or reservation id' USING ERRCODE = '42501';
            END IF;
            IF payload ? 'error_code' AND NOT (
                payload->>'error_code' IN (
                    'unsupported_payload', 'execution_failed', 'retryable_internal', 'gateway_unavailable',
                    'permission_revoked', 'source_stale', 'deadline_expired', 'attempts_exceeded', 'cancelled', 'lease_lost'
                ) OR (kind LIKE 'effect.%' AND payload->>'error_code' IN (
                    'transport_uncertain', 'provider_status_uncertain', 'invalid_response',
                    'response_too_large', 'invalid_usage', 'persistence_uncertain'
                ))
            ) THEN
                RAISE EXCEPTION 'worker event error code denied' USING ERRCODE = '42501';
            END IF;
            IF payload ? 'status' AND payload->>'status' NOT IN (
                'pending', 'running', 'succeeded', 'failed', 'cancelled', 'unknown', 'stale',
                'prepared', 'sending', 'held', 'settled', 'released'
            ) THEN
                RAISE EXCEPTION 'worker event status denied' USING ERRCODE = '42501';
            END IF;
            IF payload ? 'generation' AND (jsonb_typeof(payload->'generation') <> 'number'
               OR payload->>'generation' !~ '^(0|[1-9][0-9]{0,18})$'
               OR (payload->>'generation')::NUMERIC > 9223372036854775807) THEN
                RAISE EXCEPTION 'worker event generation denied' USING ERRCODE = '42501';
            END IF;
            IF payload ? 'revision' AND (jsonb_typeof(payload->'revision') <> 'number'
               OR payload->>'revision' !~ '^(0|[1-9][0-9]{0,18})$'
               OR (payload->>'revision')::NUMERIC > 9223372036854775807) THEN
                RAISE EXCEPTION 'worker event revision denied' USING ERRCODE = '42501';
            END IF;
            IF payload ? 'attempts' AND (jsonb_typeof(payload->'attempts') <> 'number'
               OR payload->>'attempts' !~ '^(0|[1-9][0-9]{0,8})$'
               OR (payload->>'attempts')::INTEGER > 3) THEN
                RAISE EXCEPTION 'worker event attempts denied' USING ERRCODE = '42501';
            END IF;
            IF payload ? 'units' AND (jsonb_typeof(payload->'units') <> 'number'
               OR payload->>'units' !~ '^(0|[1-9][0-9]{0,18})$'
               OR (payload->>'units')::NUMERIC > 9223372036854775807) THEN
                RAISE EXCEPTION 'worker event units denied' USING ERRCODE = '42501';
            END IF;

            IF payload ? 'job_id' THEN
                event_job := (payload->>'job_id')::UUID;
                IF NOT EXISTS (
                    SELECT 1 FROM public.jobs j WHERE j.id = event_job AND j.project_id = project_id
                      AND j.environment = public.kyro_environment()
                ) THEN
                    RAISE EXCEPTION 'worker event job mismatch' USING ERRCODE = '42501';
                END IF;
            END IF;
            IF payload ? 'effect_id' THEN
                SELECT e.job_id INTO linked_job FROM public.effects e
                JOIN public.jobs j ON j.id = e.job_id AND j.project_id = e.project_id
                WHERE e.id = (payload->>'effect_id')::UUID AND e.project_id = project_id
                  AND j.environment = public.kyro_environment();
                IF linked_job IS NULL OR (event_job IS NOT NULL AND event_job <> linked_job) THEN
                    RAISE EXCEPTION 'worker event effect mismatch' USING ERRCODE = '42501';
                END IF;
                event_job := linked_job;
            END IF;
            IF payload ? 'reservation_id' THEN
                SELECT r.job_id INTO linked_job FROM public.budget_reservations r
                JOIN public.jobs j ON j.id = r.job_id AND j.project_id = r.project_id
                WHERE r.id = (payload->>'reservation_id')::UUID AND r.project_id = project_id
                  AND j.environment = public.kyro_environment();
                IF linked_job IS NULL OR (event_job IS NOT NULL AND event_job <> linked_job) THEN
                    RAISE EXCEPTION 'worker event reservation mismatch' USING ERRCODE = '42501';
                END IF;
                event_job := linked_job;
            END IF;
            IF event_job IS NULL THEN
                RAISE EXCEPTION 'worker event requires a linked job' USING ERRCODE = '42501';
            END IF;

            IF pg_catalog.current_setting('kyro.queue_claim', true) = 'on' THEN
                IF NOT public.kyro_worker_can_access_job(event_job, project_id) THEN
                    RAISE EXCEPTION 'worker queue event scope denied' USING ERRCODE = '42501';
                END IF;
            ELSIF NULLIF(pg_catalog.current_setting('kyro.accounting_job_id', true), '') IS NOT NULL THEN
                IF NOT public.kyro_worker_accounting_job(event_job, project_id) THEN
                    RAISE EXCEPTION 'worker accounting event scope denied' USING ERRCODE = '42501';
                END IF;
            ELSE
                RAISE EXCEPTION 'worker service event context required' USING ERRCODE = '42501';
            END IF;
            event_actor := NULL;
        ELSE
            event_actor := public.kyro_actor_id();
            IF event_actor IS NULL OR NOT (
                public.kyro_actor_has_action(project_id, 'write')
                OR public.kyro_actor_has_action(project_id, 'manage')
            ) THEN
                RAISE EXCEPTION 'worker business event action denied' USING ERRCODE = '42501';
            END IF;
            IF payload ? 'job_id' AND NOT EXISTS (
                SELECT 1 FROM public.jobs j WHERE j.id = (payload->>'job_id')::UUID
                  AND j.project_id = project_id AND j.actor_id = event_actor
                  AND j.environment = public.kyro_environment()
            ) THEN
                RAISE EXCEPTION 'worker business event job mismatch' USING ERRCODE = '42501';
            END IF;
        END IF;
    ELSE
        RAISE EXCEPTION 'runtime role denied' USING ERRCODE = '42501';
    END IF;

    UPDATE public.projects p
       SET event_sequence = p.event_sequence + 1, updated_at = clock_timestamp()
     WHERE p.id = project_id
     RETURNING p.event_sequence INTO next_sequence;
    IF next_sequence IS NULL THEN
        RAISE EXCEPTION 'project not found' USING ERRCODE = 'P0002';
    END IF;

    INSERT INTO public.events (project_id, sequence, type, payload, actor_id)
    VALUES (project_id, next_sequence, kind, payload, event_actor)
    RETURNING * INTO event_row;

    INSERT INTO public.outbox_events (project_id, event_sequence, topic, payload)
    VALUES (
        project_id,
        next_sequence,
        'project.event',
        jsonb_build_object('project_id', project_id, 'sequence', next_sequence, 'type', kind)
    );
    RETURN event_row;
END
$$;
