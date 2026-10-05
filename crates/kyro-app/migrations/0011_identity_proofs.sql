CREATE TABLE app_mfa_backup_codes(
 tenant_id uuid NOT NULL,application_id uuid NOT NULL,principal_id uuid NOT NULL,id uuid NOT NULL,
 token_hash bytea NOT NULL CHECK(octet_length(token_hash)=32),consumed_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),PRIMARY KEY(tenant_id,application_id,principal_id,id),
 UNIQUE(tenant_id,application_id,principal_id,token_hash),
 FOREIGN KEY(tenant_id,application_id,principal_id) REFERENCES app_mfa_credentials(tenant_id,application_id,principal_id) ON DELETE CASCADE
);
ALTER TABLE app_mfa_backup_codes ENABLE ROW LEVEL SECURITY;
ALTER TABLE app_mfa_backup_codes FORCE ROW LEVEL SECURITY;
CREATE POLICY mfa_self ON app_mfa_backup_codes TO kyro_app USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id());
GRANT SELECT,INSERT,UPDATE,DELETE ON app_mfa_backup_codes TO kyro_app;
GRANT UPDATE(revoked_at) ON app_api_keys TO kyro_app_auth;
CREATE POLICY auth_audit_scope ON app_events TO kyro_app_auth USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id());
GRANT SELECT,INSERT ON app_events TO kyro_app_auth;
GRANT USAGE,SELECT ON SEQUENCE app_events_sequence_seq TO kyro_app_auth;
