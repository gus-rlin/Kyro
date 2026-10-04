-- P1 PostgreSQL foundation. Runtime roles are deliberately not owners and do
-- not bypass RLS. The migration connection must be an administrative role.
DO $roles$
BEGIN
    IF current_user IN ('kyro_api', 'kyro_worker') THEN
        RAISE EXCEPTION 'migrations require an administrative role';
    END IF;

    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'kyro_api') THEN
        CREATE ROLE kyro_api LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'kyro_worker') THEN
        CREATE ROLE kyro_worker LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
    END IF;

    ALTER ROLE kyro_api WITH LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
    ALTER ROLE kyro_worker WITH LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
END
$roles$;

REVOKE CREATE ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO kyro_api, kyro_worker;

CREATE TABLE public.actors (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    issuer TEXT NOT NULL CHECK (length(issuer) BETWEEN 1 AND 2048),
    subject TEXT NOT NULL CHECK (length(subject) BETWEEN 1 AND 1024),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (issuer, subject)
);

CREATE TABLE public.organizations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    created_by UUID NOT NULL REFERENCES public.actors(id) ON DELETE RESTRICT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE public.memberships (
    organization_id UUID NOT NULL REFERENCES public.organizations(id) ON DELETE CASCADE,
    actor_id UUID NOT NULL REFERENCES public.actors(id) ON DELETE RESTRICT,
    role TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'member')),
    created_by UUID NOT NULL REFERENCES public.actors(id) ON DELETE RESTRICT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (organization_id, actor_id)
);
CREATE INDEX memberships_actor_idx ON public.memberships (actor_id, organization_id);

CREATE TABLE public.sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    token_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    actor_id UUID NOT NULL REFERENCES public.actors(id) ON DELETE CASCADE,
    csrf_hash BYTEA NOT NULL CHECK (octet_length(csrf_hash) = 32),
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (expires_at > created_at)
);
CREATE INDEX sessions_actor_expiry_idx ON public.sessions (actor_id, expires_at) WHERE revoked_at IS NULL;

CREATE TABLE public.login_flows (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    issuer TEXT NOT NULL CHECK (length(issuer) BETWEEN 1 AND 2048),
    state_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(state_hash) = 32),
    nonce_hash BYTEA NOT NULL CHECK (octet_length(nonce_hash) = 32),
    browser_binding_hash BYTEA NOT NULL CHECK (octet_length(browser_binding_hash) = 32),
    -- Required briefly for the OIDC code exchange. Never return or log this value.
    pkce_verifier TEXT CHECK (pkce_verifier IS NULL OR length(pkce_verifier) BETWEEN 43 AND 128),
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (expires_at > created_at),
    CHECK ((consumed_at IS NULL) = (pkce_verifier IS NOT NULL))
);
CREATE INDEX login_flows_binding_expiry_idx ON public.login_flows (browser_binding_hash, expires_at);

CREATE TABLE public.projects (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id UUID NOT NULL REFERENCES public.organizations(id) ON DELETE RESTRICT,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    current_revision BIGINT NOT NULL DEFAULT 0 CHECK (current_revision >= 0),
    event_sequence BIGINT NOT NULL DEFAULT 0 CHECK (event_sequence >= 0),
    data_policy JSONB NOT NULL DEFAULT '{}'::jsonb,
    limits JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_by UUID NOT NULL REFERENCES public.actors(id) ON DELETE RESTRICT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (octet_length(data_policy::text) <= 65536),
    CHECK (octet_length(limits::text) <= 16384)
);
CREATE INDEX projects_organization_idx ON public.projects (organization_id, created_at DESC);

CREATE TABLE public.capability_grants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    actor_id UUID NOT NULL REFERENCES public.actors(id) ON DELETE CASCADE,
    project_id UUID NOT NULL REFERENCES public.projects(id) ON DELETE CASCADE,
    actions TEXT[] NOT NULL CHECK (
        cardinality(actions) BETWEEN 1 AND 6
        AND array_position(actions, NULL) IS NULL
        AND actions <@ ARRAY['read', 'write', 'execute', 'model', 'manage', 'budget']::TEXT[]
    ),
    resources TEXT[] NOT NULL CHECK (
        cardinality(resources) = 1
        AND array_lower(resources, 1) = 1
        AND resources[1] IS NOT NULL
        AND resources[1] IN ('*', project_id::TEXT)
    ),
    environment TEXT NOT NULL CHECK (environment IN ('development', 'production')),
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    created_by UUID NOT NULL REFERENCES public.actors(id) ON DELETE RESTRICT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (expires_at IS NULL OR expires_at > created_at)
);
CREATE INDEX capability_grants_actor_project_idx ON public.capability_grants (actor_id, project_id, environment)
    WHERE revoked_at IS NULL;
CREATE INDEX capability_grants_project_idx ON public.capability_grants (project_id, environment)
    WHERE revoked_at IS NULL;

CREATE TABLE public.app_revisions (
    project_id UUID NOT NULL REFERENCES public.projects(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL CHECK (revision >= 1),
    spec JSONB NOT NULL,
    created_by UUID NOT NULL REFERENCES public.actors(id) ON DELETE RESTRICT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (project_id, revision),
    CHECK (octet_length(spec::text) <= 1048576)
);
CREATE INDEX app_revisions_created_idx ON public.app_revisions (project_id, created_at DESC);

CREATE TABLE public.change_commands (
    project_id UUID NOT NULL REFERENCES public.projects(id) ON DELETE CASCADE,
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 200),
    fingerprint BYTEA NOT NULL CHECK (octet_length(fingerprint) = 32),
    result JSONB NOT NULL,
    command_id UUID NOT NULL DEFAULT gen_random_uuid(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (project_id, idempotency_key),
    CHECK (jsonb_typeof(result) = 'object' AND octet_length(result::text) <= 32768)
);
CREATE INDEX change_commands_created_idx ON public.change_commands (project_id, created_at DESC);

CREATE TABLE public.decisions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES public.projects(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL CHECK (revision >= 0),
    actor_id UUID NOT NULL REFERENCES public.actors(id) ON DELETE RESTRICT,
    kind TEXT NOT NULL CHECK (length(kind) BETWEEN 1 AND 80),
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (octet_length(payload::text) <= 32768)
);
CREATE INDEX decisions_project_created_idx ON public.decisions (project_id, created_at DESC);

CREATE TABLE public.jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES public.projects(id) ON DELETE CASCADE,
    actor_id UUID NOT NULL REFERENCES public.actors(id) ON DELETE RESTRICT,
    environment TEXT NOT NULL CHECK (environment IN ('development', 'production')),
    source_revision BIGINT NOT NULL CHECK (source_revision >= 1),
    payload JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'running', 'succeeded', 'failed', 'cancelled', 'unknown', 'stale')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0 AND attempts <= 3),
    max_attempts INTEGER NOT NULL DEFAULT 3 CHECK (max_attempts BETWEEN 1 AND 3),
    generation BIGINT NOT NULL DEFAULT 0 CHECK (generation >= 0),
    lease_owner TEXT CHECK (lease_owner IS NULL OR length(lease_owner) BETWEEN 1 AND 200),
    lease_until TIMESTAMPTZ,
    deadline TIMESTAMPTZ NOT NULL,
    cancel_requested BOOLEAN NOT NULL DEFAULT FALSE,
    result JSONB,
    error_code TEXT CHECK (error_code IS NULL OR error_code IN (
        'unsupported_payload', 'execution_failed', 'retryable_internal', 'gateway_unavailable',
        'permission_revoked', 'source_stale', 'deadline_expired', 'attempts_exceeded', 'cancelled', 'lease_lost'
    )),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (attempts <= max_attempts),
    CHECK (octet_length(payload::text) <= 1048576),
    CHECK ((status = 'running') = (lease_owner IS NOT NULL AND lease_until IS NOT NULL)),
    CHECK (result IS NULL OR (jsonb_typeof(result) = 'object' AND octet_length(result::text) <= 32768)),
    CHECK (status <> 'succeeded' OR result IS NOT NULL),
    CHECK (status <> 'failed' OR error_code IS NOT NULL),
    CHECK (status <> 'pending' OR generation >= 0),
    CHECK (deadline > created_at),
    UNIQUE (id, project_id),
    FOREIGN KEY (project_id, source_revision) REFERENCES public.app_revisions(project_id, revision) ON DELETE RESTRICT
);
CREATE INDEX jobs_queue_idx ON public.jobs (environment, status, created_at, id)
    WHERE status IN ('pending', 'running');
CREATE INDEX jobs_project_idx ON public.jobs (project_id, created_at DESC);
CREATE INDEX jobs_actor_idx ON public.jobs (actor_id, created_at DESC);

CREATE TABLE public.project_budgets (
    project_id UUID PRIMARY KEY REFERENCES public.projects(id) ON DELETE CASCADE,
    limit_units BIGINT NOT NULL DEFAULT 0 CHECK (limit_units >= 0),
    reserved_units BIGINT NOT NULL DEFAULT 0 CHECK (reserved_units >= 0),
    spent_units BIGINT NOT NULL DEFAULT 0 CHECK (spent_units >= 0),
    currency TEXT NOT NULL DEFAULT 'SYN' CHECK (currency ~ '^[A-Z][A-Z0-9_]{2,11}$'),
    unit_scale BIGINT NOT NULL DEFAULT 1 CHECK (unit_scale BETWEEN 1 AND 1000000000),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (spent_units <= limit_units),
    CHECK (reserved_units <= limit_units - spent_units)
);

CREATE FUNCTION public.kyro_guard_budget_unit_change() RETURNS TRIGGER
LANGUAGE plpgsql SET search_path = pg_catalog, public
AS $$
BEGIN
    IF (NEW.currency IS DISTINCT FROM OLD.currency OR NEW.unit_scale IS DISTINCT FROM OLD.unit_scale)
       AND (OLD.reserved_units <> 0 OR OLD.spent_units <> 0) THEN
        RAISE EXCEPTION 'budget unit cannot change after reservation or spend' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER project_budgets_unit_immutable
    BEFORE UPDATE OF currency, unit_scale ON public.project_budgets
    FOR EACH ROW EXECUTE FUNCTION public.kyro_guard_budget_unit_change();

CREATE TABLE public.effects (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id UUID NOT NULL UNIQUE,
    project_id UUID NOT NULL REFERENCES public.projects(id) ON DELETE CASCADE,
    generation BIGINT NOT NULL CHECK (generation >= 1),
    destination TEXT NOT NULL CHECK (length(destination) BETWEEN 1 AND 200),
    fingerprint BYTEA NOT NULL CHECK (octet_length(fingerprint) = 32),
    intent JSONB NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('prepared', 'sending', 'succeeded', 'failed', 'unknown', 'cancelled')),
    result JSONB,
    reservation_id UUID UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (jsonb_typeof(intent) = 'object' AND octet_length(intent::text) <= 32768),
    CHECK (result IS NULL OR (jsonb_typeof(result) IN ('object', 'array') AND octet_length(result::text) <= 65536)),
    UNIQUE (id, job_id, project_id),
    FOREIGN KEY (job_id, project_id) REFERENCES public.jobs(id, project_id) ON DELETE RESTRICT
);
CREATE INDEX effects_project_created_idx ON public.effects (project_id, created_at DESC);

CREATE TABLE public.budget_reservations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES public.projects(id) ON DELETE CASCADE,
    job_id UUID NOT NULL REFERENCES public.jobs(id) ON DELETE RESTRICT,
    effect_id UUID NOT NULL UNIQUE,
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 200),
    units BIGINT NOT NULL CHECK (units > 0),
    status TEXT NOT NULL CHECK (status IN ('held', 'settled', 'released')),
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (project_id, idempotency_key),
    UNIQUE (id, job_id, project_id),
    FOREIGN KEY (job_id, project_id) REFERENCES public.jobs(id, project_id) ON DELETE RESTRICT,
    CHECK (expires_at IS NULL OR expires_at > created_at)
);
ALTER TABLE public.effects
    ADD CONSTRAINT effects_reservation_fk FOREIGN KEY (reservation_id, job_id, project_id)
    REFERENCES public.budget_reservations(id, job_id, project_id) ON DELETE RESTRICT DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE public.budget_reservations
    ADD CONSTRAINT reservations_effect_scope_fk FOREIGN KEY (effect_id, job_id, project_id)
    REFERENCES public.effects(id, job_id, project_id) ON DELETE RESTRICT DEFERRABLE INITIALLY DEFERRED;
CREATE INDEX budget_reservations_project_status_idx ON public.budget_reservations (project_id, status, created_at DESC);
CREATE INDEX budget_reservations_job_idx ON public.budget_reservations (job_id);

CREATE TABLE public.usage_ledger (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES public.projects(id) ON DELETE CASCADE,
    job_id UUID NOT NULL REFERENCES public.jobs(id) ON DELETE RESTRICT,
    reservation_id UUID NOT NULL UNIQUE,
    units BIGINT NOT NULL CHECK (units >= 0),
    kind TEXT NOT NULL CHECK (kind IN ('settlement', 'adjustment')),
    provider TEXT CHECK (provider IS NULL OR length(provider) BETWEEN 1 AND 100),
    model TEXT CHECK (model IS NULL OR length(model) BETWEEN 1 AND 200),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (octet_length(metadata::text) <= 16384),
    FOREIGN KEY (job_id, project_id) REFERENCES public.jobs(id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (reservation_id, job_id, project_id)
        REFERENCES public.budget_reservations(id, job_id, project_id) ON DELETE RESTRICT
);
CREATE INDEX usage_ledger_project_recorded_idx ON public.usage_ledger (project_id, recorded_at DESC);
CREATE INDEX usage_ledger_job_idx ON public.usage_ledger (job_id);

CREATE TABLE public.events (
    project_id UUID NOT NULL REFERENCES public.projects(id) ON DELETE CASCADE,
    sequence BIGINT NOT NULL CHECK (sequence >= 1),
    type TEXT NOT NULL CHECK (type ~ '^[a-z][a-z0-9_.-]{0,79}$'),
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    actor_id UUID REFERENCES public.actors(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (project_id, sequence),
    CHECK (octet_length(payload::text) <= 32768)
);
CREATE INDEX events_project_created_idx ON public.events (project_id, created_at DESC);

CREATE TABLE public.outbox_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL,
    event_sequence BIGINT NOT NULL,
    topic TEXT NOT NULL CHECK (length(topic) BETWEEN 1 AND 100),
    payload JSONB NOT NULL,
    available_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    delivered_at TIMESTAMPTZ,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (project_id, event_sequence),
    FOREIGN KEY (project_id, event_sequence) REFERENCES public.events(project_id, sequence) ON DELETE CASCADE,
    CHECK (octet_length(payload::text) <= 32768)
);
CREATE INDEX outbox_pending_idx ON public.outbox_events (available_at, created_at) WHERE delivered_at IS NULL;

CREATE TABLE public.runtime_control (
    id SMALLINT PRIMARY KEY CHECK (id = 1),
    external_sends_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
INSERT INTO public.runtime_control (id, external_sends_enabled) VALUES (1, TRUE);

-- SECURITY DEFINER helpers are owned by the privileged migrator. The
-- migrator must have BYPASSRLS/SUPERUSER; runtime roles never receive it.
CREATE FUNCTION public.kyro_actor_id() RETURNS UUID
LANGUAGE SQL STABLE PARALLEL SAFE
AS $$ SELECT NULLIF(pg_catalog.current_setting('kyro.actor_id', true), '')::UUID $$;

CREATE FUNCTION public.kyro_environment() RETURNS TEXT
LANGUAGE SQL STABLE PARALLEL SAFE
AS $$ SELECT NULLIF(pg_catalog.current_setting('kyro.environment', true), '') $$;

CREATE FUNCTION public.kyro_has_org_member(target_org UUID, target_actor UUID) RETURNS BOOLEAN
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT EXISTS (
        SELECT 1 FROM public.memberships m
        WHERE m.organization_id = target_org AND m.actor_id = target_actor
    )
$$;

CREATE FUNCTION public.kyro_has_org_owner(target_org UUID, target_actor UUID) RETURNS BOOLEAN
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT EXISTS (
        SELECT 1 FROM public.memberships m
        WHERE m.organization_id = target_org AND m.actor_id = target_actor AND m.role = 'owner'
    )
$$;

CREATE FUNCTION public.kyro_org_created_by(target_org UUID, target_actor UUID) RETURNS BOOLEAN
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT EXISTS (
        SELECT 1 FROM public.organizations o
        WHERE o.id = target_org AND o.created_by = target_actor
    )
$$;

CREATE FUNCTION public.has_project_org_owner(target_project UUID, target_actor UUID) RETURNS BOOLEAN
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT EXISTS (
        SELECT 1 FROM public.projects p
        JOIN public.memberships m ON m.organization_id = p.organization_id
        WHERE p.id = target_project AND m.actor_id = target_actor AND m.role = 'owner'
    )
$$;

CREATE FUNCTION public.has_project_org_member(target_project UUID, target_actor UUID) RETURNS BOOLEAN
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT EXISTS (
        SELECT 1 FROM public.projects p
        JOIN public.memberships m ON m.organization_id = p.organization_id
        WHERE p.id = target_project AND m.actor_id = target_actor
    )
$$;

CREATE FUNCTION public.kyro_project_created_by(target_project UUID, target_actor UUID) RETURNS BOOLEAN
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT EXISTS (
        SELECT 1 FROM public.projects p
        WHERE p.id = target_project AND p.created_by = target_actor
    )
$$;

CREATE FUNCTION public.kyro_project_active_limit(target_project UUID) RETURNS INTEGER
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT CASE WHEN session_user <> 'kyro_worker'
                      OR pg_catalog.current_setting('kyro.queue_claim', true) IS DISTINCT FROM 'on'
                      OR public.kyro_environment() IS NULL
                      OR public.kyro_environment() NOT IN ('development', 'production')
        THEN NULL
        ELSE CASE
            WHEN NOT (p.limits ? 'max_active_jobs') THEN 4
            WHEN jsonb_typeof(p.limits->'max_active_jobs') = 'number'
                 AND (p.limits->>'max_active_jobs') ~ '^[1-9][0-9]?$'
                 AND (p.limits->>'max_active_jobs')::INTEGER BETWEEN 1 AND 32
                THEN (p.limits->>'max_active_jobs')::INTEGER
            ELSE 1
        END
    END
    FROM public.projects p
    WHERE p.id = target_project
$$;

CREATE FUNCTION public.kyro_project_visible(target_project UUID) RETURNS BOOLEAN
LANGUAGE SQL VOLATILE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT public.kyro_actor_id() IS NOT NULL
       AND public.kyro_environment() IN ('development', 'production')
       AND EXISTS (
           SELECT 1 FROM public.capability_grants g
           WHERE g.project_id = target_project
             AND g.actor_id = public.kyro_actor_id()
             AND g.environment = public.kyro_environment()
             AND (g.expires_at IS NULL OR g.expires_at > clock_timestamp())
             AND g.revoked_at IS NULL
             AND (g.resources @> ARRAY['*']::TEXT[] OR g.resources @> ARRAY[target_project::TEXT]::TEXT[])
       )
$$;

CREATE FUNCTION public.kyro_actor_has_action(target_project UUID, target_action TEXT) RETURNS BOOLEAN
LANGUAGE SQL VOLATILE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT public.kyro_actor_id() IS NOT NULL
       AND public.kyro_environment() IN ('development', 'production')
       AND EXISTS (
           SELECT 1 FROM public.capability_grants g
           WHERE g.project_id = target_project
             AND g.actor_id = public.kyro_actor_id()
             AND g.environment = public.kyro_environment()
             AND target_action = ANY(g.actions)
             AND (g.expires_at IS NULL OR g.expires_at > clock_timestamp())
             AND g.revoked_at IS NULL
             AND (g.resources @> ARRAY['*']::TEXT[] OR g.resources @> ARRAY[target_project::TEXT]::TEXT[])
       )
$$;

CREATE FUNCTION public.grant_initial_project_owner(target_project UUID) RETURNS UUID
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
DECLARE
    actor UUID := public.kyro_actor_id();
    grant_id UUID;
BEGIN
    IF actor IS NULL OR public.kyro_environment() IS NULL
       OR public.kyro_environment() NOT IN ('development', 'production') THEN
        RAISE EXCEPTION 'actor context required' USING ERRCODE = '42501';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM public.projects p
        JOIN public.memberships m ON m.organization_id = p.organization_id
        WHERE p.id = target_project AND p.created_by = actor
          AND m.actor_id = actor AND m.role = 'owner'
    ) THEN
        RAISE EXCEPTION 'initial owner grant not authorized' USING ERRCODE = '42501';
    END IF;
    IF EXISTS (SELECT 1 FROM public.capability_grants g WHERE g.project_id = target_project) THEN
        RAISE EXCEPTION 'project already has grants' USING ERRCODE = '23505';
    END IF;
    INSERT INTO public.capability_grants
        (actor_id, project_id, actions, resources, environment, created_by)
    VALUES
        (actor, target_project,
         ARRAY['read', 'write', 'execute', 'model', 'manage', 'budget'],
         ARRAY['*'], public.kyro_environment(), actor)
    RETURNING id INTO grant_id;
    RETURN grant_id;
END
$$;

CREATE FUNCTION public.kyro_worker_can_access_job(target_job UUID, target_project UUID) RETURNS BOOLEAN
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT session_user = 'kyro_worker'
       AND pg_catalog.current_setting('kyro.queue_claim', true) = 'on'
       AND public.kyro_environment() IN ('development', 'production')
       AND EXISTS (
           SELECT 1 FROM public.jobs j
           WHERE j.id = target_job AND j.project_id = target_project
             AND j.environment = public.kyro_environment()
       )
$$;

CREATE FUNCTION public.kyro_worker_can_access_effect(target_effect UUID, target_project UUID) RETURNS BOOLEAN
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT session_user = 'kyro_worker'
       AND pg_catalog.current_setting('kyro.queue_claim', true) = 'on'
       AND public.kyro_environment() IN ('development', 'production')
       AND EXISTS (
           SELECT 1 FROM public.effects e
           JOIN public.jobs j ON j.id = e.job_id
           WHERE e.id = target_effect AND e.project_id = target_project
             AND j.project_id = e.project_id AND j.environment = public.kyro_environment()
       )
$$;

CREATE FUNCTION public.kyro_worker_can_account_project(target_project UUID) RETURNS BOOLEAN
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT session_user = 'kyro_worker'
       AND pg_catalog.current_setting('kyro.queue_claim', true) = 'on'
       AND public.kyro_environment() IN ('development', 'production')
       AND EXISTS (
           SELECT 1 FROM public.jobs j
           JOIN public.effects e ON e.job_id = j.id AND e.project_id = j.project_id
           WHERE j.project_id = target_project AND j.environment = public.kyro_environment()
       )
$$;

CREATE FUNCTION public.kyro_worker_can_access_reservation(target_reservation UUID, target_project UUID, target_job UUID) RETURNS BOOLEAN
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT session_user = 'kyro_worker'
       AND pg_catalog.current_setting('kyro.queue_claim', true) = 'on'
       AND public.kyro_environment() IN ('development', 'production')
       AND EXISTS (
           SELECT 1 FROM public.budget_reservations r
           JOIN public.effects e ON e.id = r.effect_id AND e.job_id = r.job_id AND e.project_id = r.project_id
           JOIN public.jobs j ON j.id = r.job_id AND j.project_id = r.project_id
           WHERE r.id = target_reservation AND r.project_id = target_project
             AND r.job_id = target_job AND j.environment = public.kyro_environment()
       )
$$;

CREATE FUNCTION public.kyro_append_event(project_id UUID, kind TEXT, payload JSONB) RETURNS public.events
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
DECLARE
    event_actor UUID;
    next_sequence BIGINT;
    event_row public.events;
    payload_keys TEXT[] := ARRAY['job_id', 'effect_id', 'reservation_id', 'status', 'generation', 'attempts', 'units', 'error_code', 'revision'];
BEGIN
    IF kind IS NULL OR kind !~ '^[a-z][a-z0-9_.-]{0,79}$'
       OR payload IS NULL OR jsonb_typeof(payload) <> 'object'
       OR octet_length(payload::text) > 32768 THEN
        RAISE EXCEPTION 'invalid event' USING ERRCODE = '22023';
    END IF;

    IF session_user = 'kyro_api' THEN
        IF NOT public.kyro_project_visible(project_id) THEN
            RAISE EXCEPTION 'project not found' USING ERRCODE = 'P0002';
        END IF;
        event_actor := public.kyro_actor_id();
    ELSIF session_user = 'kyro_worker' THEN
        IF pg_catalog.current_setting('kyro.queue_claim', true) IS DISTINCT FROM 'on'
           OR public.kyro_environment() IS NULL
           OR public.kyro_environment() NOT IN ('development', 'production') THEN
            RAISE EXCEPTION 'worker context required' USING ERRCODE = '42501';
        END IF;
        IF kind NOT LIKE 'job.%' AND kind NOT LIKE 'effect.%' AND kind NOT LIKE 'budget.%' THEN
            RAISE EXCEPTION 'worker event kind denied' USING ERRCODE = '42501';
        END IF;
        IF payload - payload_keys <> '{}'::JSONB OR jsonb_object_length(payload) > 9 THEN
            RAISE EXCEPTION 'worker event payload denied' USING ERRCODE = '42501';
        END IF;
        IF payload ? 'job_id' AND NOT EXISTS (
            SELECT 1 FROM public.jobs j
            WHERE j.id = (payload->>'job_id')::UUID AND j.project_id = project_id
              AND j.environment = public.kyro_environment()
        ) THEN
            RAISE EXCEPTION 'worker event job mismatch' USING ERRCODE = '42501';
        END IF;
        IF payload ? 'effect_id' AND NOT EXISTS (
            SELECT 1 FROM public.effects e JOIN public.jobs j ON j.id = e.job_id
            WHERE e.id = (payload->>'effect_id')::UUID AND e.project_id = project_id
              AND j.project_id = e.project_id AND j.environment = public.kyro_environment()
        ) THEN
            RAISE EXCEPTION 'worker event effect mismatch' USING ERRCODE = '42501';
        END IF;
        IF payload ? 'reservation_id' AND NOT EXISTS (
            SELECT 1 FROM public.budget_reservations r JOIN public.jobs j ON j.id = r.job_id
            WHERE r.id = (payload->>'reservation_id')::UUID AND r.project_id = project_id
              AND j.project_id = r.project_id AND j.environment = public.kyro_environment()
        ) THEN
            RAISE EXCEPTION 'worker event reservation mismatch' USING ERRCODE = '42501';
        END IF;
        IF NOT (payload ? 'job_id' OR payload ? 'effect_id' OR payload ? 'reservation_id') THEN
            RAISE EXCEPTION 'worker event requires linked reference' USING ERRCODE = '42501';
        END IF;
        event_actor := NULL;
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

ALTER TABLE public.actors ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.actors FORCE ROW LEVEL SECURITY;
CREATE POLICY actors_api_auth ON public.actors TO kyro_api USING (TRUE) WITH CHECK (TRUE);

ALTER TABLE public.organizations ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.organizations FORCE ROW LEVEL SECURITY;
CREATE POLICY organizations_member_read ON public.organizations FOR SELECT TO kyro_api
    USING (public.kyro_has_org_member(id, public.kyro_actor_id()));
CREATE POLICY organizations_create ON public.organizations FOR INSERT TO kyro_api
    WITH CHECK (created_by = public.kyro_actor_id());
CREATE POLICY organizations_owner_update ON public.organizations FOR UPDATE TO kyro_api
    USING (public.kyro_has_org_owner(id, public.kyro_actor_id()))
    WITH CHECK (public.kyro_has_org_owner(id, public.kyro_actor_id()));

ALTER TABLE public.memberships ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.memberships FORCE ROW LEVEL SECURITY;
CREATE POLICY memberships_visible ON public.memberships FOR SELECT TO kyro_api
    USING (actor_id = public.kyro_actor_id() OR public.kyro_has_org_owner(organization_id, public.kyro_actor_id()));
CREATE POLICY memberships_bootstrap_owner ON public.memberships FOR INSERT TO kyro_api
    WITH CHECK (
        actor_id = public.kyro_actor_id() AND created_by = public.kyro_actor_id() AND role = 'owner'
        AND public.kyro_org_created_by(organization_id, public.kyro_actor_id())
    );
CREATE POLICY memberships_owner_add_member ON public.memberships FOR INSERT TO kyro_api
    WITH CHECK (
        created_by = public.kyro_actor_id()
        AND role IN ('admin', 'member')
        AND public.kyro_has_org_owner(organization_id, public.kyro_actor_id())
    );
CREATE POLICY memberships_owner_manage ON public.memberships FOR UPDATE TO kyro_api
    USING (public.kyro_has_org_owner(organization_id, public.kyro_actor_id()))
    WITH CHECK (public.kyro_has_org_owner(organization_id, public.kyro_actor_id()));

ALTER TABLE public.sessions ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.sessions FORCE ROW LEVEL SECURITY;
CREATE POLICY sessions_api_auth ON public.sessions TO kyro_api USING (TRUE) WITH CHECK (TRUE);

ALTER TABLE public.login_flows ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.login_flows FORCE ROW LEVEL SECURITY;
CREATE POLICY login_flows_api_auth ON public.login_flows TO kyro_api USING (TRUE) WITH CHECK (TRUE);

ALTER TABLE public.projects ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.projects FORCE ROW LEVEL SECURITY;
CREATE POLICY projects_visible ON public.projects FOR SELECT TO kyro_api, kyro_worker
    USING (public.kyro_project_visible(id));
CREATE POLICY projects_create_by_org_owner ON public.projects FOR INSERT TO kyro_api
    WITH CHECK (
        created_by = public.kyro_actor_id()
        AND public.kyro_has_org_owner(organization_id, public.kyro_actor_id())
    );
CREATE POLICY projects_actor_update ON public.projects FOR UPDATE TO kyro_api, kyro_worker
    USING (public.kyro_actor_has_action(id, 'write') OR public.kyro_actor_has_action(id, 'manage'))
    WITH CHECK (public.kyro_actor_has_action(id, 'write') OR public.kyro_actor_has_action(id, 'manage'));

ALTER TABLE public.capability_grants ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.capability_grants FORCE ROW LEVEL SECURITY;
CREATE POLICY grants_visible ON public.capability_grants FOR SELECT TO kyro_api, kyro_worker
    USING (public.kyro_project_visible(project_id));
CREATE POLICY grants_owner_create ON public.capability_grants FOR INSERT TO kyro_api
    WITH CHECK (
        created_by = public.kyro_actor_id()
        AND environment = public.kyro_environment()
        AND public.has_project_org_owner(project_id, public.kyro_actor_id())
        AND public.has_project_org_member(project_id, actor_id)
    );
CREATE POLICY grants_owner_revoke ON public.capability_grants FOR UPDATE TO kyro_api
    USING (public.has_project_org_owner(project_id, public.kyro_actor_id()))
    WITH CHECK (
        public.has_project_org_owner(project_id, public.kyro_actor_id())
        AND public.has_project_org_member(project_id, actor_id)
        AND environment = public.kyro_environment()
    );

ALTER TABLE public.app_revisions ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_revisions FORCE ROW LEVEL SECURITY;
CREATE POLICY revisions_visible ON public.app_revisions FOR SELECT TO kyro_api, kyro_worker
    USING (public.kyro_project_visible(project_id));
CREATE POLICY revisions_append ON public.app_revisions FOR INSERT TO kyro_api, kyro_worker
    WITH CHECK (
        created_by = public.kyro_actor_id()
        AND (public.kyro_actor_has_action(project_id, 'write') OR public.kyro_actor_has_action(project_id, 'manage'))
    );

ALTER TABLE public.change_commands ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.change_commands FORCE ROW LEVEL SECURITY;
CREATE POLICY change_commands_visible ON public.change_commands FOR SELECT TO kyro_api, kyro_worker
    USING (public.kyro_project_visible(project_id));
CREATE POLICY change_commands_insert ON public.change_commands FOR INSERT TO kyro_api, kyro_worker
    WITH CHECK (public.kyro_actor_has_action(project_id, 'write') OR public.kyro_actor_has_action(project_id, 'manage'));

ALTER TABLE public.decisions ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.decisions FORCE ROW LEVEL SECURITY;
CREATE POLICY decisions_visible ON public.decisions FOR SELECT TO kyro_api, kyro_worker
    USING (public.kyro_project_visible(project_id));
CREATE POLICY decisions_insert ON public.decisions FOR INSERT TO kyro_api, kyro_worker
    WITH CHECK (
        actor_id = public.kyro_actor_id()
        AND (public.kyro_actor_has_action(project_id, 'write') OR public.kyro_actor_has_action(project_id, 'manage'))
    );

ALTER TABLE public.jobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.jobs FORCE ROW LEVEL SECURITY;
CREATE POLICY jobs_visible ON public.jobs FOR SELECT TO kyro_api, kyro_worker
    USING (
        public.kyro_project_visible(project_id)
        OR (current_user = 'kyro_worker'
            AND pg_catalog.current_setting('kyro.queue_claim', true) = 'on'
            AND environment = public.kyro_environment())
    );
CREATE POLICY jobs_admit ON public.jobs FOR INSERT TO kyro_api
    WITH CHECK (
        actor_id = public.kyro_actor_id()
        AND environment = public.kyro_environment()
        AND public.kyro_actor_has_action(project_id, 'execute')
    );
CREATE POLICY jobs_update_actor ON public.jobs FOR UPDATE TO kyro_worker
    USING (
        actor_id = public.kyro_actor_id()
        AND public.kyro_project_visible(project_id)
        AND environment = public.kyro_environment()
    )
    WITH CHECK (
        actor_id = public.kyro_actor_id()
        AND public.kyro_project_visible(project_id)
        AND environment = public.kyro_environment()
    );
CREATE POLICY jobs_update_queue ON public.jobs FOR UPDATE TO kyro_worker
    USING (pg_catalog.current_setting('kyro.queue_claim', true) = 'on' AND environment = public.kyro_environment())
    WITH CHECK (pg_catalog.current_setting('kyro.queue_claim', true) = 'on' AND environment = public.kyro_environment());

ALTER TABLE public.project_budgets ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.project_budgets FORCE ROW LEVEL SECURITY;
CREATE POLICY budgets_visible ON public.project_budgets FOR SELECT TO kyro_api, kyro_worker
    USING (
        public.kyro_project_visible(project_id)
        OR public.kyro_worker_can_account_project(project_id)
    );
CREATE POLICY budgets_initial_zero ON public.project_budgets FOR INSERT TO kyro_api
    WITH CHECK (
        limit_units = 0 AND reserved_units = 0 AND spent_units = 0
        AND currency = 'SYN' AND unit_scale = 1
        AND public.kyro_project_created_by(project_id, public.kyro_actor_id())
        AND public.has_project_org_owner(project_id, public.kyro_actor_id())
    );
CREATE POLICY budgets_api_configure ON public.project_budgets FOR UPDATE TO kyro_api
    USING (public.kyro_actor_has_action(project_id, 'budget') OR public.kyro_actor_has_action(project_id, 'manage'))
    WITH CHECK (public.kyro_actor_has_action(project_id, 'budget') OR public.kyro_actor_has_action(project_id, 'manage'));
CREATE POLICY budgets_worker_account ON public.project_budgets FOR UPDATE TO kyro_worker
    USING (public.kyro_worker_can_account_project(project_id))
    WITH CHECK (public.kyro_worker_can_account_project(project_id));

ALTER TABLE public.effects ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.effects FORCE ROW LEVEL SECURITY;
CREATE POLICY effects_visible ON public.effects FOR SELECT TO kyro_api, kyro_worker
    USING (public.kyro_project_visible(project_id) OR public.kyro_worker_can_access_effect(id, project_id));
CREATE POLICY effects_prepare ON public.effects FOR INSERT TO kyro_worker
    WITH CHECK (
        EXISTS (
            SELECT 1 FROM public.jobs j
            WHERE j.id = job_id AND j.project_id = project_id AND j.actor_id = public.kyro_actor_id()
              AND j.environment = public.kyro_environment()
        )
        AND public.kyro_actor_has_action(project_id, 'model')
    );
CREATE POLICY effects_worker_account ON public.effects FOR UPDATE TO kyro_worker
    USING (public.kyro_worker_can_access_effect(id, project_id))
    WITH CHECK (public.kyro_worker_can_access_effect(id, project_id));

ALTER TABLE public.budget_reservations ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.budget_reservations FORCE ROW LEVEL SECURITY;
CREATE POLICY reservations_visible ON public.budget_reservations FOR SELECT TO kyro_api, kyro_worker
    USING (
        public.kyro_project_visible(project_id)
        OR public.kyro_worker_can_access_job(job_id, project_id)
    );
CREATE POLICY reservations_prepare ON public.budget_reservations FOR INSERT TO kyro_worker
    WITH CHECK (
        EXISTS (
            SELECT 1 FROM public.jobs j
            WHERE j.id = job_id AND j.project_id = project_id AND j.actor_id = public.kyro_actor_id()
              AND j.environment = public.kyro_environment()
        )
        AND public.kyro_actor_has_action(project_id, 'model')
    );
CREATE POLICY reservations_worker_account ON public.budget_reservations FOR UPDATE TO kyro_worker
    USING (public.kyro_worker_can_access_job(job_id, project_id))
    WITH CHECK (public.kyro_worker_can_access_job(job_id, project_id));

ALTER TABLE public.usage_ledger ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.usage_ledger FORCE ROW LEVEL SECURITY;
CREATE POLICY usage_visible ON public.usage_ledger FOR SELECT TO kyro_api, kyro_worker
    USING (public.kyro_project_visible(project_id) OR public.kyro_worker_can_access_job(job_id, project_id));
CREATE POLICY usage_worker_append ON public.usage_ledger FOR INSERT TO kyro_worker
    WITH CHECK (public.kyro_worker_can_access_reservation(reservation_id, project_id, job_id));

ALTER TABLE public.events ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.events FORCE ROW LEVEL SECURITY;
CREATE POLICY events_visible ON public.events FOR SELECT TO kyro_api, kyro_worker
    USING (public.kyro_project_visible(project_id));

ALTER TABLE public.outbox_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.outbox_events FORCE ROW LEVEL SECURITY;
CREATE POLICY outbox_visible ON public.outbox_events FOR SELECT TO kyro_api
    USING (public.kyro_project_visible(project_id));

-- Auth/control-plane access is server-only. Tenant records remain RLS scoped.
GRANT SELECT, INSERT, UPDATE ON public.actors, public.organizations, public.memberships,
    public.sessions, public.login_flows TO kyro_api;
GRANT SELECT, INSERT ON public.projects TO kyro_api;
GRANT UPDATE (name, data_policy, limits, current_revision, updated_at) ON public.projects TO kyro_api;
GRANT SELECT, INSERT ON public.capability_grants TO kyro_api;
GRANT UPDATE (actions, resources, environment, expires_at, revoked_at) ON public.capability_grants TO kyro_api;
GRANT SELECT, INSERT ON public.app_revisions, public.change_commands, public.decisions, public.jobs TO kyro_api;
GRANT SELECT ON public.project_budgets, public.effects, public.budget_reservations, public.usage_ledger,
    public.events, public.outbox_events, public.runtime_control TO kyro_api;
GRANT INSERT ON public.project_budgets TO kyro_api;
GRANT UPDATE (limit_units, currency, unit_scale, updated_at) ON public.project_budgets TO kyro_api;

GRANT SELECT, UPDATE ON public.jobs TO kyro_worker;
GRANT SELECT, INSERT ON public.app_revisions, public.change_commands, public.decisions TO kyro_worker;
GRANT SELECT ON public.projects TO kyro_worker;
GRANT UPDATE (name, data_policy, limits, current_revision, updated_at) ON public.projects TO kyro_worker;
GRANT SELECT ON public.capability_grants TO kyro_worker;
GRANT SELECT, INSERT, UPDATE ON public.effects, public.budget_reservations TO kyro_worker;
GRANT SELECT, INSERT ON public.usage_ledger TO kyro_worker;
GRANT SELECT ON public.runtime_control TO kyro_worker;
GRANT UPDATE (reserved_units, spent_units, updated_at) ON public.project_budgets TO kyro_worker;

REVOKE ALL ON public.events, public.outbox_events FROM PUBLIC, kyro_api, kyro_worker;
GRANT SELECT ON public.events TO kyro_api, kyro_worker;
GRANT SELECT ON public.outbox_events TO kyro_api;

REVOKE ALL ON public.runtime_control FROM PUBLIC, kyro_api, kyro_worker;
GRANT SELECT ON public.runtime_control TO kyro_api, kyro_worker;

REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA public FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.kyro_actor_id(), public.kyro_environment(),
    public.kyro_project_visible(UUID), public.kyro_actor_has_action(UUID, TEXT)
    TO kyro_api, kyro_worker;
GRANT EXECUTE ON FUNCTION public.kyro_has_org_member(UUID, UUID), public.kyro_has_org_owner(UUID, UUID),
    public.kyro_org_created_by(UUID, UUID), public.has_project_org_owner(UUID, UUID),
    public.has_project_org_member(UUID, UUID), public.kyro_project_created_by(UUID, UUID) TO kyro_api;
GRANT EXECUTE ON FUNCTION public.grant_initial_project_owner(UUID) TO kyro_api;
GRANT EXECUTE ON FUNCTION public.kyro_worker_can_access_job(UUID, UUID),
    public.kyro_worker_can_access_effect(UUID, UUID), public.kyro_worker_can_account_project(UUID),
    public.kyro_worker_can_access_reservation(UUID, UUID, UUID),
    public.kyro_project_active_limit(UUID) TO kyro_worker;
GRANT EXECUTE ON FUNCTION public.kyro_append_event(UUID, TEXT, JSONB) TO kyro_api, kyro_worker;
GRANT EXECUTE ON FUNCTION public.kyro_guard_budget_unit_change() TO kyro_api, kyro_worker;

