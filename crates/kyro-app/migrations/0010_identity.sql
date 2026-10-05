-- Public authentication has its own login and scoped role. It is never assumed by
-- the application command pool, a client-selected actor or an application worker.
DO $roles$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM pg_roles WHERE rolname='kyro_app_auth') THEN CREATE ROLE kyro_app_auth NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;END IF;
 IF NOT EXISTS(SELECT 1 FROM pg_roles WHERE rolname='kyro_app_auth_runtime') THEN CREATE ROLE kyro_app_auth_runtime LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;END IF;
 ALTER ROLE kyro_app_auth WITH NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
 ALTER ROLE kyro_app_auth_runtime WITH LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
 GRANT kyro_app_auth TO kyro_app_auth_runtime;
END $roles$;
GRANT USAGE ON SCHEMA public TO kyro_app_auth;
GRANT EXECUTE ON FUNCTION kyro_app_tenant_id(),kyro_app_application_id(),kyro_app_actor_id(),kyro_app_session_id() TO kyro_app_auth;

ALTER TABLE app_sessions ADD COLUMN api_key_id uuid;
ALTER TABLE app_sessions ADD FOREIGN KEY(tenant_id,application_id,api_key_id) REFERENCES app_api_keys(tenant_id,application_id,id);
ALTER TABLE app_principals ADD COLUMN account_type text NOT NULL DEFAULT 'human' CHECK(account_type IN ('human','service'));
ALTER TABLE app_one_time_credentials DROP CONSTRAINT app_one_time_credentials_purpose_check;
ALTER TABLE app_one_time_credentials ADD CHECK(purpose IN ('magic_link','recovery','verify_email'));
ALTER TABLE app_role_permissions ADD COLUMN definition_version bigint NOT NULL DEFAULT 1 CHECK(definition_version>0);
CREATE INDEX app_sessions_key_idx ON app_sessions(tenant_id,application_id,api_key_id) WHERE api_key_id IS NOT NULL;

CREATE TABLE app_local_credentials (
 tenant_id uuid NOT NULL,application_id uuid NOT NULL,principal_id uuid NOT NULL,
 email text NOT NULL CHECK(length(email) BETWEEN 3 AND 320 AND email=lower(email)),
 email_verified boolean NOT NULL DEFAULT false,password_hash text NOT NULL CHECK(length(password_hash) BETWEEN 40 AND 512),
 version bigint NOT NULL DEFAULT 1 CHECK(version>0),updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,principal_id),UNIQUE(tenant_id,application_id,email),
 FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id),FOREIGN KEY(tenant_id,principal_id) REFERENCES app_principals(tenant_id,id)
);
CREATE TABLE app_mfa_credentials (
 tenant_id uuid NOT NULL,application_id uuid NOT NULL,principal_id uuid NOT NULL,
 secret_cipher bytea NOT NULL CHECK(octet_length(secret_cipher) BETWEEN 48 AND 128),
 confirmed boolean NOT NULL DEFAULT false,last_step bigint NOT NULL DEFAULT -1,created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,principal_id),FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id),FOREIGN KEY(tenant_id,principal_id) REFERENCES app_principals(tenant_id,id)
);
CREATE TABLE app_auth_deliveries (
 tenant_id uuid NOT NULL,application_id uuid NOT NULL,id uuid NOT NULL,principal_id uuid NOT NULL,
 purpose text NOT NULL CHECK(purpose IN ('magic_link','recovery','verify_email')),content_cipher bytea NOT NULL CHECK(octet_length(content_cipher)<=4096),
 state text NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','sending','sent','failed','unknown')),
 expires_at timestamptz NOT NULL,created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id),FOREIGN KEY(tenant_id,application_id,id) REFERENCES app_one_time_credentials(tenant_id,application_id,id),
 CHECK(expires_at>created_at AND expires_at<=created_at+interval '15 minutes')
);
CREATE TABLE app_auth_rate_buckets (
 tenant_id uuid NOT NULL,application_id uuid NOT NULL,bucket text NOT NULL CHECK(length(bucket)<=80),
 window_start timestamptz NOT NULL,count integer NOT NULL CHECK(count BETWEEN 1 AND 300),
 PRIMARY KEY(tenant_id,application_id,bucket,window_start),FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id)
);

DO $policies$
DECLARE n text;
BEGIN
 FOREACH n IN ARRAY ARRAY['app_local_credentials','app_mfa_credentials','app_auth_deliveries','app_auth_rate_buckets'] LOOP
  EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY',n);
  EXECUTE format('ALTER TABLE public.%I FORCE ROW LEVEL SECURITY',n);
 END LOOP;
 FOREACH n IN ARRAY ARRAY['app_principals','app_memberships','app_role_permissions','app_sessions','app_oidc_flows','app_external_identities','app_one_time_credentials','app_api_keys','app_local_credentials','app_mfa_credentials','app_auth_deliveries','app_auth_rate_buckets'] LOOP
  EXECUTE format('CREATE POLICY auth_scope ON public.%I TO kyro_app_auth USING(tenant_id=public.kyro_app_tenant_id() AND %s) WITH CHECK(tenant_id=public.kyro_app_tenant_id() AND %s)',n,CASE WHEN n='app_principals' THEN 'true' ELSE 'application_id=public.kyro_app_application_id()' END,CASE WHEN n='app_principals' THEN 'true' ELSE 'application_id=public.kyro_app_application_id()' END);
 END LOOP;
 CREATE POLICY auth_tenant_scope ON app_tenants FOR SELECT TO kyro_app_auth USING(id=kyro_app_tenant_id());
 CREATE POLICY auth_application_scope ON app_applications FOR SELECT TO kyro_app_auth USING(tenant_id=kyro_app_tenant_id() AND id=kyro_app_application_id());
 FOREACH n IN ARRAY ARRAY['app_local_credentials','app_mfa_credentials'] LOOP
  EXECUTE format('CREATE POLICY credential_self_scope ON public.%I TO kyro_app USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id())',n);
 END LOOP;
 -- Fixed SECURITY DEFINER routines still obey application scope with a
 -- non-superuser migration owner; runtime roles cannot become this owner.
 FOREACH n IN ARRAY ARRAY['app_tenants','app_applications','app_principals','app_memberships','app_role_permissions','app_sessions','app_api_keys','app_local_credentials','app_mfa_credentials'] LOOP
  EXECUTE format('CREATE POLICY identity_owner_scope ON public.%I TO %I USING(%s) WITH CHECK(%s)',n,current_user,CASE WHEN n='app_tenants' THEN 'id=kyro_app_tenant_id()' WHEN n='app_applications' THEN 'tenant_id=kyro_app_tenant_id() AND id=kyro_app_application_id()' WHEN n='app_principals' THEN 'tenant_id=kyro_app_tenant_id()' ELSE 'tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id()' END,CASE WHEN n='app_tenants' THEN 'id=kyro_app_tenant_id()' WHEN n='app_applications' THEN 'tenant_id=kyro_app_tenant_id() AND id=kyro_app_application_id()' WHEN n='app_principals' THEN 'tenant_id=kyro_app_tenant_id()' ELSE 'tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id()' END);
 END LOOP;
END $policies$;
GRANT SELECT ON app_tenants,app_applications,app_principals,app_memberships,app_role_permissions,app_api_keys TO kyro_app_auth;
GRANT INSERT ON app_principals,app_memberships TO kyro_app_auth;
GRANT UPDATE(display_name) ON app_principals TO kyro_app_auth;
GRANT SELECT,INSERT,UPDATE ON app_sessions,app_external_identities,app_one_time_credentials,app_local_credentials,app_mfa_credentials,app_auth_deliveries TO kyro_app_auth;
GRANT SELECT,INSERT,DELETE ON app_oidc_flows TO kyro_app_auth;
GRANT SELECT,INSERT,UPDATE,DELETE ON app_auth_rate_buckets TO kyro_app_auth;
GRANT SELECT,INSERT,UPDATE ON app_local_credentials,app_mfa_credentials TO kyro_app;
GRANT DELETE ON app_mfa_credentials TO kyro_app;
CREATE POLICY delivery_self_scope ON app_auth_deliveries FOR INSERT TO kyro_app WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id());
GRANT INSERT ON app_auth_deliveries TO kyro_app;

CREATE FUNCTION app_identity_revoke(target uuid,include_keys boolean) RETURNS integer LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $f$
DECLARE t uuid:=public.kyro_app_tenant_id();a uuid:=public.kyro_app_application_id();p uuid:=public.kyro_app_actor_id();n integer;
BEGIN
 IF NOT EXISTS(SELECT 1 FROM public.app_sessions s JOIN public.app_principals u ON u.tenant_id=s.tenant_id AND u.id=s.principal_id AND u.status='active' WHERE s.tenant_id=t AND s.application_id=a AND s.id=public.kyro_app_session_id() AND s.principal_id=p AND s.revoked_at IS NULL AND s.expires_at>clock_timestamp() AND (target=p OR (s.api_key_id IS NULL AND s.mfa_at>clock_timestamp()-interval '5 minutes' AND EXISTS(SELECT 1 FROM public.app_memberships m WHERE m.tenant_id=t AND m.application_id=a AND m.principal_id=p AND m.role IN ('owner','admin','security.admin') AND m.status='active')))) THEN RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='verified identity required';END IF;
 UPDATE public.app_sessions SET revoked_at=clock_timestamp() WHERE tenant_id=t AND application_id=a AND principal_id=target AND revoked_at IS NULL AND (include_keys OR api_key_id IS NULL);GET DIAGNOSTICS n=ROW_COUNT;
 IF include_keys THEN UPDATE public.app_api_keys SET revoked_at=clock_timestamp() WHERE tenant_id=t AND application_id=a AND principal_id=target AND revoked_at IS NULL;END IF;
 RETURN n;
END $f$;
REVOKE ALL ON FUNCTION app_identity_revoke(uuid,boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app_identity_revoke(uuid,boolean) TO kyro_app;

-- Authorization changes are a closed routine. Self membership changes are
-- refused, and definition replacement uses a version checked under one lock.
CREATE FUNCTION app_identity_role(role_name text,permissions text[],expected bigint,target uuid,member_status text) RETURNS bigint LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $f$
DECLARE t uuid:=public.kyro_app_tenant_id();a uuid:=public.kyro_app_application_id();p uuid:=public.kyro_app_actor_id();v bigint;per text;
BEGIN
 IF role_name !~ '^[a-z][a-z0-9._-]{0,63}$' OR role_name IN ('owner','admin','security.admin') OR (target IS NOT NULL AND target=p) OR cardinality(permissions)>64 THEN RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='invalid role change';END IF;
 IF NOT EXISTS(SELECT 1 FROM public.app_sessions s WHERE s.tenant_id=t AND s.application_id=a AND s.id=public.kyro_app_session_id() AND s.principal_id=p AND s.api_key_id IS NULL AND s.revoked_at IS NULL AND s.expires_at>clock_timestamp() AND s.mfa_at>clock_timestamp()-interval '5 minutes') OR NOT EXISTS(SELECT 1 FROM public.app_memberships m WHERE m.tenant_id=t AND m.application_id=a AND m.principal_id=p AND m.role IN ('owner','admin','security.admin') AND m.status='active') THEN RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='recent administrator authentication required';END IF;
 PERFORM pg_advisory_xact_lock(hashtextextended('identity-role:'||t::text||':'||a::text||':'||role_name,0));
 SELECT COALESCE(max(definition_version),0) INTO v FROM public.app_role_permissions WHERE tenant_id=t AND application_id=a AND role=role_name;
 IF v<>expected THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='stale role definition';END IF;
 IF target IS NULL THEN
  IF cardinality(permissions)=0 THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='empty role';END IF;
  FOREACH per IN ARRAY permissions LOOP
   IF per !~ '^B[0-9]{3}\.execute$' OR NOT EXISTS(SELECT 1 FROM public.app_memberships m JOIN public.app_role_permissions rp ON rp.tenant_id=m.tenant_id AND rp.application_id=m.application_id AND rp.role=m.role WHERE m.tenant_id=t AND m.application_id=a AND m.principal_id=p AND m.status='active' AND rp.permission IN ('*',per)) THEN RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='permission escalation refused';END IF;
  END LOOP;
  DELETE FROM public.app_role_permissions WHERE tenant_id=t AND application_id=a AND role=role_name;
  INSERT INTO public.app_role_permissions(tenant_id,application_id,role,permission,definition_version) SELECT t,a,role_name,items.permission,v+1 FROM unnest(permissions) AS items(permission);
  RETURN v+1;
 ELSE
  IF v=0 OR member_status NOT IN ('active','suspended','revoked') OR NOT EXISTS(SELECT 1 FROM public.app_memberships m WHERE m.tenant_id=t AND m.application_id=a AND m.principal_id=target) THEN RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='existing application member required';END IF;
  INSERT INTO public.app_memberships(tenant_id,application_id,principal_id,role,status) VALUES(t,a,target,role_name,member_status) ON CONFLICT(tenant_id,application_id,principal_id,role) DO UPDATE SET status=EXCLUDED.status;
  RETURN v;
 END IF;
END $f$;
REVOKE ALL ON FUNCTION app_identity_role(text,text[],bigint,uuid,text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app_identity_role(text,text[],bigint,uuid,text) TO kyro_app;

CREATE FUNCTION app_identity_key_revoke(key_id uuid) RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $f$
DECLARE t uuid:=public.kyro_app_tenant_id();a uuid:=public.kyro_app_application_id();p uuid:=public.kyro_app_actor_id();target uuid;
BEGIN
 SELECT principal_id INTO target FROM public.app_api_keys WHERE tenant_id=t AND application_id=a AND id=key_id FOR UPDATE;
 IF target IS NULL OR NOT EXISTS(SELECT 1 FROM public.app_sessions s WHERE s.tenant_id=t AND s.application_id=a AND s.id=public.kyro_app_session_id() AND s.principal_id=p AND s.revoked_at IS NULL AND s.expires_at>clock_timestamp() AND (target=p OR (s.api_key_id IS NULL AND s.mfa_at>clock_timestamp()-interval '5 minutes' AND EXISTS(SELECT 1 FROM public.app_memberships m WHERE m.tenant_id=t AND m.application_id=a AND m.principal_id=p AND m.status='active' AND m.role IN ('admin','owner','security.admin'))))) THEN RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='verified key owner required';END IF;
 UPDATE public.app_api_keys SET revoked_at=clock_timestamp() WHERE tenant_id=t AND application_id=a AND id=key_id;
 UPDATE public.app_sessions SET revoked_at=clock_timestamp() WHERE tenant_id=t AND application_id=a AND api_key_id=key_id AND revoked_at IS NULL;
END $f$;
REVOKE ALL ON FUNCTION app_identity_key_revoke(uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app_identity_key_revoke(uuid) TO kyro_app;

CREATE FUNCTION app_identity_key_issue(key_id uuid,target uuid,key_hash bytea,scopes text[],expiry timestamptz) RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $f$
DECLARE t uuid:=public.kyro_app_tenant_id();a uuid:=public.kyro_app_application_id();p uuid:=public.kyro_app_actor_id();scope text;component text;
BEGIN
 IF cardinality(scopes) NOT BETWEEN 1 AND 64 OR octet_length(key_hash)<>32 OR expiry<=clock_timestamp() OR expiry>clock_timestamp()+interval '90 days' THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='invalid key limits';END IF;
 IF NOT EXISTS(SELECT 1 FROM public.app_sessions s WHERE s.tenant_id=t AND s.application_id=a AND s.id=public.kyro_app_session_id() AND s.principal_id=p AND s.api_key_id IS NULL AND s.revoked_at IS NULL AND s.expires_at>clock_timestamp() AND s.mfa_at>clock_timestamp()-interval '5 minutes') OR NOT EXISTS(SELECT 1 FROM public.app_principals u JOIN public.app_memberships m ON m.tenant_id=u.tenant_id AND m.principal_id=u.id AND m.application_id=a AND m.status='active' WHERE u.tenant_id=t AND u.id=target AND u.status='active' AND (target=p OR (u.account_type='service' AND EXISTS(SELECT 1 FROM public.app_memberships adm WHERE adm.tenant_id=t AND adm.application_id=a AND adm.principal_id=p AND adm.role IN ('owner','admin','security.admin') AND adm.status='active')))) THEN RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='recent owner or service administrator required';END IF;
 FOREACH scope IN ARRAY scopes LOOP
  IF scope !~ '^B[0-9]{3}:[a-z][a-z0-9._-]{0,127}$' THEN RAISE EXCEPTION USING ERRCODE='22023',MESSAGE='invalid scope';END IF;
  component:=split_part(scope,':',1)||'.execute';
  IF NOT EXISTS(SELECT 1 FROM public.app_memberships m JOIN public.app_role_permissions rp ON rp.tenant_id=m.tenant_id AND rp.application_id=m.application_id AND rp.role=m.role WHERE m.tenant_id=t AND m.application_id=a AND m.principal_id=target AND m.status='active' AND rp.permission IN ('*',component)) OR NOT EXISTS(SELECT 1 FROM public.app_memberships m JOIN public.app_role_permissions rp ON rp.tenant_id=m.tenant_id AND rp.application_id=m.application_id AND rp.role=m.role WHERE m.tenant_id=t AND m.application_id=a AND m.principal_id=p AND m.status='active' AND rp.permission IN ('*',component)) THEN RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='scope escalation refused';END IF;
 END LOOP;
 INSERT INTO public.app_api_keys(tenant_id,application_id,id,principal_id,token_hash,scope_ids,expires_at) VALUES(t,a,key_id,target,key_hash,scopes,expiry);
 INSERT INTO public.app_sessions(tenant_id,application_id,id,principal_id,token_hash,csrf_hash,expires_at,api_key_id) VALUES(t,a,key_id,target,key_hash,decode(repeat('00',32),'hex'),expiry,key_id);
END $f$;
REVOKE ALL ON FUNCTION app_identity_key_issue(uuid,uuid,bytea,text[],timestamptz) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app_identity_key_issue(uuid,uuid,bytea,text[],timestamptz) TO kyro_app;
