-- P2 application runtime schema. Apply with a dedicated migration/admin role.
DO $roles$
BEGIN
    IF current_user IN ('kyro_app', 'kyro_app_runtime') THEN
        RAISE EXCEPTION 'application core migration requires an administrative role';
    END IF;

    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'kyro_app') THEN
        CREATE ROLE kyro_app NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
    END IF;
    ALTER ROLE kyro_app WITH NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;

    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'kyro_app_runtime') THEN
        CREATE ROLE kyro_app_runtime LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
    END IF;
    ALTER ROLE kyro_app_runtime WITH LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
    GRANT kyro_app TO kyro_app_runtime;
END
$roles$;

GRANT USAGE ON SCHEMA public TO kyro_app;

CREATE TABLE public.app_tenants (
    id UUID PRIMARY KEY,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'suspended', 'closed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE public.app_applications (
    tenant_id UUID NOT NULL REFERENCES public.app_tenants(id) ON DELETE CASCADE,
    id UUID NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'suspended', 'closed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, id)
);

CREATE TABLE public.app_principals (
    tenant_id UUID NOT NULL REFERENCES public.app_tenants(id) ON DELETE CASCADE,
    id UUID NOT NULL DEFAULT gen_random_uuid(),
    display_name TEXT NOT NULL DEFAULT '' CHECK (length(display_name) <= 200),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled', 'locked')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, id)
);

CREATE TABLE public.app_memberships (
    tenant_id UUID NOT NULL,
    application_id UUID NOT NULL,
    principal_id UUID NOT NULL,
    role TEXT NOT NULL CHECK (length(role) BETWEEN 1 AND 128),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'suspended', 'revoked')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, principal_id, role),
    FOREIGN KEY (tenant_id, application_id)
        REFERENCES public.app_applications(tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, principal_id)
        REFERENCES public.app_principals(tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX app_memberships_principal_idx
    ON public.app_memberships (tenant_id, application_id, principal_id, status);

CREATE TABLE public.app_role_permissions (
    tenant_id UUID NOT NULL,
    application_id UUID NOT NULL,
    role TEXT NOT NULL CHECK (length(role) BETWEEN 1 AND 128),
    permission TEXT NOT NULL CHECK (length(permission) BETWEEN 1 AND 128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, role, permission),
    FOREIGN KEY (tenant_id, application_id)
        REFERENCES public.app_applications(tenant_id, id) ON DELETE CASCADE
);

CREATE TABLE public.app_sessions (
    tenant_id UUID NOT NULL,
    application_id UUID NOT NULL,
    id UUID NOT NULL DEFAULT gen_random_uuid(),
    principal_id UUID NOT NULL,
    token_hash BYTEA NOT NULL CHECK (octet_length(token_hash) = 32),
    csrf_hash BYTEA NOT NULL CHECK (octet_length(csrf_hash) = 32),
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    auth_time TIMESTAMPTZ,
    acr TEXT,
    amr TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[],
    mfa_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    last_seen_at TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, application_id, id),
    UNIQUE (tenant_id, application_id, token_hash),
    FOREIGN KEY (tenant_id, application_id)
        REFERENCES public.app_applications(tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, principal_id)
        REFERENCES public.app_principals(tenant_id, id) ON DELETE CASCADE,
    CHECK (expires_at > created_at),
    CHECK (cardinality(amr) <= 32)
);
CREATE INDEX app_sessions_principal_idx
    ON public.app_sessions (tenant_id, application_id, principal_id, expires_at)
    WHERE revoked_at IS NULL;

CREATE TABLE public.app_oidc_flows (
    tenant_id UUID NOT NULL,
    application_id UUID NOT NULL,
    issuer TEXT NOT NULL CHECK (length(issuer) BETWEEN 1 AND 512),
    state_hash BYTEA NOT NULL CHECK (octet_length(state_hash) = 32),
    nonce_hash BYTEA NOT NULL CHECK (octet_length(nonce_hash) = 32),
    browser_binding_hash BYTEA NOT NULL CHECK (octet_length(browser_binding_hash) = 32),
    pkce_verifier TEXT NOT NULL CHECK (length(pkce_verifier) BETWEEN 43 AND 128),
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, issuer, state_hash),
    FOREIGN KEY (tenant_id, application_id)
        REFERENCES public.app_applications(tenant_id, id) ON DELETE CASCADE,
    CHECK (expires_at > created_at),
    CHECK (expires_at <= created_at + INTERVAL '15 minutes')
);

CREATE TABLE public.app_external_identities (
    tenant_id UUID NOT NULL,
    application_id UUID NOT NULL,
    issuer TEXT NOT NULL CHECK (length(issuer) BETWEEN 1 AND 512),
    subject TEXT NOT NULL CHECK (length(subject) BETWEEN 1 AND 512),
    principal_id UUID NOT NULL,
    email TEXT CHECK (email IS NULL OR length(email) <= 320),
    email_verified BOOLEAN NOT NULL DEFAULT FALSE,
    auth_time TIMESTAMPTZ,
    acr TEXT,
    amr TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[],
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, issuer, subject),
    UNIQUE (tenant_id, application_id, principal_id, issuer),
    FOREIGN KEY (tenant_id, application_id)
        REFERENCES public.app_applications(tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, principal_id)
        REFERENCES public.app_principals(tenant_id, id) ON DELETE CASCADE,
    CHECK (cardinality(amr) <= 32)
);
CREATE INDEX app_external_identity_principal_idx
    ON public.app_external_identities (tenant_id, application_id, principal_id);

CREATE TABLE public.app_one_time_credentials (
    tenant_id UUID NOT NULL,
    application_id UUID NOT NULL,
    id UUID NOT NULL DEFAULT gen_random_uuid(),
    principal_id UUID NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('magic_link', 'recovery')),
    token_hash BYTEA NOT NULL CHECK (octet_length(token_hash) = 32),
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, id),
    UNIQUE (tenant_id, application_id, purpose, token_hash),
    FOREIGN KEY (tenant_id, application_id)
        REFERENCES public.app_applications(tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, principal_id)
        REFERENCES public.app_principals(tenant_id, id) ON DELETE CASCADE,
    CHECK (expires_at > created_at),
    CHECK (expires_at <= created_at + INTERVAL '24 hours')
);

CREATE TABLE public.app_api_keys (
    tenant_id UUID NOT NULL,
    application_id UUID NOT NULL,
    id UUID NOT NULL DEFAULT gen_random_uuid(),
    principal_id UUID NOT NULL,
    token_hash BYTEA NOT NULL CHECK (octet_length(token_hash) = 32),
    scope_ids TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[] CHECK (cardinality(scope_ids) <= 64),
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    last_used_at TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, application_id, id),
    UNIQUE (tenant_id, application_id, token_hash),
    FOREIGN KEY (tenant_id, application_id)
        REFERENCES public.app_applications(tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, principal_id)
        REFERENCES public.app_principals(tenant_id, id) ON DELETE CASCADE,
    CHECK (expires_at IS NULL OR expires_at > created_at)
);

CREATE TABLE public.app_records (
    tenant_id UUID NOT NULL,
    application_id UUID NOT NULL,
    kind TEXT NOT NULL CHECK (length(kind) BETWEEN 1 AND 128),
    id UUID NOT NULL,
    version BIGINT NOT NULL CHECK (version > 0),
    data JSONB NOT NULL CHECK (
        jsonb_typeof(data) = 'object' AND octet_length(data::TEXT) <= 65536
    ),
    created_by UUID NOT NULL,
    updated_by UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, kind, id),
    FOREIGN KEY (tenant_id, application_id)
        REFERENCES public.app_applications(tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, created_by)
        REFERENCES public.app_principals(tenant_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, updated_by)
        REFERENCES public.app_principals(tenant_id, id) ON DELETE RESTRICT
);
CREATE INDEX app_records_scope_kind_idx
    ON public.app_records (tenant_id, application_id, kind, id);

CREATE TABLE public.app_record_history (
    tenant_id UUID NOT NULL,
    application_id UUID NOT NULL,
    id UUID NOT NULL DEFAULT gen_random_uuid(),
    kind TEXT NOT NULL CHECK (length(kind) BETWEEN 1 AND 128),
    record_id UUID NOT NULL,
    version BIGINT NOT NULL CHECK (version > 0),
    operation TEXT NOT NULL CHECK (operation IN ('insert', 'update', 'delete')),
    data JSONB NOT NULL CHECK (
        jsonb_typeof(data) = 'object' AND octet_length(data::TEXT) <= 65536
    ),
    actor_principal_id UUID NOT NULL,
    changed_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, id),
    UNIQUE (tenant_id, application_id, kind, record_id, version),
    FOREIGN KEY (tenant_id, application_id)
        REFERENCES public.app_applications(tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, actor_principal_id)
        REFERENCES public.app_principals(tenant_id, id) ON DELETE RESTRICT
);
CREATE INDEX app_record_history_record_idx
    ON public.app_record_history (tenant_id, application_id, kind, record_id, version DESC);

CREATE TABLE public.app_idempotency (
    tenant_id UUID NOT NULL,
    application_id UUID NOT NULL,
    actor_principal_id UUID NOT NULL,
    component_id TEXT NOT NULL CHECK (length(component_id) BETWEEN 1 AND 128),
    action TEXT NOT NULL CHECK (length(action) BETWEEN 1 AND 128),
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 200),
    request_hash BYTEA NOT NULL CHECK (octet_length(request_hash) = 32),
    response JSONB NOT NULL CHECK (octet_length(response::TEXT) <= 1048576),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, actor_principal_id, component_id, action, idempotency_key),
    FOREIGN KEY (tenant_id, application_id)
        REFERENCES public.app_applications(tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, actor_principal_id)
        REFERENCES public.app_principals(tenant_id, id) ON DELETE CASCADE
);

CREATE TABLE public.app_events (
    tenant_id UUID NOT NULL,
    application_id UUID NOT NULL,
    sequence BIGINT GENERATED ALWAYS AS IDENTITY,
    id UUID NOT NULL DEFAULT gen_random_uuid(),
    actor_principal_id UUID NOT NULL,
    event_type TEXT NOT NULL CHECK (length(event_type) BETWEEN 1 AND 128),
    component_id TEXT NOT NULL CHECK (length(component_id) BETWEEN 1 AND 128),
    action TEXT NOT NULL CHECK (length(action) BETWEEN 1 AND 128),
    resource_id UUID,
    payload JSONB NOT NULL DEFAULT '{}'::JSONB CHECK (
        jsonb_typeof(payload) = 'object' AND octet_length(payload::TEXT) <= 65536
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, sequence),
    UNIQUE (tenant_id, application_id, id),
    FOREIGN KEY (tenant_id, application_id)
        REFERENCES public.app_applications(tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, actor_principal_id)
        REFERENCES public.app_principals(tenant_id, id) ON DELETE RESTRICT
);

CREATE TABLE public.app_outbox (
    tenant_id UUID NOT NULL,
    application_id UUID NOT NULL,
    id UUID NOT NULL DEFAULT gen_random_uuid(),
    event_id UUID NOT NULL,
    event_type TEXT NOT NULL CHECK (length(event_type) BETWEEN 1 AND 128),
    payload JSONB NOT NULL CHECK (
        jsonb_typeof(payload) = 'object' AND octet_length(payload::TEXT) <= 1048576
    ),
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'claimed', 'delivered', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    available_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, application_id, id),
    UNIQUE (tenant_id, application_id, event_id),
    FOREIGN KEY (tenant_id, application_id, event_id)
        REFERENCES public.app_events(tenant_id, application_id, id) ON DELETE CASCADE
);
CREATE INDEX app_outbox_ready_idx
    ON public.app_outbox (tenant_id, application_id, state, available_at, created_at);

CREATE TABLE public.app_quotas (
    tenant_id UUID NOT NULL,
    application_id UUID NOT NULL,
    quota_key TEXT NOT NULL CHECK (length(quota_key) BETWEEN 1 AND 128),
    limit_value BIGINT NOT NULL CHECK (limit_value >= 0),
    used_value BIGINT NOT NULL DEFAULT 0 CHECK (used_value >= 0),
    reserved_value BIGINT NOT NULL DEFAULT 0 CHECK (reserved_value >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, quota_key),
    FOREIGN KEY (tenant_id, application_id)
        REFERENCES public.app_applications(tenant_id, id) ON DELETE CASCADE,
    CHECK (used_value + reserved_value <= limit_value)
);

CREATE FUNCTION public.kyro_app_tenant_id() RETURNS UUID
LANGUAGE SQL STABLE PARALLEL SAFE
SET search_path = pg_catalog
AS $function$
    SELECT NULLIF(pg_catalog.current_setting('kyro.app_tenant_id', true), '')::UUID
$function$;

CREATE FUNCTION public.kyro_app_application_id() RETURNS UUID
LANGUAGE SQL STABLE PARALLEL SAFE
SET search_path = pg_catalog
AS $function$
    SELECT NULLIF(pg_catalog.current_setting('kyro.app_application_id', true), '')::UUID
$function$;

CREATE FUNCTION public.kyro_app_actor_id() RETURNS UUID
LANGUAGE SQL STABLE PARALLEL SAFE
SET search_path = pg_catalog
AS $function$
    SELECT NULLIF(pg_catalog.current_setting('kyro.app_actor_id', true), '')::UUID
$function$;

CREATE FUNCTION public.kyro_app_session_id() RETURNS UUID
LANGUAGE SQL STABLE PARALLEL SAFE
SET search_path = pg_catalog
AS $function$
    SELECT NULLIF(pg_catalog.current_setting('kyro.app_session_id', true), '')::UUID
$function$;

REVOKE ALL ON FUNCTION public.kyro_app_tenant_id(), public.kyro_app_application_id(),
    public.kyro_app_actor_id(), public.kyro_app_session_id() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.kyro_app_tenant_id(), public.kyro_app_application_id(),
    public.kyro_app_actor_id(), public.kyro_app_session_id() TO kyro_app;

ALTER TABLE public.app_tenants ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_tenants FORCE ROW LEVEL SECURITY;
CREATE POLICY app_tenant_scope ON public.app_tenants FOR SELECT TO kyro_app
    USING (id = public.kyro_app_tenant_id());

ALTER TABLE public.app_applications ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_applications FORCE ROW LEVEL SECURITY;
CREATE POLICY app_application_scope ON public.app_applications TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id() AND id = public.kyro_app_application_id())
    WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND id = public.kyro_app_application_id());

ALTER TABLE public.app_principals ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_principals FORCE ROW LEVEL SECURITY;
CREATE POLICY app_principal_scope ON public.app_principals TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id())
    WITH CHECK (tenant_id = public.kyro_app_tenant_id());

ALTER TABLE public.app_memberships ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_memberships FORCE ROW LEVEL SECURITY;
CREATE POLICY app_membership_scope ON public.app_memberships TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id())
    WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id());

ALTER TABLE public.app_role_permissions ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_role_permissions FORCE ROW LEVEL SECURITY;
CREATE POLICY app_role_permission_scope ON public.app_role_permissions TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id())
    WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id());

ALTER TABLE public.app_sessions ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_sessions FORCE ROW LEVEL SECURITY;
CREATE POLICY app_session_scope ON public.app_sessions TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id()
        AND principal_id = public.kyro_app_actor_id()
        AND (public.kyro_app_session_id() IS NULL OR id = public.kyro_app_session_id()))
    WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id()
        AND principal_id = public.kyro_app_actor_id());

ALTER TABLE public.app_oidc_flows ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_oidc_flows FORCE ROW LEVEL SECURITY;
CREATE POLICY app_oidc_flow_scope ON public.app_oidc_flows TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id())
    WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id());

ALTER TABLE public.app_external_identities ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_external_identities FORCE ROW LEVEL SECURITY;
CREATE POLICY app_external_identity_scope ON public.app_external_identities TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id())
    WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id());

ALTER TABLE public.app_one_time_credentials ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_one_time_credentials FORCE ROW LEVEL SECURITY;
CREATE POLICY app_one_time_credential_scope ON public.app_one_time_credentials TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id())
    WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id());

ALTER TABLE public.app_api_keys ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_api_keys FORCE ROW LEVEL SECURITY;
CREATE POLICY app_api_key_scope ON public.app_api_keys TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id())
    WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id());

ALTER TABLE public.app_records ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_records FORCE ROW LEVEL SECURITY;
CREATE POLICY app_record_select ON public.app_records FOR SELECT TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id());
CREATE POLICY app_record_insert ON public.app_records FOR INSERT TO kyro_app
    WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id()
        AND created_by = public.kyro_app_actor_id() AND updated_by = public.kyro_app_actor_id());
CREATE POLICY app_record_update ON public.app_records FOR UPDATE TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id())
    WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id()
        AND updated_by = public.kyro_app_actor_id());
CREATE POLICY app_record_delete ON public.app_records FOR DELETE TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id());

ALTER TABLE public.app_record_history ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_record_history FORCE ROW LEVEL SECURITY;
CREATE POLICY app_record_history_scope ON public.app_record_history TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id())
    WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id()
        AND actor_principal_id = public.kyro_app_actor_id());

ALTER TABLE public.app_idempotency ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_idempotency FORCE ROW LEVEL SECURITY;
CREATE POLICY app_idempotency_scope ON public.app_idempotency TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id()
        AND actor_principal_id = public.kyro_app_actor_id())
    WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id()
        AND actor_principal_id = public.kyro_app_actor_id());

ALTER TABLE public.app_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_events FORCE ROW LEVEL SECURITY;
CREATE POLICY app_event_scope ON public.app_events TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id())
    WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id()
        AND actor_principal_id = public.kyro_app_actor_id());

ALTER TABLE public.app_outbox ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_outbox FORCE ROW LEVEL SECURITY;
CREATE POLICY app_outbox_scope ON public.app_outbox TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id())
    WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id());

ALTER TABLE public.app_quotas ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_quotas FORCE ROW LEVEL SECURITY;
CREATE POLICY app_quota_scope ON public.app_quotas TO kyro_app
    USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id())
    WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id());

GRANT SELECT ON public.app_tenants, public.app_applications, public.app_role_permissions TO kyro_app;
GRANT SELECT, INSERT, UPDATE ON public.app_principals, public.app_memberships, public.app_sessions,
    public.app_oidc_flows, public.app_external_identities, public.app_one_time_credentials,
    public.app_api_keys TO kyro_app;
GRANT DELETE ON public.app_oidc_flows TO kyro_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON public.app_records TO kyro_app;
GRANT SELECT, INSERT ON public.app_record_history, public.app_idempotency, public.app_events TO kyro_app;
GRANT SELECT, INSERT ON public.app_outbox TO kyro_app;
GRANT SELECT, UPDATE ON public.app_quotas TO kyro_app;
GRANT USAGE, SELECT ON SEQUENCE public.app_events_sequence_seq TO kyro_app;
