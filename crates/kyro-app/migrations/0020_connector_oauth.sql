CREATE TABLE app_connector_oauth_flows (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), principal_id uuid NOT NULL,
 id uuid NOT NULL, adapter_id uuid NOT NULL, source_session_id uuid NOT NULL DEFAULT kyro_app_session_id(),
 profile_hash bytea NOT NULL CHECK(octet_length(profile_hash)=32),state_hash bytea NOT NULL CHECK(octet_length(state_hash)=32),
 content_cipher bytea NOT NULL CHECK(octet_length(content_cipher) BETWEEN 28 AND 16384),
 state text NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','queued','completed','unknown')),
 attempts integer NOT NULL DEFAULT 0 CHECK(attempts BETWEEN 0 AND 5),call_id uuid,
 expires_at timestamptz NOT NULL DEFAULT clock_timestamp()+interval '10 minutes',created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id),FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id),
 FOREIGN KEY(tenant_id,application_id,source_session_id) REFERENCES app_sessions(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,application_id,call_id) REFERENCES app_connector_calls(tenant_id,application_id,id)
);
CREATE TABLE app_connector_oauth_connections (
 tenant_id uuid NOT NULL,application_id uuid NOT NULL DEFAULT kyro_app_application_id(),principal_id uuid NOT NULL,
 id uuid NOT NULL,adapter_id uuid NOT NULL,profile_hash bytea NOT NULL CHECK(octet_length(profile_hash)=32),
 version bigint NOT NULL DEFAULT 1 CHECK(version>0),state text NOT NULL CHECK(state IN ('active','refreshing','revoking','revoked','unknown')),
 credential_cipher bytea CHECK(credential_cipher IS NULL OR octet_length(credential_cipher) BETWEEN 28 AND 16384),scopes text[] NOT NULL,
 expires_at timestamptz NOT NULL,call_id uuid,updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id),UNIQUE(tenant_id,application_id,principal_id,adapter_id),
 FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id),
 FOREIGN KEY(tenant_id,application_id,call_id) REFERENCES app_connector_calls(tenant_id,application_id,id)
);
ALTER TABLE app_connector_oauth_flows ENABLE ROW LEVEL SECURITY;ALTER TABLE app_connector_oauth_flows FORCE ROW LEVEL SECURITY;
ALTER TABLE app_connector_oauth_connections ENABLE ROW LEVEL SECURITY;ALTER TABLE app_connector_oauth_connections FORCE ROW LEVEL SECURITY;
CREATE POLICY own ON app_connector_oauth_flows USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id());
CREATE POLICY own ON app_connector_oauth_connections USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id());
CREATE POLICY owner ON app_connector_oauth_flows TO CURRENT_USER USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id());
CREATE POLICY owner ON app_connector_oauth_connections TO CURRENT_USER USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id());
GRANT SELECT,INSERT,UPDATE,DELETE ON app_connector_oauth_flows,app_connector_oauth_connections TO kyro_app;
CREATE TRIGGER authority_fence BEFORE UPDATE OR DELETE ON app_connector_oauth_connections FOR EACH STATEMENT EXECUTE FUNCTION app_authority_fence();
CREATE FUNCTION app_connector_oauth_unknown() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $$
BEGIN
 IF NEW.state='unknown' AND OLD.state<>'unknown' THEN
  UPDATE public.app_connector_oauth_flows SET state='unknown' WHERE tenant_id=NEW.tenant_id AND application_id=NEW.application_id AND call_id=NEW.id AND state='queued';
  UPDATE public.app_connector_oauth_connections SET state='unknown',updated_at=clock_timestamp() WHERE tenant_id=NEW.tenant_id AND application_id=NEW.application_id AND call_id=NEW.id AND state IN ('refreshing','revoking');
 END IF;
 RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION app_connector_oauth_unknown() FROM PUBLIC;
CREATE TRIGGER oauth_unknown AFTER UPDATE OF state ON app_connector_calls FOR EACH ROW EXECUTE FUNCTION app_connector_oauth_unknown();
