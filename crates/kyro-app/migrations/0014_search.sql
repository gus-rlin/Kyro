CREATE TABLE app_search_sources (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), principal_id uuid NOT NULL,
 id uuid NOT NULL, source jsonb NOT NULL CHECK(octet_length(source::text)<=1024), source_hash bytea NOT NULL CHECK(octet_length(source_hash)=32),
 state text NOT NULL CHECK(state IN ('building','ready')), chunk_count integer NOT NULL CHECK(chunk_count BETWEEN 1 AND 256),
 expires_at timestamptz NOT NULL, created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,principal_id,id), FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id),
 FOREIGN KEY(tenant_id,principal_id) REFERENCES app_principals(tenant_id,id)
);
CREATE TABLE app_search_chunks (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), principal_id uuid NOT NULL,
 source_id uuid NOT NULL, ordinal integer NOT NULL CHECK(ordinal BETWEEN 0 AND 255),
 content text NOT NULL CHECK(octet_length(content)<=8192), content_hash bytea NOT NULL CHECK(octet_length(content_hash)=32),
 search_vector tsvector GENERATED ALWAYS AS (to_tsvector('simple',content)) STORED,
 embedding double precision[], embedding_registration jsonb,
 PRIMARY KEY(tenant_id,application_id,principal_id,source_id,ordinal),
 FOREIGN KEY(tenant_id,application_id,principal_id,source_id) REFERENCES app_search_sources(tenant_id,application_id,principal_id,id) ON DELETE CASCADE,
 CHECK ((embedding IS NULL)=(embedding_registration IS NULL)), CHECK(embedding IS NULL OR cardinality(embedding) BETWEEN 2 AND 4096)
);
CREATE INDEX app_search_fulltext ON app_search_chunks USING gin(search_vector);
DO $$ DECLARE tbl text; BEGIN
 FOREACH tbl IN ARRAY ARRAY['app_search_sources','app_search_chunks'] LOOP
  EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY',tbl);
  EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY',tbl);
  EXECUTE format('CREATE POLICY scoped ON %I USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id())',tbl);
  EXECUTE format('GRANT SELECT,INSERT,UPDATE,DELETE ON %I TO kyro_app',tbl);
 END LOOP;
END $$;
CREATE TRIGGER authority_fence BEFORE INSERT OR UPDATE OR DELETE ON app_document_acl FOR EACH STATEMENT EXECUTE FUNCTION app_authority_fence();
-- Privacy changes to document grants use the same fence as identity revocation.
-- Source deletion erases each actor's derived index in the deletion transaction.
CREATE FUNCTION app_search_source_deleted() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $$
DECLARE removed bigint;
BEGIN
 WITH deleted AS (DELETE FROM public.app_search_sources WHERE tenant_id=OLD.tenant_id AND application_id=OLD.application_id
  AND source->>'id'=OLD.id::text
  AND ((TG_TABLE_NAME='app_records' AND source->>'kind'=OLD.kind) OR (TG_TABLE_NAME='app_documents' AND source->>'type' IN ('file','editorial'))) RETURNING chunk_count)
 SELECT COALESCE(sum(chunk_count),0) INTO removed FROM deleted;
 UPDATE public.app_quotas SET used_value=used_value-removed WHERE tenant_id=OLD.tenant_id AND application_id=OLD.application_id AND quota_key='search_chunks';
 RETURN OLD;
END $$;
REVOKE ALL ON FUNCTION app_search_source_deleted() FROM PUBLIC;
CREATE POLICY owner_cleanup ON app_search_sources TO CURRENT_USER USING(true) WITH CHECK(true);
CREATE POLICY owner_cleanup ON app_search_chunks TO CURRENT_USER USING(true) WITH CHECK(true);
CREATE POLICY search_cleanup_owner ON app_quotas TO CURRENT_USER USING(true) WITH CHECK(true);
CREATE TRIGGER search_cleanup BEFORE DELETE ON app_records FOR EACH ROW EXECUTE FUNCTION app_search_source_deleted();
CREATE TRIGGER search_cleanup BEFORE DELETE ON app_documents FOR EACH ROW EXECUTE FUNCTION app_search_source_deleted();
