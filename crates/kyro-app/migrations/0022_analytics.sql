CREATE TABLE app_analytics_definitions (
 tenant_id uuid NOT NULL,application_id uuid NOT NULL DEFAULT kyro_app_application_id(),id uuid NOT NULL,version bigint NOT NULL CHECK(version>0),
 kind text NOT NULL CHECK(kind IN ('collection','metric')),definition jsonb NOT NULL CHECK(octet_length(definition::text)<=32768),sha256 bytea NOT NULL CHECK(octet_length(sha256)=32),
 created_by uuid NOT NULL,created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id,version),FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id)
);
ALTER TABLE app_analytics_definitions ENABLE ROW LEVEL SECURITY;ALTER TABLE app_analytics_definitions FORCE ROW LEVEL SECURITY;
CREATE POLICY app_scope ON app_analytics_definitions TO kyro_app USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND created_by=kyro_app_actor_id());
GRANT SELECT,INSERT ON app_analytics_definitions TO kyro_app;
CREATE TABLE app_analytics_facts (
 tenant_id uuid NOT NULL,application_id uuid NOT NULL DEFAULT kyro_app_application_id(),principal_id uuid NOT NULL DEFAULT kyro_app_actor_id(),id uuid NOT NULL,
 collection_id uuid NOT NULL,collection_version bigint NOT NULL,payload jsonb NOT NULL CHECK(octet_length(payload::text)<=4096),
 occurred_at timestamptz NOT NULL DEFAULT clock_timestamp(),expires_at timestamptz NOT NULL,source jsonb,
 PRIMARY KEY(tenant_id,application_id,id),FOREIGN KEY(tenant_id,application_id,collection_id,collection_version) REFERENCES app_analytics_definitions(tenant_id,application_id,id,version)
);
CREATE INDEX analytics_facts_period ON app_analytics_facts(tenant_id,application_id,principal_id,collection_id,collection_version,occurred_at,id);
CREATE TABLE app_analytics_snapshots (
 tenant_id uuid NOT NULL,application_id uuid NOT NULL DEFAULT kyro_app_application_id(),principal_id uuid NOT NULL DEFAULT kyro_app_actor_id(),id uuid NOT NULL,
 metric_id uuid NOT NULL,metric_version bigint NOT NULL,definition_hash bytea NOT NULL CHECK(octet_length(definition_hash)=32),comparability_hash bytea NOT NULL CHECK(octet_length(comparability_hash)=32),
 period jsonb NOT NULL,result jsonb NOT NULL CHECK(octet_length(result::text)<=1048576),bindings jsonb NOT NULL CHECK(octet_length(bindings::text)<=4194304),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),expires_at timestamptz NOT NULL DEFAULT clock_timestamp()+interval '90 days',
 PRIMARY KEY(tenant_id,application_id,id),FOREIGN KEY(tenant_id,application_id,metric_id,metric_version) REFERENCES app_analytics_definitions(tenant_id,application_id,id,version)
);
CREATE TABLE app_analytics_alerts (
 tenant_id uuid NOT NULL,application_id uuid NOT NULL DEFAULT kyro_app_application_id(),principal_id uuid NOT NULL DEFAULT kyro_app_actor_id(),id uuid NOT NULL,
 rule_id uuid NOT NULL,rule_version bigint NOT NULL,bucket bigint NOT NULL,result jsonb NOT NULL,bindings jsonb NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),expires_at timestamptz NOT NULL DEFAULT clock_timestamp()+interval '30 days',
 PRIMARY KEY(tenant_id,application_id,id),UNIQUE(tenant_id,application_id,principal_id,rule_id,rule_version,bucket)
);
CREATE TABLE app_analytics_exports (
 tenant_id uuid NOT NULL,application_id uuid NOT NULL DEFAULT kyro_app_application_id(),principal_id uuid NOT NULL DEFAULT kyro_app_actor_id(),id uuid NOT NULL,
 specification jsonb NOT NULL,source_rows jsonb NOT NULL CHECK(octet_length(source_rows::text)<=4194304),bindings jsonb NOT NULL CHECK(octet_length(bindings::text)<=4194304),snapshot_hash bytea NOT NULL CHECK(octet_length(snapshot_hash)=32),
 state text NOT NULL DEFAULT 'captured' CHECK(state IN ('captured','processing','ready','expired','cancelled')),processed integer NOT NULL DEFAULT 0 CHECK(processed>=0),
 artifact bytea NOT NULL DEFAULT ''::bytea CHECK(octet_length(artifact)<=2097152),artifact_hash bytea,download_hash bytea,download_expires timestamptz,
 reserved_bytes bigint NOT NULL CHECK(reserved_bytes BETWEEN 1 AND 2097152),created_at timestamptz NOT NULL DEFAULT clock_timestamp(),expires_at timestamptz NOT NULL DEFAULT clock_timestamp()+interval '24 hours',job_id uuid,
 PRIMARY KEY(tenant_id,application_id,id),CHECK((state='ready')=(artifact_hash IS NOT NULL)),CHECK(artifact_hash IS NULL OR octet_length(artifact_hash)=32)
);
CREATE TABLE app_analytics_reports (
 tenant_id uuid NOT NULL,application_id uuid NOT NULL DEFAULT kyro_app_application_id(),principal_id uuid NOT NULL DEFAULT kyro_app_actor_id(),recipient_id uuid NOT NULL,id uuid NOT NULL,
 metric_id uuid NOT NULL,metric_version bigint NOT NULL,period jsonb NOT NULL,due_at timestamptz NOT NULL,
 state text NOT NULL DEFAULT 'scheduled' CHECK(state IN ('scheduled','ready','cancelled')),result jsonb,bindings jsonb,
 produced_at timestamptz,expires_at timestamptz NOT NULL DEFAULT clock_timestamp()+interval '7 days',job_id uuid,
 PRIMARY KEY(tenant_id,application_id,id),FOREIGN KEY(tenant_id,application_id,metric_id,metric_version) REFERENCES app_analytics_definitions(tenant_id,application_id,id,version)
);
DO $$ DECLARE t text; BEGIN
 FOREACH t IN ARRAY ARRAY['app_analytics_facts','app_analytics_snapshots','app_analytics_alerts','app_analytics_exports','app_analytics_reports'] LOOP
  EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY',t);EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY',t);
  EXECUTE format('CREATE POLICY private_scope ON %I TO kyro_app USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id())',t);
  EXECUTE format('GRANT SELECT,INSERT,UPDATE,DELETE ON %I TO kyro_app',t);
 END LOOP;
END $$;
CREATE POLICY recipient_report ON app_analytics_reports FOR SELECT TO kyro_app USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND recipient_id=kyro_app_actor_id() AND state='ready');
REVOKE UPDATE ON app_analytics_facts,app_analytics_snapshots,app_analytics_alerts FROM kyro_app;
CREATE FUNCTION app_analytics_immutable_export() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,public AS $$ BEGIN
 IF NEW.specification IS DISTINCT FROM OLD.specification OR NEW.source_rows IS DISTINCT FROM OLD.source_rows OR NEW.bindings IS DISTINCT FROM OLD.bindings OR NEW.snapshot_hash IS DISTINCT FROM OLD.snapshot_hash OR NEW.principal_id<>OLD.principal_id OR NEW.tenant_id<>OLD.tenant_id OR NEW.application_id<>OLD.application_id OR NEW.id<>OLD.id OR NEW.reserved_bytes<>OLD.reserved_bytes OR NEW.created_at<>OLD.created_at OR NEW.expires_at<>OLD.expires_at OR (OLD.job_id IS NOT NULL AND NEW.job_id IS DISTINCT FROM OLD.job_id) THEN RAISE EXCEPTION USING ERRCODE='23514',MESSAGE='immutable export snapshot'; END IF;
 RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION app_analytics_immutable_export() FROM PUBLIC;
CREATE TRIGGER immutable_export BEFORE UPDATE ON app_analytics_exports FOR EACH ROW EXECUTE FUNCTION app_analytics_immutable_export();
CREATE TABLE app_analytics_quota_ledger (
 sequence bigint GENERATED ALWAYS AS IDENTITY,tenant_id uuid NOT NULL,application_id uuid NOT NULL,principal_id uuid,
 quota_key text NOT NULL,reserved_delta bigint NOT NULL,used_delta bigint NOT NULL,limit_value bigint NOT NULL,
 reserved_after bigint NOT NULL,used_after bigint NOT NULL,recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(sequence),FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id)
);
ALTER TABLE app_analytics_quota_ledger ENABLE ROW LEVEL SECURITY;ALTER TABLE app_analytics_quota_ledger FORCE ROW LEVEL SECURITY;
CREATE POLICY own_ledger ON app_analytics_quota_ledger TO kyro_app USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id());
CREATE POLICY ledger_writer ON app_analytics_quota_ledger TO CURRENT_USER USING(true) WITH CHECK(true);
GRANT SELECT ON app_analytics_quota_ledger TO kyro_app;
CREATE FUNCTION app_analytics_ledger_write() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $$ BEGIN
 IF TG_OP='INSERT' OR NEW.reserved_value<>OLD.reserved_value OR NEW.used_value<>OLD.used_value OR NEW.limit_value<>OLD.limit_value THEN
  INSERT INTO public.app_analytics_quota_ledger(tenant_id,application_id,principal_id,quota_key,reserved_delta,used_delta,limit_value,reserved_after,used_after)
  VALUES(NEW.tenant_id,NEW.application_id,public.kyro_app_actor_id(),NEW.quota_key,NEW.reserved_value-COALESCE(OLD.reserved_value,0),NEW.used_value-COALESCE(OLD.used_value,0),NEW.limit_value,NEW.reserved_value,NEW.used_value);
 END IF; RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION app_analytics_ledger_write() FROM PUBLIC;
CREATE TRIGGER usage_ledger AFTER INSERT OR UPDATE ON app_quotas FOR EACH ROW EXECUTE FUNCTION app_analytics_ledger_write();
