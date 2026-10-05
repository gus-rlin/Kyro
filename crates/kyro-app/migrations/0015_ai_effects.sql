ALTER TABLE app_jobs ADD COLUMN deadline timestamptz NOT NULL DEFAULT (clock_timestamp()+interval '10 minutes');
CREATE TABLE app_ai_requests (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), principal_id uuid NOT NULL, id uuid NOT NULL,
 component_id text NOT NULL CHECK(component_id IN ('B092','B094','B095','B096','B097','B099','B100')),
 operation text NOT NULL, specification jsonb NOT NULL CHECK(octet_length(specification::text)<=65536),
 source_bindings jsonb NOT NULL CHECK(octet_length(source_bindings::text)<=16384), configuration_hash bytea NOT NULL CHECK(octet_length(configuration_hash)=32),
 job_id uuid, state text NOT NULL DEFAULT 'queued' CHECK(state IN ('queued','completed','unknown','failed','cancelled')),
 result jsonb CHECK(octet_length(result::text)<=65536), decision text CHECK(decision IN ('accepted','rejected','applied')),
 correction jsonb CHECK(octet_length(correction::text)<=32768), decision_version bigint NOT NULL DEFAULT 0, decided_at timestamptz,
 expires_at timestamptz NOT NULL, created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,principal_id,id), UNIQUE(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id),
 FOREIGN KEY(tenant_id,application_id,job_id) REFERENCES app_jobs(tenant_id,application_id,id)
);
CREATE TABLE app_ai_effects (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), principal_id uuid NOT NULL,
 id uuid NOT NULL, request_id uuid NOT NULL, call_key text NOT NULL CHECK(length(call_key) BETWEEN 1 AND 128),
 generation bigint NOT NULL CHECK(generation>0), intent jsonb NOT NULL CHECK(octet_length(intent::text)<=16384),
 fingerprint bytea NOT NULL CHECK(octet_length(fingerprint)=32), status text NOT NULL CHECK(status IN ('prepared','sending','succeeded','unknown','failed','cancelled')),
 reserved_units bigint NOT NULL CHECK(reserved_units>0), reserved_tokens bigint NOT NULL CHECK(reserved_tokens>0),
 reservation_status text NOT NULL DEFAULT 'held' CHECK(reservation_status IN ('held','settled','released')),
 actual_units bigint CHECK(actual_units>=0), response jsonb CHECK(octet_length(response::text)<=65536), failure_code text,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id), UNIQUE(tenant_id,application_id,request_id,call_key),
 FOREIGN KEY(tenant_id,application_id,principal_id,request_id) REFERENCES app_ai_requests(tenant_id,application_id,principal_id,id)
);
DO $$ DECLARE tbl text; BEGIN
 FOREACH tbl IN ARRAY ARRAY['app_ai_requests','app_ai_effects'] LOOP
  EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY',tbl);
  EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY',tbl);
  EXECUTE format('CREATE POLICY scoped ON %I USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id())',tbl);
  EXECUTE format('GRANT SELECT,INSERT,UPDATE ON %I TO kyro_app',tbl);
 END LOOP;
END $$;
