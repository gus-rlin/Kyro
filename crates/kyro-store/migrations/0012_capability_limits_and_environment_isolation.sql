CREATE FUNCTION public.kyro_valid_capability_limits(input_limits JSONB) RETURNS BOOLEAN
LANGUAGE plpgsql IMMUTABLE PARALLEL SAFE SET search_path = pg_catalog
AS $$
DECLARE
    item RECORD;
    minimum_value NUMERIC;
    maximum_value NUMERIC;
    actual_value NUMERIC;
BEGIN
    IF input_limits IS NULL
       OR pg_catalog.jsonb_typeof(input_limits) IS DISTINCT FROM 'object'
       OR pg_catalog.octet_length(input_limits::TEXT) > 1024 THEN
        RETURN FALSE;
    END IF;

    FOR item IN
        SELECT entry.key, entry.value
        FROM pg_catalog.jsonb_each(input_limits) AS entry
    LOOP
        CASE item.key
            WHEN 'max_job_attempts' THEN
                minimum_value := 1;
                maximum_value := 3;
            WHEN 'max_job_ttl_secs' THEN
                minimum_value := 10;
                maximum_value := 1800;
            WHEN 'max_model_input_bytes' THEN
                minimum_value := 1;
                maximum_value := 1048576;
            WHEN 'max_model_output_tokens' THEN
                minimum_value := 1;
                maximum_value := 1000000;
            WHEN 'max_changeset_operations' THEN
                minimum_value := 1;
                maximum_value := 128;
            ELSE
                RETURN FALSE;
        END CASE;

        IF pg_catalog.jsonb_typeof(item.value) IS DISTINCT FROM 'number' THEN
            RETURN FALSE;
        END IF;

        BEGIN
            actual_value := (item.value #>> '{}')::NUMERIC;
        EXCEPTION WHEN numeric_value_out_of_range THEN
            RETURN FALSE;
        END;

        IF actual_value <> pg_catalog.trunc(actual_value)
           OR actual_value < minimum_value
           OR actual_value > maximum_value THEN
            RETURN FALSE;
        END IF;
    END LOOP;

    RETURN TRUE;
END
$$;

REVOKE ALL ON FUNCTION public.kyro_valid_capability_limits(JSONB) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.kyro_valid_capability_limits(JSONB) TO kyro_api;

ALTER TABLE public.capability_grants
    ADD COLUMN limits JSONB NOT NULL DEFAULT '{}'::JSONB;
ALTER TABLE public.capability_grants
    ADD CONSTRAINT capability_grants_limits_valid
    CHECK (public.kyro_valid_capability_limits(limits));

-- Project visibility is environment-scoped, but a project may contain jobs
-- admitted in more than one environment. Do not let a project grant expose a
-- job from a different runtime environment. Queue claim and accounting
-- predicates retain their exact-job/environment checks.
DROP POLICY jobs_visible ON public.jobs;
CREATE POLICY jobs_visible ON public.jobs FOR SELECT TO kyro_api, kyro_worker
    USING (
        (environment = public.kyro_environment() AND public.kyro_project_visible(project_id))
        OR (
            current_user = 'kyro_worker'
            AND pg_catalog.current_setting('kyro.queue_claim', true) = 'on'
            AND environment = public.kyro_environment()
        )
        OR public.kyro_worker_accounting_job(id, project_id)
    );

DROP POLICY effects_visible ON public.effects;
CREATE POLICY effects_visible ON public.effects FOR SELECT TO kyro_api, kyro_worker
    USING (
        (
            public.kyro_project_visible(project_id)
            AND EXISTS (
                SELECT 1 FROM public.jobs j
                WHERE j.id = effects.job_id
                  AND j.project_id = effects.project_id
                  AND j.environment = public.kyro_environment()
            )
        )
        OR public.kyro_worker_can_access_effect(id, project_id)
    );

DROP POLICY reservations_visible ON public.budget_reservations;
CREATE POLICY reservations_visible ON public.budget_reservations FOR SELECT TO kyro_api, kyro_worker
    USING (
        (
            public.kyro_project_visible(project_id)
            AND EXISTS (
                SELECT 1 FROM public.jobs j
                WHERE j.id = budget_reservations.job_id
                  AND j.project_id = budget_reservations.project_id
                  AND j.environment = public.kyro_environment()
            )
        )
        OR public.kyro_worker_can_access_reservation(id, project_id, job_id)
    );

DROP POLICY usage_visible ON public.usage_ledger;
CREATE POLICY usage_visible ON public.usage_ledger FOR SELECT TO kyro_api, kyro_worker
    USING (
        (
            public.kyro_project_visible(project_id)
            AND EXISTS (
                SELECT 1 FROM public.jobs j
                WHERE j.id = usage_ledger.job_id
                  AND j.project_id = usage_ledger.project_id
                  AND j.environment = public.kyro_environment()
            )
        )
        OR public.kyro_worker_can_access_reservation(reservation_id, project_id, job_id)
    );

CREATE FUNCTION public.kyro_event_payload_environment_visible(
    target_project UUID,
    target_payload JSONB
) RETURNS BOOLEAN
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
DECLARE
    runtime_environment TEXT := public.kyro_environment();
    resolved_job UUID;
    linked_job UUID;
    referenced_id UUID;
BEGIN
    IF session_user NOT IN ('kyro_api', 'kyro_worker')
       OR target_project IS NULL
       OR target_payload IS NULL
       OR pg_catalog.jsonb_typeof(target_payload) IS DISTINCT FROM 'object'
       OR runtime_environment IS NULL
       OR runtime_environment NOT IN ('development', 'production')
       OR public.kyro_project_visible(target_project) IS NOT TRUE THEN
        RETURN FALSE;
    END IF;

    IF target_payload ? 'job_id' THEN
        BEGIN
            resolved_job := (target_payload->>'job_id')::UUID;
        EXCEPTION WHEN invalid_text_representation THEN
            RETURN FALSE;
        END;
        IF NOT EXISTS (
            SELECT 1 FROM public.jobs j
            WHERE j.id = resolved_job
              AND j.project_id = target_project
              AND j.environment = runtime_environment
        ) THEN
            RETURN FALSE;
        END IF;
    END IF;

    IF target_payload ? 'effect_id' THEN
        BEGIN
            referenced_id := (target_payload->>'effect_id')::UUID;
        EXCEPTION WHEN invalid_text_representation THEN
            RETURN FALSE;
        END;
        SELECT e.job_id INTO linked_job
        FROM public.effects e
        JOIN public.jobs j ON j.id = e.job_id AND j.project_id = e.project_id
        WHERE e.id = referenced_id
          AND e.project_id = target_project
          AND j.environment = runtime_environment;
        IF linked_job IS NULL OR (resolved_job IS NOT NULL AND resolved_job <> linked_job) THEN
            RETURN FALSE;
        END IF;
        resolved_job := linked_job;
    END IF;

    IF target_payload ? 'reservation_id' THEN
        BEGIN
            referenced_id := (target_payload->>'reservation_id')::UUID;
        EXCEPTION WHEN invalid_text_representation THEN
            RETURN FALSE;
        END;
        SELECT r.job_id INTO linked_job
        FROM public.budget_reservations r
        JOIN public.jobs j ON j.id = r.job_id AND j.project_id = r.project_id
        WHERE r.id = referenced_id
          AND r.project_id = target_project
          AND j.environment = runtime_environment;
        IF linked_job IS NULL OR (resolved_job IS NOT NULL AND resolved_job <> linked_job) THEN
            RETURN FALSE;
        END IF;
    END IF;

    RETURN TRUE;
END
$$;

REVOKE ALL ON FUNCTION public.kyro_event_payload_environment_visible(UUID, JSONB) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.kyro_event_payload_environment_visible(UUID, JSONB)
    TO kyro_api, kyro_worker;

DROP POLICY events_visible ON public.events;
CREATE POLICY events_visible ON public.events FOR SELECT TO kyro_api, kyro_worker
    USING (
        public.kyro_project_visible(project_id)
        AND public.kyro_event_payload_environment_visible(project_id, payload)
    );

DROP POLICY outbox_visible ON public.outbox_events;
CREATE POLICY outbox_visible ON public.outbox_events FOR SELECT TO kyro_api
    USING (
        public.kyro_project_visible(project_id)
        AND EXISTS (
            SELECT 1 FROM public.events e
            WHERE e.project_id = outbox_events.project_id
              AND e.sequence = outbox_events.event_sequence
              AND public.kyro_event_payload_environment_visible(e.project_id, e.payload)
        )
    );
