-- Queue traversal and post-send accounting are separate service capabilities.
-- Neither context grants access to project documents or changes actor grants.

CREATE FUNCTION public.kyro_worker_accounting_job(target_job UUID, target_project UUID) RETURNS BOOLEAN
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT session_user = 'kyro_worker'
       AND NULLIF(pg_catalog.current_setting('kyro.accounting_job_id', true), '')::UUID = target_job
       AND public.kyro_environment() IN ('development', 'production')
       AND EXISTS (
           SELECT 1 FROM public.jobs j
           WHERE j.id = target_job AND j.project_id = target_project
             AND j.environment = public.kyro_environment()
       )
$$;

CREATE OR REPLACE FUNCTION public.kyro_project_visible(target_project UUID) RETURNS BOOLEAN
LANGUAGE SQL VOLATILE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT public.kyro_actor_id() IS NOT NULL
       AND public.kyro_environment() IN ('development', 'production')
       AND EXISTS (
           SELECT 1
           FROM public.projects p
           JOIN public.memberships m ON m.organization_id = p.organization_id
           JOIN public.capability_grants g ON g.project_id = p.id AND g.actor_id = m.actor_id
           WHERE p.id = target_project
             AND m.actor_id = public.kyro_actor_id()
             AND g.environment = public.kyro_environment()
             AND (g.expires_at IS NULL OR g.expires_at > clock_timestamp())
             AND g.revoked_at IS NULL
             AND (g.resources @> ARRAY['*']::TEXT[] OR g.resources @> ARRAY[target_project::TEXT]::TEXT[])
       )
$$;

CREATE OR REPLACE FUNCTION public.kyro_actor_has_action(target_project UUID, target_action TEXT) RETURNS BOOLEAN
LANGUAGE SQL VOLATILE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT public.kyro_actor_id() IS NOT NULL
       AND public.kyro_environment() IN ('development', 'production')
       AND EXISTS (
           SELECT 1
           FROM public.projects p
           JOIN public.memberships m ON m.organization_id = p.organization_id
           JOIN public.capability_grants g ON g.project_id = p.id AND g.actor_id = m.actor_id
           WHERE p.id = target_project
             AND m.actor_id = public.kyro_actor_id()
             AND g.environment = public.kyro_environment()
             AND target_action = ANY(g.actions)
             AND (g.expires_at IS NULL OR g.expires_at > clock_timestamp())
             AND g.revoked_at IS NULL
             AND (g.resources @> ARRAY['*']::TEXT[] OR g.resources @> ARRAY[target_project::TEXT]::TEXT[])
       )
$$;

-- Global cleanup batches should not scan tenant-local or revoked rows.
CREATE INDEX sessions_expiry_cleanup_idx ON public.sessions (expires_at, id) WHERE revoked_at IS NULL;
CREATE INDEX sessions_revoked_cleanup_idx ON public.sessions (created_at, id) WHERE revoked_at IS NOT NULL;

-- Budget configuration has its own CAS epoch. Settlement/accounting changes
-- never advance it; every change to limit/currency/scale advances it once.
ALTER TABLE public.project_budgets
    ADD COLUMN configuration_version BIGINT NOT NULL DEFAULT 0 CHECK (configuration_version >= 0);

CREATE FUNCTION public.kyro_guard_budget_configuration_version() RETURNS TRIGGER
LANGUAGE plpgsql SET search_path = pg_catalog, public
AS $$
DECLARE
    configuration_changed BOOLEAN;
BEGIN
    configuration_changed := NEW.limit_units IS DISTINCT FROM OLD.limit_units
        OR NEW.currency IS DISTINCT FROM OLD.currency
        OR NEW.unit_scale IS DISTINCT FROM OLD.unit_scale;
    IF configuration_changed THEN
        IF NEW.configuration_version <> OLD.configuration_version + 1 THEN
            RAISE EXCEPTION 'budget configuration version must advance exactly once' USING ERRCODE = '23514';
        END IF;
    ELSIF NEW.configuration_version <> OLD.configuration_version THEN
        RAISE EXCEPTION 'budget configuration version changes only with budget configuration' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER project_budgets_configuration_version_guard
    BEFORE UPDATE ON public.project_budgets
    FOR EACH ROW EXECUTE FUNCTION public.kyro_guard_budget_configuration_version();
GRANT UPDATE (configuration_version) ON public.project_budgets TO kyro_api;

CREATE OR REPLACE FUNCTION public.kyro_worker_can_access_effect(target_effect UUID, target_project UUID) RETURNS BOOLEAN
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT session_user = 'kyro_worker'
       AND public.kyro_environment() IN ('development', 'production')
       AND EXISTS (
           SELECT 1 FROM public.effects e
           JOIN public.jobs j ON j.id = e.job_id AND j.project_id = e.project_id
           WHERE e.id = target_effect AND e.project_id = target_project
             AND e.job_id = NULLIF(pg_catalog.current_setting('kyro.accounting_job_id', true), '')::UUID
             AND j.environment = public.kyro_environment()
       )
$$;

CREATE OR REPLACE FUNCTION public.kyro_worker_can_account_project(target_project UUID) RETURNS BOOLEAN
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT session_user = 'kyro_worker'
       AND public.kyro_environment() IN ('development', 'production')
       AND EXISTS (
           SELECT 1 FROM public.jobs j
           JOIN public.effects e ON e.job_id = j.id AND e.project_id = j.project_id
           WHERE j.id = NULLIF(pg_catalog.current_setting('kyro.accounting_job_id', true), '')::UUID
             AND j.project_id = target_project AND j.environment = public.kyro_environment()
       )
$$;

CREATE OR REPLACE FUNCTION public.kyro_worker_can_access_reservation(target_reservation UUID, target_project UUID, target_job UUID) RETURNS BOOLEAN
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT session_user = 'kyro_worker'
       AND NULLIF(pg_catalog.current_setting('kyro.accounting_job_id', true), '')::UUID = target_job
       AND public.kyro_environment() IN ('development', 'production')
       AND EXISTS (
           SELECT 1 FROM public.budget_reservations r
           JOIN public.effects e ON e.id = r.effect_id AND e.job_id = r.job_id AND e.project_id = r.project_id
           JOIN public.jobs j ON j.id = r.job_id AND j.project_id = r.project_id
           WHERE r.id = target_reservation AND r.project_id = target_project
             AND r.job_id = target_job AND j.environment = public.kyro_environment()
       )
$$;

-- The queue claim context is for traversal only. Settlement is scoped to the
-- exact job loaded by the worker, even after its lease or actor grant expires.
DROP POLICY reservations_visible ON public.budget_reservations;
CREATE POLICY reservations_visible ON public.budget_reservations FOR SELECT TO kyro_api, kyro_worker
    USING (
        public.kyro_project_visible(project_id)
        OR public.kyro_worker_can_access_reservation(id, project_id, job_id)
    );
DROP POLICY reservations_worker_account ON public.budget_reservations;
CREATE POLICY reservations_worker_account ON public.budget_reservations FOR UPDATE TO kyro_worker
    USING (public.kyro_worker_can_access_reservation(id, project_id, job_id))
    WITH CHECK (public.kyro_worker_can_access_reservation(id, project_id, job_id));

DROP POLICY usage_visible ON public.usage_ledger;
CREATE POLICY usage_visible ON public.usage_ledger FOR SELECT TO kyro_api, kyro_worker
    USING (
        public.kyro_project_visible(project_id)
        OR public.kyro_worker_can_access_reservation(reservation_id, project_id, job_id)
    );

-- An organization always retains at least one owner. The organization row is
-- the serialization point for concurrent owner removals/demotions.
CREATE FUNCTION public.kyro_guard_last_org_owner() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
DECLARE
    target_org UUID;
    target_actor UUID;
    locked_org UUID;
BEGIN
    IF TG_OP = 'DELETE' THEN
        target_org := OLD.organization_id;
        target_actor := OLD.actor_id;
        IF OLD.role <> 'owner' THEN
            RETURN OLD;
        END IF;
    ELSE
        IF OLD.role <> 'owner' OR NEW.role = 'owner' THEN
            RETURN NEW;
        END IF;
        target_org := OLD.organization_id;
        target_actor := OLD.actor_id;
    END IF;

    SELECT o.id INTO locked_org
    FROM public.organizations o
    WHERE o.id = target_org
    FOR UPDATE;

    IF locked_org IS NULL THEN
        RAISE EXCEPTION 'organization owner invariant failed' USING ERRCODE = '23514';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM public.memberships m
        WHERE m.organization_id = target_org AND m.role = 'owner' AND m.actor_id <> target_actor
    ) THEN
        RAISE EXCEPTION 'organization must retain an owner' USING ERRCODE = '23514';
    END IF;

    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER memberships_guard_last_owner_delete
    BEFORE DELETE ON public.memberships
    FOR EACH ROW EXECUTE FUNCTION public.kyro_guard_last_org_owner();
CREATE TRIGGER memberships_guard_last_owner_demotion
    BEFORE UPDATE OF role ON public.memberships
    FOR EACH ROW EXECUTE FUNCTION public.kyro_guard_last_org_owner();

-- A removed organization member loses grants in every environment atomically.
CREATE FUNCTION public.kyro_revoke_removed_member_grants() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
BEGIN
    UPDATE public.capability_grants g
       SET revoked_at = clock_timestamp()
      FROM public.projects p
     WHERE p.id = g.project_id
       AND p.organization_id = OLD.organization_id
       AND g.actor_id = OLD.actor_id
       AND g.revoked_at IS NULL;
    RETURN OLD;
END
$$;

CREATE TRIGGER memberships_revoke_removed_grants
    AFTER DELETE ON public.memberships
    FOR EACH ROW EXECUTE FUNCTION public.kyro_revoke_removed_member_grants();

-- Keep identity and creator columns immutable to the runtime API.
REVOKE UPDATE ON public.memberships, public.organizations FROM kyro_api;
GRANT UPDATE (role) ON public.memberships TO kyro_api;
GRANT UPDATE (name) ON public.organizations TO kyro_api;

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
              (kind LIKE 'budget.%' AND (g.actions @> ARRAY['budget']::TEXT[] OR g.actions @> ARRAY['manage']::TEXT[]))
              OR (kind NOT LIKE 'budget.%' AND action_name = ANY(g.actions))
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

GRANT EXECUTE ON FUNCTION public.kyro_worker_accounting_job(UUID, UUID) TO kyro_worker;
REVOKE EXECUTE ON FUNCTION public.kyro_worker_accounting_job(UUID, UUID),
    public.kyro_guard_last_org_owner(), public.kyro_revoke_removed_member_grants(),
    public.kyro_guard_budget_configuration_version() FROM PUBLIC;
