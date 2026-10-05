CREATE POLICY expired_upload_owner ON app_document_uploads TO CURRENT_USER
 USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id())
 WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id());
CREATE POLICY expired_upload_usage_owner ON app_document_usage TO CURRENT_USER
 USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id())
 WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id());
CREATE FUNCTION app_document_expire_uploads() RETURNS bigint LANGUAGE plpgsql SECURITY DEFINER
 SET search_path=pg_catalog,public AS $$
DECLARE t uuid:=public.kyro_app_tenant_id(); a uuid:=public.kyro_app_application_id(); n bigint; bytes bigint;
BEGIN
 IF NOT EXISTS(SELECT 1 FROM public.app_sessions s JOIN public.app_memberships m
  ON m.tenant_id=s.tenant_id AND m.application_id=s.application_id AND m.principal_id=s.principal_id
  WHERE s.tenant_id=t AND s.application_id=a AND s.id=public.kyro_app_session_id()
  AND s.principal_id=public.kyro_app_actor_id() AND s.revoked_at IS NULL AND s.expires_at>clock_timestamp()
  AND m.status='active' AND m.role IN ('documents.write','documents.admin')) THEN
  RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='document writer required';
 END IF;
 INSERT INTO public.app_document_usage(tenant_id,application_id) VALUES(t,a) ON CONFLICT DO NOTHING;
 PERFORM 1 FROM public.app_document_usage WHERE tenant_id=t AND application_id=a FOR UPDATE;
 WITH d AS(DELETE FROM public.app_document_uploads WHERE tenant_id=t AND application_id=a
  AND expires_at<=clock_timestamp() RETURNING expected_size)
 SELECT count(*),COALESCE(sum(expected_size),0) INTO n,bytes FROM d;
 UPDATE public.app_document_usage SET reserved_files=reserved_files-n,reserved_bytes=reserved_bytes-bytes
  WHERE tenant_id=t AND application_id=a;
 RETURN n;
END $$;
REVOKE ALL ON FUNCTION app_document_expire_uploads() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app_document_expire_uploads() TO kyro_app;
