-- External snapshots share B040's validation/checkpoints; provenance and input
-- remain immutable for the entire preview, processing and completed lifecycle.
ALTER TABLE app_data_imports ADD COLUMN source_provenance jsonb
 CHECK(source_provenance IS NULL OR (jsonb_typeof(source_provenance)='object' AND octet_length(source_provenance::text)<=4096));
DROP POLICY app_data_imports_tenant ON app_data_imports;
CREATE POLICY own_import ON app_data_imports TO kyro_app
 USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND created_by=kyro_app_actor_id())
 WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND created_by=kyro_app_actor_id());
CREATE FUNCTION app_import_immutable_input() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,public AS $$
BEGIN
 IF NEW.tenant_id IS DISTINCT FROM OLD.tenant_id OR NEW.application_id IS DISTINCT FROM OLD.application_id OR NEW.import_id IS DISTINCT FROM OLD.import_id
 OR NEW.component_id IS DISTINCT FROM OLD.component_id OR NEW.entity_kind IS DISTINCT FROM OLD.entity_kind OR NEW.schema_version IS DISTINCT FROM OLD.schema_version
 OR NEW.schema_hash IS DISTINCT FROM OLD.schema_hash OR NEW.payload_hash IS DISTINCT FROM OLD.payload_hash OR NEW.payload IS DISTINCT FROM OLD.payload
 OR NEW.row_count IS DISTINCT FROM OLD.row_count OR NEW.created_by IS DISTINCT FROM OLD.created_by OR NEW.source_provenance IS DISTINCT FROM OLD.source_provenance
 OR NEW.created_at IS DISTINCT FROM OLD.created_at OR NEW.expires_at IS DISTINCT FROM OLD.expires_at THEN
  RAISE EXCEPTION USING ERRCODE='23514',MESSAGE='immutable import input';
 END IF;
 RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION app_import_immutable_input() FROM PUBLIC;
CREATE TRIGGER immutable_import_input BEFORE UPDATE ON app_data_imports FOR EACH ROW EXECUTE FUNCTION app_import_immutable_input();
