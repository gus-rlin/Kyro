CREATE FUNCTION app_identity_key_admission() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,public AS $f$
BEGIN
 -- The fixed key-issue routine already checked identity and requested scopes.
 -- This lock also bounds concurrent inserts for different service principals.
 PERFORM pg_advisory_xact_lock(hashtextextended('key-admission:'||NEW.tenant_id::text||':'||NEW.application_id::text,0));
 IF (SELECT count(*) FROM public.app_api_keys WHERE tenant_id=NEW.tenant_id AND application_id=NEW.application_id AND revoked_at IS NULL AND expires_at>clock_timestamp())>=10000 OR (SELECT count(*) FROM public.app_api_keys WHERE tenant_id=NEW.tenant_id AND application_id=NEW.application_id AND principal_id=NEW.principal_id AND revoked_at IS NULL AND expires_at>clock_timestamp())>=32 THEN RAISE EXCEPTION USING ERRCODE='54000',MESSAGE='API key admission limit';END IF;
 RETURN NEW;
END $f$;
REVOKE ALL ON FUNCTION app_identity_key_admission() FROM PUBLIC;
CREATE TRIGGER app_identity_key_admission BEFORE INSERT ON app_api_keys FOR EACH ROW EXECUTE FUNCTION app_identity_key_admission();
