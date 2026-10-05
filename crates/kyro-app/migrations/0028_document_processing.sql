ALTER TABLE app_document_outbox ADD COLUMN origin_principal_id uuid DEFAULT kyro_app_actor_id();
ALTER TABLE app_document_outbox ADD COLUMN origin_session_id uuid DEFAULT kyro_app_session_id();
ALTER TABLE app_document_outbox ADD COLUMN generation bigint NOT NULL DEFAULT 0 CHECK(generation>=0);
ALTER TABLE app_document_outbox ADD COLUMN attempts integer NOT NULL DEFAULT 0 CHECK(attempts BETWEEN 0 AND 3);
ALTER TABLE app_document_outbox ADD COLUMN lease_id uuid;
ALTER TABLE app_document_outbox ADD COLUMN lease_owner uuid;
ALTER TABLE app_document_outbox ADD COLUMN lease_until timestamptz;
ALTER TABLE app_document_outbox ADD COLUMN deadline timestamptz NOT NULL DEFAULT clock_timestamp()+interval '10 minutes';
ALTER TABLE app_document_outbox ADD COLUMN error_code text;
ALTER TABLE app_document_outbox ADD COLUMN receipt jsonb CHECK(receipt IS NULL OR octet_length(receipt::text)<=8192);
ALTER TABLE app_document_outbox ADD COLUMN completed_at timestamptz;
ALTER TABLE app_document_outbox ADD FOREIGN KEY(tenant_id,origin_principal_id) REFERENCES app_principals(tenant_id,id);
ALTER TABLE app_document_outbox ADD FOREIGN KEY(tenant_id,application_id,origin_session_id) REFERENCES app_sessions(tenant_id,application_id,id);
-- An old queued row has no trustworthy session provenance. Never invent one.
UPDATE app_document_outbox SET state='failed',error_code='legacy_origin_unavailable'
 WHERE state IN ('pending','running');
ALTER TABLE app_document_outbox ADD CHECK((state='running')=(lease_id IS NOT NULL AND lease_owner IS NOT NULL AND lease_until IS NOT NULL));
ALTER TABLE app_document_outbox ADD CHECK((origin_principal_id IS NULL)=(origin_session_id IS NULL));
ALTER TABLE app_document_extractions ADD COLUMN provenance jsonb NOT NULL DEFAULT '{}'
 CHECK(octet_length(provenance::text)<=16384);
CREATE POLICY document_job_owner ON app_document_outbox TO CURRENT_USER
 USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id())
 WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id());
CREATE FUNCTION app_document_job_identity(job_id uuid,job_lease uuid,job_generation bigint)
 RETURNS TABLE(principal_id uuid,session_id uuid,token_hash bytea,expires_at timestamptz)
 LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM public.app_memberships WHERE tenant_id=public.kyro_app_tenant_id()
  AND application_id=public.kyro_app_application_id() AND principal_id=public.kyro_app_actor_id()
  AND role='documents.processor' AND status='active') THEN
  RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='document processor required';
 END IF;
 RETURN QUERY SELECT s.principal_id,s.id,s.token_hash,s.expires_at FROM public.app_document_outbox j
 JOIN public.app_sessions s ON s.tenant_id=j.tenant_id AND s.application_id=j.application_id
  AND s.id=j.origin_session_id AND s.principal_id=j.origin_principal_id
 WHERE j.tenant_id=public.kyro_app_tenant_id() AND j.application_id=public.kyro_app_application_id()
  AND j.id=job_id AND j.state='running' AND j.lease_id=job_lease AND j.generation=job_generation
  AND j.lease_owner=public.kyro_app_actor_id() AND j.lease_until>clock_timestamp()
  AND j.deadline>clock_timestamp() AND s.revoked_at IS NULL AND s.expires_at>clock_timestamp();
END $$;
REVOKE ALL ON FUNCTION app_document_job_identity(uuid,uuid,bigint) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app_document_job_identity(uuid,uuid,bigint) TO kyro_app;
