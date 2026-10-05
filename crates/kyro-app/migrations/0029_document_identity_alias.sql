-- PostgreSQL RETURNS TABLE names are PL/pgSQL variables. Qualify every
-- membership column instead of relying on a variable-conflict setting.
CREATE OR REPLACE FUNCTION app_document_job_identity(job_id uuid,job_lease uuid,job_generation bigint)
 RETURNS TABLE(principal_id uuid,session_id uuid,token_hash bytea,expires_at timestamptz)
 LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM public.app_memberships m WHERE m.tenant_id=public.kyro_app_tenant_id()
  AND m.application_id=public.kyro_app_application_id() AND m.principal_id=public.kyro_app_actor_id()
  AND m.role='documents.processor' AND m.status='active') THEN
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
