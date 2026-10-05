-- Statement triggers acquire the fence before any tuple locks. Operator writes
-- without an application scope take the global fence; runtime writes are scoped.
CREATE FUNCTION app_authority_fence() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, public AS $$
DECLARE t uuid := public.kyro_app_tenant_id(); a uuid := public.kyro_app_application_id();
BEGIN
  IF t IS NULL OR a IS NULL OR TG_TABLE_NAME IN ('app_principals','app_tenants','app_applications') THEN
    PERFORM pg_advisory_xact_lock(hashtextextended('app-authority-global:v1',0));
  ELSE
    PERFORM pg_advisory_xact_lock_shared(hashtextextended('app-authority-global:v1',0));
    PERFORM pg_advisory_xact_lock(hashtextextended('app-authority:'||t::text||':'||a::text,0));
  END IF;
  RETURN NULL;
END $$;
REVOKE ALL ON FUNCTION app_authority_fence() FROM PUBLIC;
DO $$ DECLARE tbl text; BEGIN
  FOREACH tbl IN ARRAY ARRAY['app_sessions','app_api_keys','app_memberships','app_role_permissions',
    'app_local_credentials','app_mfa_credentials','app_mfa_backup_codes'] LOOP
    EXECUTE format('CREATE TRIGGER authority_fence BEFORE INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION app_authority_fence()',tbl);
  END LOOP;
END $$;
CREATE TRIGGER authority_fence BEFORE UPDATE OF status, account_type, tenant_id, id OR DELETE ON app_principals FOR EACH STATEMENT EXECUTE FUNCTION app_authority_fence();
CREATE TRIGGER authority_fence BEFORE UPDATE OF status OR DELETE ON app_tenants FOR EACH STATEMENT EXECUTE FUNCTION app_authority_fence();
CREATE TRIGGER authority_fence BEFORE UPDATE OF status OR DELETE ON app_applications FOR EACH STATEMENT EXECUTE FUNCTION app_authority_fence();
