ALTER TABLE app_outbox ADD COLUMN source_session_id uuid DEFAULT kyro_app_session_id();
ALTER TABLE app_outbox ADD COLUMN adapter_version bigint, ADD COLUMN secret_reference_version bigint;
ALTER TABLE app_outbox ADD FOREIGN KEY(tenant_id,application_id,source_session_id) REFERENCES app_sessions(tenant_id,application_id,id);
CREATE TABLE app_connector_calls (
 tenant_id uuid NOT NULL,application_id uuid NOT NULL DEFAULT kyro_app_application_id(),principal_id uuid NOT NULL,id uuid NOT NULL,
 adapter_id uuid NOT NULL,component_id text NOT NULL,operation text NOT NULL,profile_hash bytea NOT NULL CHECK(octet_length(profile_hash)=32),
 request_cipher bytea NOT NULL CHECK(octet_length(request_cipher) BETWEEN 28 AND 65564),
 state text NOT NULL DEFAULT 'queued' CHECK(state IN ('queued','delivered','failed','unknown','cancelled')),
 result jsonb, error_code text, outbox_id uuid, reserved_units bigint NOT NULL CHECK(reserved_units>=0),
 estimated_units bigint,invoice_verified boolean NOT NULL DEFAULT false CHECK(NOT invoice_verified),
 expires_at timestamptz NOT NULL DEFAULT clock_timestamp()+interval '30 days',created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id),FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id),
 FOREIGN KEY(tenant_id,application_id,outbox_id) REFERENCES app_outbox(tenant_id,application_id,id),
 CHECK(result IS NULL OR octet_length(result::text)<=1048576)
);
ALTER TABLE app_connector_calls ENABLE ROW LEVEL SECURITY; ALTER TABLE app_connector_calls FORCE ROW LEVEL SECURITY;
CREATE POLICY scoped ON app_connector_calls USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id());
GRANT SELECT,INSERT,UPDATE,DELETE ON app_connector_calls TO kyro_app;
CREATE FUNCTION app_outbox_identity(outbox_id uuid,outbox_lease uuid,outbox_generation bigint)
 RETURNS TABLE(principal_id uuid,session_id uuid,token_hash bytea,expires_at timestamptz) LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM public.app_memberships m WHERE m.tenant_id=public.kyro_app_tenant_id() AND m.application_id=public.kyro_app_application_id() AND m.principal_id=public.kyro_app_actor_id() AND m.role='jobs.worker' AND m.status='active') THEN RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='worker required';END IF;
 RETURN QUERY SELECT s.principal_id,s.id,s.token_hash,s.expires_at FROM public.app_outbox o JOIN public.app_sessions s ON s.tenant_id=o.tenant_id AND s.application_id=o.application_id AND s.id=o.source_session_id
 WHERE o.tenant_id=public.kyro_app_tenant_id() AND o.application_id=public.kyro_app_application_id() AND o.id=outbox_id AND o.state='claimed' AND o.lease_id=outbox_lease AND o.generation=outbox_generation AND o.lease_owner=public.kyro_app_actor_id() AND o.lease_until>clock_timestamp() AND s.revoked_at IS NULL AND s.expires_at>clock_timestamp();
END $$;
REVOKE ALL ON FUNCTION app_outbox_identity(uuid,uuid,bigint) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app_outbox_identity(uuid,uuid,bigint) TO kyro_app;
CREATE FUNCTION app_verified_email(recipient uuid) RETURNS text LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,public AS $$
 SELECT c.email FROM public.app_local_credentials c JOIN public.app_principals p ON p.tenant_id=c.tenant_id AND p.id=c.principal_id
 WHERE c.tenant_id=public.kyro_app_tenant_id() AND c.application_id=public.kyro_app_application_id() AND c.principal_id=recipient AND c.email_verified AND p.status='active' AND p.account_type='human'
 AND EXISTS(SELECT 1 FROM public.app_memberships m WHERE m.tenant_id=c.tenant_id AND m.application_id=c.application_id AND m.principal_id=recipient AND m.status='active')
 AND EXISTS(SELECT 1 FROM public.app_sessions s WHERE s.tenant_id=c.tenant_id AND s.application_id=c.application_id AND s.id=public.kyro_app_session_id() AND s.principal_id=public.kyro_app_actor_id() AND s.revoked_at IS NULL AND s.expires_at>clock_timestamp())
$$;
REVOKE ALL ON FUNCTION app_verified_email(uuid) FROM PUBLIC;GRANT EXECUTE ON FUNCTION app_verified_email(uuid) TO kyro_app;
CREATE TABLE app_delivery_endpoints (
 tenant_id uuid NOT NULL,application_id uuid NOT NULL DEFAULT kyro_app_application_id(),principal_id uuid NOT NULL,id uuid NOT NULL,
 channel text NOT NULL CHECK(channel IN ('mobile','push')),destination_cipher bytea NOT NULL CHECK(octet_length(destination_cipher) BETWEEN 28 AND 4096),
 version bigint NOT NULL DEFAULT 1 CHECK(version>0),verified boolean NOT NULL DEFAULT false,revoked boolean NOT NULL DEFAULT false,
 challenge_hash bytea CHECK(challenge_hash IS NULL OR octet_length(challenge_hash)=32),challenge_expires timestamptz,challenge_attempts integer NOT NULL DEFAULT 0 CHECK(challenge_attempts BETWEEN 0 AND 5),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),PRIMARY KEY(tenant_id,application_id,id),FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id)
);
ALTER TABLE app_delivery_endpoints ENABLE ROW LEVEL SECURITY;ALTER TABLE app_delivery_endpoints FORCE ROW LEVEL SECURITY;
CREATE POLICY endpoint_own ON app_delivery_endpoints USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id());
CREATE POLICY endpoint_owner ON app_delivery_endpoints TO CURRENT_USER USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id());
GRANT SELECT,INSERT,UPDATE,DELETE ON app_delivery_endpoints TO kyro_app;
CREATE TRIGGER authority_fence BEFORE UPDATE OR DELETE ON app_delivery_endpoints FOR EACH STATEMENT EXECUTE FUNCTION app_authority_fence();
CREATE FUNCTION app_delivery_endpoint(recipient uuid,endpoint uuid) RETURNS TABLE(id uuid,version bigint,destination_cipher bytea,channel text) LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,public AS $$
 SELECT e.id,e.version,e.destination_cipher,e.channel FROM public.app_delivery_endpoints e WHERE e.tenant_id=public.kyro_app_tenant_id() AND e.application_id=public.kyro_app_application_id() AND e.principal_id=recipient AND e.id=endpoint AND e.verified AND NOT e.revoked
 AND EXISTS(SELECT 1 FROM public.app_sessions s WHERE s.id=public.kyro_app_session_id() AND s.tenant_id=e.tenant_id AND s.application_id=e.application_id AND s.principal_id=public.kyro_app_actor_id() AND s.revoked_at IS NULL AND s.expires_at>clock_timestamp())
$$;
REVOKE ALL ON FUNCTION app_delivery_endpoint(uuid,uuid) FROM PUBLIC;GRANT EXECUTE ON FUNCTION app_delivery_endpoint(uuid,uuid) TO kyro_app;
CREATE POLICY connector_owner ON app_connector_calls TO CURRENT_USER USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id());
CREATE POLICY connector_quota_owner ON app_quotas TO CURRENT_USER USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id());
CREATE FUNCTION app_connector_outcome() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $$
BEGIN
 IF NEW.state='unknown' AND OLD.state<>'unknown' THEN
  UPDATE public.app_connector_calls SET state='unknown',error_code='delivery_or_authority_unknown' WHERE tenant_id=NEW.tenant_id AND application_id=NEW.application_id AND outbox_id=NEW.id AND state='queued';
 END IF;
 RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION app_connector_outcome() FROM PUBLIC;
CREATE TRIGGER connector_outcome AFTER UPDATE OF state ON app_outbox FOR EACH ROW EXECUTE FUNCTION app_connector_outcome();
