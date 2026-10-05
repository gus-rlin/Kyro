CREATE TABLE app_auth_delivery_usage (
 tenant_id uuid NOT NULL,
 application_id uuid NOT NULL,
 window_kind text NOT NULL CHECK(window_kind IN ('minute','day')),
 window_start timestamptz NOT NULL,
 attempts integer NOT NULL CHECK(attempts BETWEEN 1 AND 10000),
 PRIMARY KEY(tenant_id,application_id,window_kind,window_start),
 FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id)
);
ALTER TABLE app_auth_delivery_usage ENABLE ROW LEVEL SECURITY;
ALTER TABLE app_auth_delivery_usage FORCE ROW LEVEL SECURITY;
CREATE POLICY auth_scope ON app_auth_delivery_usage TO kyro_app_auth
 USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id())
 WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id());
GRANT SELECT,INSERT,UPDATE,DELETE ON app_auth_delivery_usage TO kyro_app_auth;
