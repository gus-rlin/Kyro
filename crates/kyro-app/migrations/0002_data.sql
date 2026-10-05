-- Declarative, tenant-scoped persistence for catalogue blocks B031–B040.
-- Runtime roles can store validated schema data; none of these tables execute
-- client-provided SQL or create request-defined database objects.

CREATE TABLE public.app_data_schemas (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id UUID NOT NULL REFERENCES public.app_tenants(id) ON DELETE CASCADE,
    component_id TEXT NOT NULL CHECK (component_id = 'data'),
    entity_kind TEXT NOT NULL CHECK (entity_kind ~ '^[a-z][a-z0-9_]{0,63}$'),
    schema_version BIGINT NOT NULL CHECK (schema_version > 0),
    schema_hash BYTEA NOT NULL CHECK (octet_length(schema_hash) = 32),
    definition JSONB NOT NULL CHECK (
        jsonb_typeof(definition) = 'object'
        AND octet_length(definition::TEXT) <= 65536
    ),
    created_by UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, component_id, entity_kind, schema_version),
    FOREIGN KEY (tenant_id, created_by)
        REFERENCES public.app_principals (tenant_id, id) ON DELETE RESTRICT
);

CREATE TABLE public.app_data_unique_values (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id UUID NOT NULL REFERENCES public.app_tenants(id) ON DELETE CASCADE,
    component_id TEXT NOT NULL CHECK (component_id = 'data'),
    entity_kind TEXT NOT NULL CHECK (entity_kind ~ '^[a-z][a-z0-9_]{0,63}$'),
    record_kind TEXT NOT NULL CHECK (length(record_kind) BETWEEN 1 AND 128),
    field_name TEXT NOT NULL CHECK (field_name ~ '^[a-z][a-z0-9_]{0,63}$'),
    field_value JSONB NOT NULL CHECK (field_value <> 'null'::JSONB),
    record_id UUID NOT NULL,
    PRIMARY KEY (tenant_id, application_id, component_id, record_kind, field_name, field_value),
    FOREIGN KEY (tenant_id, application_id, record_kind, record_id)
        REFERENCES public.app_records (tenant_id, application_id, kind, id) ON DELETE CASCADE
);

CREATE TABLE public.app_data_relationships (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id UUID NOT NULL REFERENCES public.app_tenants(id) ON DELETE CASCADE,
    component_id TEXT NOT NULL CHECK (component_id = 'data'),
    relationship_name TEXT NOT NULL CHECK (relationship_name ~ '^[a-z][a-z0-9_]{0,63}$'),
    source_kind TEXT NOT NULL CHECK (length(source_kind) BETWEEN 1 AND 128),
    source_id UUID NOT NULL,
    target_kind TEXT NOT NULL CHECK (length(target_kind) BETWEEN 1 AND 128),
    target_id UUID NOT NULL,
    cardinality TEXT NOT NULL CHECK (cardinality IN ('one_to_one', 'one_to_many', 'many_to_many')),
    on_target_delete TEXT NOT NULL CHECK (on_target_delete IN ('restrict', 'detach', 'cascade')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, component_id, relationship_name, source_kind, source_id, target_kind, target_id),
    FOREIGN KEY (tenant_id, application_id, source_kind, source_id)
        REFERENCES public.app_records (tenant_id, application_id, kind, id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id, application_id, target_kind, target_id)
        REFERENCES public.app_records (tenant_id, application_id, kind, id) ON DELETE RESTRICT
);
CREATE INDEX app_data_relationships_source_idx
    ON public.app_data_relationships (tenant_id, application_id, component_id, relationship_name, source_kind, source_id, target_id);
CREATE INDEX app_data_relationships_target_idx
    ON public.app_data_relationships (tenant_id, application_id, component_id, relationship_name, target_kind, target_id, source_kind, source_id);
CREATE UNIQUE INDEX app_data_relationships_one_to_one_source_idx
    ON public.app_data_relationships (tenant_id, application_id, component_id, relationship_name, source_kind, source_id)
    WHERE cardinality = 'one_to_one';
CREATE UNIQUE INDEX app_data_relationships_one_to_one_target_idx
    ON public.app_data_relationships (tenant_id, application_id, component_id, relationship_name, target_kind, target_id)
    WHERE cardinality = 'one_to_one';
CREATE UNIQUE INDEX app_data_relationships_one_to_many_target_idx
    ON public.app_data_relationships (tenant_id, application_id, component_id, relationship_name, target_kind, target_id)
    WHERE cardinality = 'one_to_many';

CREATE TABLE public.app_data_drafts (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id UUID NOT NULL REFERENCES public.app_tenants(id) ON DELETE CASCADE,
    record_kind TEXT NOT NULL CHECK (length(record_kind) BETWEEN 1 AND 128),
    record_id UUID NOT NULL,
    base_version BIGINT NOT NULL CHECK (base_version > 0),
    draft_revision BIGINT NOT NULL CHECK (draft_revision > 0),
    content JSONB NOT NULL CHECK (
        jsonb_typeof(content) = 'object'
        AND octet_length(content::TEXT) <= 65536
    ),
    updated_by UUID NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, record_kind, record_id),
    FOREIGN KEY (tenant_id, application_id, record_kind, record_id)
        REFERENCES public.app_records (tenant_id, application_id, kind, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, updated_by)
        REFERENCES public.app_principals (tenant_id, id) ON DELETE RESTRICT
);

CREATE TABLE public.app_data_imports (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id UUID NOT NULL REFERENCES public.app_tenants(id) ON DELETE CASCADE,
    component_id TEXT NOT NULL CHECK (component_id = 'data'),
    import_id UUID NOT NULL,
    entity_kind TEXT NOT NULL CHECK (entity_kind ~ '^[a-z][a-z0-9_]{0,63}$'),
    schema_version BIGINT NOT NULL CHECK (schema_version > 0),
    schema_hash BYTEA NOT NULL CHECK (octet_length(schema_hash) = 32),
    payload_hash BYTEA NOT NULL CHECK (octet_length(payload_hash) = 32),
    state TEXT NOT NULL CHECK (state IN ('preview', 'processing', 'invalid', 'completed')),
    payload JSONB NOT NULL CHECK (
        jsonb_typeof(payload) = 'array'
        AND octet_length(payload::TEXT) <= 4194304
    ),
    errors JSONB NOT NULL DEFAULT '[]'::JSONB CHECK (
        jsonb_typeof(errors) = 'array'
        AND octet_length(errors::TEXT) <= 1048576
    ),
    processed_count INTEGER NOT NULL DEFAULT 0 CHECK (processed_count BETWEEN 0 AND 5000),
    row_count INTEGER NOT NULL CHECK (row_count BETWEEN 0 AND 5000),
    reserved_units BIGINT NOT NULL DEFAULT 0 CHECK (reserved_units >= 0),
    result JSONB CHECK (result IS NULL OR octet_length(result::TEXT) <= 1048576),
    created_by UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT (clock_timestamp() + INTERVAL '24 hours'),
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, application_id, component_id, import_id),
    FOREIGN KEY (tenant_id, created_by)
        REFERENCES public.app_principals (tenant_id, id) ON DELETE RESTRICT,
    CHECK ((state = 'completed') = (completed_at IS NOT NULL)),
    CHECK ((state = 'completed') = (result IS NOT NULL)),
    CHECK ((state IN ('preview','processing')) = (reserved_units > 0))
);
CREATE INDEX app_data_imports_expiry_idx
    ON public.app_data_imports (tenant_id, application_id, expires_at) WHERE state = 'preview';

CREATE TABLE public.app_data_cache_epochs (
    application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id),
    tenant_id UUID NOT NULL REFERENCES public.app_tenants(id) ON DELETE CASCADE,
    component_id TEXT NOT NULL CHECK (component_id = 'data'),
    entity_kind TEXT NOT NULL CHECK (entity_kind ~ '^[a-z][a-z0-9_]{0,63}$'),
    epoch BIGINT NOT NULL CHECK (epoch >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant_id, application_id, component_id, entity_kind)
);

DO $policies$
DECLARE
    table_name TEXT;
BEGIN
    FOREACH table_name IN ARRAY ARRAY[
        'app_data_schemas', 'app_data_unique_values', 'app_data_relationships',
        'app_data_drafts', 'app_data_imports', 'app_data_cache_epochs'
    ] LOOP
        EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY', table_name);
        EXECUTE format('ALTER TABLE public.%I FORCE ROW LEVEL SECURITY', table_name);
        EXECUTE format(
            'CREATE POLICY %I ON public.%I TO kyro_app USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id()) WITH CHECK (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id())',
            table_name || '_tenant', table_name
        );
    END LOOP;
END
$policies$;

GRANT SELECT, INSERT ON public.app_data_schemas TO kyro_app;
GRANT SELECT, INSERT, DELETE ON public.app_data_unique_values TO kyro_app;
GRANT SELECT, INSERT, DELETE ON public.app_data_relationships TO kyro_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON public.app_data_drafts TO kyro_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON public.app_data_imports TO kyro_app;
GRANT SELECT, INSERT, UPDATE ON public.app_data_cache_epochs TO kyro_app;
