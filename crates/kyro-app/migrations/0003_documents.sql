-- Tenant-scoped document storage and bounded external processing queue.
-- AppTx sets kyro.app_tenant_id transaction-locally from the verified actor.
CREATE OR REPLACE FUNCTION public.app_document_tenant_id()
RETURNS uuid
LANGUAGE sql
STABLE
AS $$
    SELECT NULLIF(pg_catalog.current_setting('kyro.app_tenant_id', true), '')::uuid
$$;

CREATE TABLE public.app_documents (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id uuid NOT NULL,
    id uuid NOT NULL,
    owner_id uuid NOT NULL,
    kind text NOT NULL CHECK (kind IN ('file', 'template', 'editorial')),
    state text NOT NULL,
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    content_version bigint NOT NULL DEFAULT 1 CHECK (content_version > 0),
    published_version bigint,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(metadata) = 'object'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, id),
    UNIQUE (tenant_id, application_id, id, kind),
    CHECK (published_version IS NULL OR published_version > 0)
);

CREATE TABLE public.app_document_usage (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id uuid NOT NULL,
    PRIMARY KEY (tenant_id, application_id),
    file_count bigint NOT NULL DEFAULT 0 CHECK (file_count >= 0),
    bytes_used bigint NOT NULL DEFAULT 0 CHECK (bytes_used >= 0),
    file_limit bigint NOT NULL DEFAULT 1000 CHECK (file_limit > 0),
    byte_limit bigint NOT NULL DEFAULT 104857600 CHECK (byte_limit > 0),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE public.app_document_versions (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id uuid NOT NULL,
    document_id uuid NOT NULL,
    version bigint NOT NULL CHECK (version > 0),
    display_name text NOT NULL CHECK (length(display_name) BETWEEN 1 AND 255),
    media_type text NOT NULL CHECK (length(media_type) BETWEEN 1 AND 127),
    size_bytes bigint NOT NULL CHECK (size_bytes BETWEEN 0 AND 5242880),
    sha256 bytea NOT NULL CHECK (octet_length(sha256) = 32),
    content bytea NOT NULL CHECK (octet_length(content) = size_bytes),
    created_by uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, document_id, version),
    FOREIGN KEY (tenant_id, application_id, document_id)
        REFERENCES public.app_documents (tenant_id, application_id, id)
);

CREATE TABLE public.app_document_acl (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id uuid NOT NULL,
    document_id uuid NOT NULL,
    principal_id uuid NOT NULL,
    permission text NOT NULL CHECK (permission IN ('read', 'write', 'share')),
    granted_by uuid NOT NULL,
    granted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    revoked_at timestamptz,
    PRIMARY KEY (tenant_id, application_id, document_id, principal_id, permission),
    FOREIGN KEY (tenant_id, application_id, document_id)
        REFERENCES public.app_documents (tenant_id, application_id, id)
);

CREATE TABLE public.app_document_download_tokens (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id uuid NOT NULL,
    token_hash bytea NOT NULL CHECK (octet_length(token_hash) = 32),
    document_id uuid NOT NULL,
    version bigint NOT NULL CHECK (version > 0),
    principal_id uuid NOT NULL,
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    used_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, token_hash),
    FOREIGN KEY (tenant_id, application_id, document_id, version)
        REFERENCES public.app_document_versions (tenant_id, application_id, document_id, version)
);

CREATE TABLE public.app_document_outbox (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id uuid NOT NULL,
    id uuid NOT NULL,
    document_id uuid NOT NULL,
    document_version bigint NOT NULL CHECK (document_version > 0),
    effect_kind text NOT NULL CHECK (effect_kind IN ('scan', 'transform', 'extract')),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    state text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'running', 'succeeded', 'failed', 'unknown')),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, id),
    FOREIGN KEY (tenant_id, application_id, document_id, document_version)
        REFERENCES public.app_document_versions (tenant_id, application_id, document_id, version)
);

CREATE TABLE public.app_document_extractions (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id uuid NOT NULL,
    job_id uuid NOT NULL,
    document_id uuid NOT NULL,
    document_version bigint NOT NULL CHECK (document_version > 0),
    source_sha256 text NOT NULL CHECK (source_sha256 ~ '^[0-9a-fA-F]{64}$'),
    provider text NOT NULL CHECK (provider IN ('synthetic-fixture', 'configured-adapter')),
    extracted_text text NOT NULL CHECK (octet_length(extracted_text) <= 1048576),
    created_by uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, job_id),
    FOREIGN KEY (tenant_id, application_id, job_id)
        REFERENCES public.app_document_outbox (tenant_id, application_id, id),
    FOREIGN KEY (tenant_id, application_id, document_id, document_version)
        REFERENCES public.app_document_versions (tenant_id, application_id, document_id, version)
);

CREATE TABLE public.app_document_derivatives (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id uuid NOT NULL,
    document_id uuid NOT NULL,
    source_version bigint NOT NULL CHECK (source_version > 0),
    width integer NOT NULL CHECK (width BETWEEN 1 AND 1024),
    height integer NOT NULL CHECK (height BETWEEN 1 AND 1024),
    format text NOT NULL CHECK (format IN ('png', 'jpeg')),
    sha256 bytea NOT NULL CHECK (octet_length(sha256) = 32),
    content bytea NOT NULL CHECK (octet_length(content) BETWEEN 1 AND 8388608),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, document_id, source_version, width, height, format),
    FOREIGN KEY (tenant_id, application_id, document_id, source_version)
        REFERENCES public.app_document_versions (tenant_id, application_id, document_id, version)
);
CREATE TABLE public.app_document_templates (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id uuid NOT NULL,
    document_id uuid NOT NULL,
    version bigint NOT NULL CHECK (version > 0),
    source text NOT NULL CHECK (octet_length(source) <= 65536),
    approved boolean NOT NULL DEFAULT false,
    updated_by uuid NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, document_id, version),
    FOREIGN KEY (tenant_id, application_id, document_id)
        REFERENCES public.app_documents (tenant_id, application_id, id)
);

CREATE TABLE public.app_editorial_revisions (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id uuid NOT NULL,
    document_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    title text NOT NULL CHECK (octet_length(title) <= 512),
    body text NOT NULL CHECK (octet_length(body) <= 131072),
    sha256 bytea NOT NULL CHECK (octet_length(sha256) = 32),
    created_by uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, document_id, revision),
    FOREIGN KEY (tenant_id, application_id, document_id)
        REFERENCES public.app_documents (tenant_id, application_id, id)
);

CREATE TABLE public.app_document_taxonomies (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id uuid NOT NULL,
    id uuid NOT NULL,
    parent_id uuid,
    label text NOT NULL CHECK (length(label) BETWEEN 1 AND 128),
    created_by uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, id),
    UNIQUE (tenant_id, application_id, id),
    FOREIGN KEY (tenant_id, application_id, parent_id)
        REFERENCES public.app_document_taxonomies (tenant_id, application_id, id),
    CHECK (parent_id IS NULL OR parent_id <> id)
);

CREATE INDEX app_document_versions_by_document
    ON public.app_document_versions (tenant_id, application_id, document_id, version DESC);
CREATE INDEX app_document_acl_active
    ON public.app_document_acl (tenant_id, application_id, document_id, principal_id, permission)
    WHERE revoked_at IS NULL;
CREATE INDEX app_document_tokens_expiry
    ON public.app_document_download_tokens (tenant_id, application_id, expires_at)
    WHERE revoked_at IS NULL AND used_at IS NULL;
CREATE INDEX app_document_outbox_pending
    ON public.app_document_outbox (created_at, tenant_id)
    WHERE state = 'pending';
CREATE INDEX app_editorial_latest
    ON public.app_editorial_revisions (tenant_id, application_id, document_id, revision DESC);
CREATE INDEX app_document_taxonomy_parent
    ON public.app_document_taxonomies (tenant_id, application_id, parent_id);

CREATE FUNCTION public.reject_immutable_document_row_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'immutable document history';
END
$$;

CREATE TRIGGER app_document_versions_immutable
    BEFORE UPDATE OR DELETE ON public.app_document_versions
    FOR EACH ROW EXECUTE FUNCTION public.reject_immutable_document_row_update();
CREATE TRIGGER app_editorial_revisions_immutable
    BEFORE UPDATE OR DELETE ON public.app_editorial_revisions
    FOR EACH ROW EXECUTE FUNCTION public.reject_immutable_document_row_update();
CREATE TRIGGER app_document_extractions_immutable
    BEFORE UPDATE OR DELETE ON public.app_document_extractions
    FOR EACH ROW EXECUTE FUNCTION public.reject_immutable_document_row_update();
CREATE TRIGGER app_document_derivatives_immutable
    BEFORE UPDATE OR DELETE ON public.app_document_derivatives
    FOR EACH ROW EXECUTE FUNCTION public.reject_immutable_document_row_update();

DO $$
DECLARE
    table_name text;
BEGIN
    FOREACH table_name IN ARRAY ARRAY[
        'app_documents', 'app_document_usage', 'app_document_versions',
        'app_document_acl', 'app_document_download_tokens', 'app_document_outbox',
        'app_document_extractions', 'app_document_derivatives', 'app_document_templates', 'app_editorial_revisions', 'app_document_taxonomies'
    ] LOOP
        EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY', table_name);
        EXECUTE format('ALTER TABLE public.%I FORCE ROW LEVEL SECURITY', table_name);
        EXECUTE format(
            'CREATE POLICY %I ON public.%I TO kyro_app USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id()) WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id())',
            table_name || '_tenant', table_name
        );
        EXECUTE format('GRANT SELECT, INSERT, UPDATE ON public.%I TO kyro_app', table_name);
    END LOOP;
END
$$;

REVOKE DELETE ON public.app_document_versions, public.app_editorial_revisions FROM kyro_app;