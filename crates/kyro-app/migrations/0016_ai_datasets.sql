CREATE TABLE app_ai_datasets (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), principal_id uuid NOT NULL,
 id uuid NOT NULL, version bigint NOT NULL CHECK(version>0), definition jsonb NOT NULL CHECK(octet_length(definition::text)<=65536),
 definition_hash bytea NOT NULL CHECK(octet_length(definition_hash)=32), created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,principal_id,id,version), FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id)
);
ALTER TABLE app_ai_datasets ENABLE ROW LEVEL SECURITY;
ALTER TABLE app_ai_datasets FORCE ROW LEVEL SECURITY;
CREATE POLICY scoped ON app_ai_datasets USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id());
GRANT SELECT,INSERT ON app_ai_datasets TO kyro_app;
