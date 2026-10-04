-- The API can request cancellation but cannot finalize jobs or release effects.
CREATE FUNCTION public.kyro_guard_job_cancel_requested() RETURNS TRIGGER
LANGUAGE plpgsql SET search_path = pg_catalog, public
AS $$
BEGIN
    IF OLD.cancel_requested AND NOT NEW.cancel_requested THEN
        RAISE EXCEPTION 'job cancellation cannot be cleared' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER jobs_cancel_requested_monotonic
    BEFORE UPDATE OF cancel_requested ON public.jobs
    FOR EACH ROW EXECUTE FUNCTION public.kyro_guard_job_cancel_requested();
REVOKE EXECUTE ON FUNCTION public.kyro_guard_job_cancel_requested() FROM PUBLIC;

CREATE POLICY jobs_cancel_by_actor ON public.jobs FOR UPDATE TO kyro_api
    USING (
        environment = public.kyro_environment()
        AND status IN ('pending', 'running')
        AND public.kyro_project_visible(project_id)
        AND (
            (actor_id = public.kyro_actor_id() AND public.kyro_actor_has_action(project_id, 'execute'))
            OR public.kyro_actor_has_action(project_id, 'manage')
        )
    )
    WITH CHECK (
        environment = public.kyro_environment()
        AND status IN ('pending', 'running')
        AND cancel_requested IS TRUE
        AND public.kyro_project_visible(project_id)
        AND (
            (actor_id = public.kyro_actor_id() AND public.kyro_actor_has_action(project_id, 'execute'))
            OR public.kyro_actor_has_action(project_id, 'manage')
        )
    );
GRANT UPDATE (cancel_requested) ON public.jobs TO kyro_api;

-- Cancellation and its event share one caller transaction. Execute permits
-- cancellation of the caller's job; manage permits an owner/admin to cancel
-- another actor's job in a project they can currently see.
CREATE OR REPLACE FUNCTION public.kyro_append_event(project_id UUID, kind TEXT, payload JSONB) RETURNS public.events
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
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
        action_name := CASE
            WHEN kind LIKE 'job.%' THEN 'execute'
            WHEN kind LIKE 'effect.%' THEN 'model'
            ELSE 'write'
        END;
        IF kind = 'job.cancel_requested' THEN
            IF NOT payload ? 'job_id' OR payload - ARRAY['job_id', 'status', 'generation'] <> '{}'::JSONB THEN
                RAISE EXCEPTION 'cancel event payload denied' USING ERRCODE = '42501';
            END IF;
            event_job := (payload->>'job_id')::UUID;
            IF payload ? 'status' AND payload->>'status' <> 'pending' THEN
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
              OR (kind LIKE 'budget.%' AND (
                  g.actions @> ARRAY['budget']::TEXT[] OR g.actions @> ARRAY['manage']::TEXT[]
              ))
              OR (kind <> 'job.cancel_requested' AND kind NOT LIKE 'budget.%' AND action_name = ANY(g.actions))
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
            IF payload ? 'error_code' AND payload->>'error_code' NOT IN (
                'unsupported_payload', 'execution_failed', 'retryable_internal', 'gateway_unavailable',
                'permission_revoked', 'source_stale', 'deadline_expired', 'attempts_exceeded', 'cancelled', 'lease_lost'
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
