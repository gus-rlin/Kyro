CREATE TABLE app_jobs (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL,
 principal_id uuid NOT NULL, session_id uuid NOT NULL, specification jsonb NOT NULL CHECK(octet_length(specification::text)<=65536),
 state text NOT NULL DEFAULT 'queued' CHECK(state IN ('queued','leased','completed','failed','quarantined','cancelled')),
 available_at timestamptz NOT NULL DEFAULT clock_timestamp(), generation bigint NOT NULL DEFAULT 0 CHECK(generation>=0),
 attempts integer NOT NULL DEFAULT 0 CHECK(attempts BETWEEN 0 AND 5), max_attempts integer NOT NULL DEFAULT 3 CHECK(max_attempts BETWEEN 1 AND 5),
 lease_id uuid, lease_owner uuid, lease_until timestamptz, result jsonb, error_code text, reservation_settled boolean NOT NULL DEFAULT false,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), completed_at timestamptz,
 PRIMARY KEY(tenant_id,application_id,id), FOREIGN KEY(tenant_id,application_id,session_id) REFERENCES app_sessions(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,principal_id) REFERENCES app_principals(tenant_id,id),
 CHECK((state='leased')=(lease_id IS NOT NULL AND lease_owner IS NOT NULL AND lease_until IS NOT NULL))
);
CREATE INDEX app_jobs_ready ON app_jobs(tenant_id,application_id,available_at,id) WHERE state='queued';
CREATE TABLE app_job_schedules (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, principal_id uuid NOT NULL,
 session_id uuid NOT NULL, specification jsonb NOT NULL, timezone text NOT NULL, next_at timestamptz NOT NULL, ends_at timestamptz NOT NULL,
 interval_seconds integer NOT NULL CHECK(interval_seconds BETWEEN 60 AND 2592000), missed_policy text NOT NULL CHECK(missed_policy IN ('skip','catch_up_once')),
 enabled boolean NOT NULL DEFAULT true, occurrences integer NOT NULL DEFAULT 0 CHECK(occurrences BETWEEN 0 AND 1000),
 PRIMARY KEY(tenant_id,application_id,id), FOREIGN KEY(tenant_id,application_id,session_id) REFERENCES app_sessions(tenant_id,application_id,id), CHECK(ends_at>next_at)
);
CREATE TABLE app_inbox (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), connector_id uuid NOT NULL, event_id text NOT NULL,
 body_hash bytea NOT NULL CHECK(octet_length(body_hash)=32), event uuid NOT NULL, received_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,connector_id,event_id), FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id)
);
ALTER TABLE app_outbox DROP CONSTRAINT app_outbox_state_check;
ALTER TABLE app_outbox ADD CHECK(state IN ('pending','claimed','delivered','failed','unknown','quarantined'));
ALTER TABLE app_outbox ADD COLUMN generation bigint NOT NULL DEFAULT 0;
ALTER TABLE app_outbox ADD COLUMN lease_id uuid;
ALTER TABLE app_outbox ADD COLUMN lease_owner uuid;
ALTER TABLE app_outbox ADD COLUMN lease_until timestamptz;
ALTER TABLE app_outbox ADD COLUMN receipt jsonb;
ALTER TABLE app_outbox ADD CHECK(generation>=0 AND attempts BETWEEN 0 AND 5);
GRANT UPDATE ON app_outbox TO kyro_app;
DO $policies$
DECLARE n text;
BEGIN
 FOREACH n IN ARRAY ARRAY['app_jobs','app_job_schedules','app_inbox'] LOOP
  EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY',n);
  EXECUTE format('ALTER TABLE public.%I FORCE ROW LEVEL SECURITY',n);
  EXECUTE format('CREATE POLICY app_scope ON public.%I TO kyro_app USING(tenant_id=public.kyro_app_tenant_id() AND application_id=public.kyro_app_application_id()) WITH CHECK(tenant_id=public.kyro_app_tenant_id() AND application_id=public.kyro_app_application_id())',n);
  EXECUTE format('CREATE POLICY owner_scope ON public.%I TO %I USING(tenant_id=public.kyro_app_tenant_id() AND application_id=public.kyro_app_application_id())',n,current_user);
  EXECUTE format('GRANT SELECT,INSERT,UPDATE ON public.%I TO kyro_app',n);
 END LOOP;
END $policies$;
REVOKE UPDATE ON app_inbox FROM kyro_app;
CREATE FUNCTION app_job_identity(job_id uuid,job_lease uuid,job_generation bigint)
 RETURNS TABLE(principal_id uuid,session_id uuid,token_hash bytea,expires_at timestamptz) LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $f$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM public.app_memberships WHERE tenant_id=public.kyro_app_tenant_id() AND application_id=public.kyro_app_application_id() AND principal_id=public.kyro_app_actor_id() AND role='jobs.worker' AND status='active') THEN RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='worker required';END IF;
 RETURN QUERY SELECT s.principal_id,s.id,s.token_hash,s.expires_at FROM public.app_jobs j JOIN public.app_sessions s ON s.tenant_id=j.tenant_id AND s.application_id=j.application_id AND s.id=j.session_id AND s.principal_id=j.principal_id
 WHERE j.tenant_id=public.kyro_app_tenant_id() AND j.application_id=public.kyro_app_application_id() AND j.id=job_id AND j.state='leased' AND j.lease_id=job_lease AND j.generation=job_generation AND j.lease_owner=public.kyro_app_actor_id() AND j.lease_until>clock_timestamp() AND s.revoked_at IS NULL AND s.expires_at>clock_timestamp();
END $f$;
REVOKE ALL ON FUNCTION app_job_identity(uuid,uuid,bigint) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app_job_identity(uuid,uuid,bigint) TO kyro_app;
