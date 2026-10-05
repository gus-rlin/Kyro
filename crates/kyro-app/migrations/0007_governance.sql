CREATE TABLE app_rate_buckets (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL, principal_id uuid NOT NULL, window_start timestamptz NOT NULL,
 count integer NOT NULL CHECK(count BETWEEN 0 AND 1200), PRIMARY KEY(tenant_id,application_id,principal_id,window_start),
 FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id)
);
ALTER TABLE app_rate_buckets ENABLE ROW LEVEL SECURITY;
ALTER TABLE app_rate_buckets FORCE ROW LEVEL SECURITY;
CREATE POLICY app_scope ON app_rate_buckets TO PUBLIC USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id());
CREATE FUNCTION app_consume_rate() RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $f$
DECLARE t uuid:=public.kyro_app_tenant_id(); a uuid:=public.kyro_app_application_id(); p uuid:=public.kyro_app_actor_id(); w timestamptz:=date_trunc('minute',clock_timestamp()); all_actors uuid:='00000000-0000-0000-0000-000000000000';
BEGIN
 IF t IS NULL OR a IS NULL OR p IS NULL THEN RETURN false; END IF;
 PERFORM pg_advisory_xact_lock(hashtextextended('rate:'||t::text||':'||a::text,0));
 DELETE FROM public.app_rate_buckets WHERE tenant_id=t AND application_id=a AND window_start<w;
 IF EXISTS(SELECT 1 FROM public.app_rate_buckets WHERE tenant_id=t AND application_id=a AND window_start=w AND ((principal_id=p AND count>=120) OR (principal_id=all_actors AND count>=1200))) THEN RETURN false; END IF;
 INSERT INTO public.app_rate_buckets VALUES(t,a,p,w,1),(t,a,all_actors,w,1)
 ON CONFLICT(tenant_id,application_id,principal_id,window_start) DO UPDATE SET count=app_rate_buckets.count+1;
 RETURN true;
END $f$;
REVOKE ALL ON FUNCTION app_consume_rate() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app_consume_rate() TO kyro_app;

ALTER TABLE app_events ADD COLUMN chain_index bigint;
ALTER TABLE app_events ADD COLUMN previous_hash bytea;
ALTER TABLE app_events ADD COLUMN entry_hash bytea;
-- Backfill preserves the prior events and records an initial checkpoint per application.
DO $backfill$
DECLARE r record; t uuid; a uuid; prev bytea; n bigint;
BEGIN
 FOR r IN SELECT * FROM app_events ORDER BY tenant_id,application_id,sequence LOOP
  IF t IS DISTINCT FROM r.tenant_id OR a IS DISTINCT FROM r.application_id THEN t:=r.tenant_id; a:=r.application_id;prev:=decode(repeat('00',32),'hex');n:=0;END IF;
  n:=n+1;
  prev:=sha256(convert_to(jsonb_build_object('id',r.id,'sequence',r.sequence,'tenant',r.tenant_id,'application',r.application_id,'actor',r.actor_principal_id,'component',r.component_id,'action',r.action,'resource',r.resource_id,'event_type',r.event_type,'payload',r.payload,'created_at',r.created_at,'previous',encode(prev,'hex'),'index',n)::text,'UTF8'));
  UPDATE app_events SET chain_index=n,previous_hash=COALESCE((SELECT entry_hash FROM app_events WHERE tenant_id=t AND application_id=a AND chain_index=n-1),decode(repeat('00',32),'hex')),entry_hash=prev WHERE tenant_id=r.tenant_id AND application_id=r.application_id AND id=r.id;
 END LOOP;
END $backfill$;
ALTER TABLE app_events ALTER COLUMN chain_index SET NOT NULL;
ALTER TABLE app_events ALTER COLUMN previous_hash SET NOT NULL;
ALTER TABLE app_events ALTER COLUMN entry_hash SET NOT NULL;
ALTER TABLE app_events ADD CONSTRAINT app_event_chain_unique UNIQUE(tenant_id,application_id,chain_index);
ALTER TABLE app_events ADD CHECK(chain_index>0 AND octet_length(previous_hash)=32 AND octet_length(entry_hash)=32);
CREATE FUNCTION app_event_chain() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,public SET TimeZone='UTC' AS $f$
BEGIN
 PERFORM pg_advisory_xact_lock(hashtextextended('audit:'||NEW.tenant_id::text||':'||NEW.application_id::text,0));
 SELECT chain_index+1,entry_hash INTO NEW.chain_index,NEW.previous_hash FROM public.app_events WHERE tenant_id=NEW.tenant_id AND application_id=NEW.application_id ORDER BY chain_index DESC LIMIT 1;
 NEW.chain_index:=COALESCE(NEW.chain_index,1);NEW.previous_hash:=COALESCE(NEW.previous_hash,decode(repeat('00',32),'hex'));
 NEW.entry_hash:=sha256(convert_to(jsonb_build_object('id',NEW.id,'sequence',NEW.sequence,'tenant',NEW.tenant_id,'application',NEW.application_id,'actor',NEW.actor_principal_id,'component',NEW.component_id,'action',NEW.action,'resource',NEW.resource_id,'event_type',NEW.event_type,'payload',NEW.payload,'created_at',NEW.created_at,'previous',encode(NEW.previous_hash,'hex'),'index',NEW.chain_index)::text,'UTF8'));
 RETURN NEW;
END $f$;
REVOKE ALL ON FUNCTION app_event_chain() FROM PUBLIC;
CREATE TRIGGER app_event_chain BEFORE INSERT ON app_events FOR EACH ROW EXECUTE FUNCTION app_event_chain();

CREATE TABLE app_private_exports (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL, id uuid NOT NULL, principal_id uuid NOT NULL,
 token_hash bytea NOT NULL CHECK(octet_length(token_hash)=32), expires_at timestamptz NOT NULL, content bytea NOT NULL CHECK(octet_length(content)<=1048576),
 downloaded_at timestamptz, created_at timestamptz NOT NULL DEFAULT clock_timestamp(), PRIMARY KEY(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id), FOREIGN KEY(tenant_id,principal_id) REFERENCES app_principals(tenant_id,id), CHECK(expires_at<=created_at+interval '15 minutes')
);
ALTER TABLE app_private_exports ENABLE ROW LEVEL SECURITY;
ALTER TABLE app_private_exports FORCE ROW LEVEL SECURITY;
CREATE POLICY app_scope ON app_private_exports TO kyro_app USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id());
GRANT SELECT,INSERT,UPDATE ON app_private_exports TO kyro_app;
GRANT INSERT ON app_quotas TO kyro_app;

CREATE FUNCTION app_purge_data_records(record_kind text,after_id uuid,page_size integer) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $f$
DECLARE t uuid:=public.kyro_app_tenant_id();a uuid:=public.kyro_app_application_id();p uuid:=public.kyro_app_actor_id();ids uuid[];days integer;after_value uuid;
BEGIN
 IF NOT EXISTS(SELECT 1 FROM public.app_memberships m JOIN public.app_sessions s ON s.tenant_id=m.tenant_id AND s.application_id=m.application_id AND s.principal_id=m.principal_id
  WHERE m.tenant_id=t AND m.application_id=a AND m.principal_id=p AND m.status='active' AND m.role IN ('owner','admin','security.admin') AND s.id=public.kyro_app_session_id() AND s.revoked_at IS NULL AND s.expires_at>clock_timestamp() AND s.mfa_at>clock_timestamp()-interval '5 minutes')
 THEN RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='purge denied';END IF;
 IF record_kind NOT LIKE 'data.%' OR page_size NOT BETWEEN 1 AND 100 THEN RAISE EXCEPTION USING ERRCODE='23514',MESSAGE='invalid purge scope';END IF;
 SELECT (data->>'days')::integer INTO days FROM public.app_records WHERE tenant_id=t AND application_id=a AND kind='security.retention' AND data->>'kind'=record_kind;
 IF days IS NULL OR days NOT BETWEEN 1 AND 3650 THEN RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='retention policy required';END IF;
 SELECT array_agg(id ORDER BY id) INTO ids FROM (SELECT id FROM public.app_records WHERE tenant_id=t AND application_id=a AND kind=record_kind AND (after_id IS NULL OR id>after_id) AND updated_at<clock_timestamp()-make_interval(days=>days) ORDER BY id LIMIT page_size FOR UPDATE) selected;
 IF ids IS NULL THEN RETURN jsonb_build_object('count',0,'after',after_id,'local_copies_purged',true,'external_copies','adapter_receipts_required','backups','expiry_required');END IF;
 after_value:=ids[array_length(ids,1)];
 DELETE FROM public.app_data_relationships WHERE tenant_id=t AND application_id=a AND ((source_kind=record_kind AND source_id=ANY(ids)) OR (target_kind=record_kind AND target_id=ANY(ids)));
 DELETE FROM public.app_record_history WHERE tenant_id=t AND application_id=a AND kind=record_kind AND record_id=ANY(ids);
 DELETE FROM public.app_records WHERE tenant_id=t AND application_id=a AND kind=record_kind AND id=ANY(ids);
 -- Completed import payloads and command replies are additional local copies.
 DELETE FROM public.app_data_imports WHERE tenant_id=t AND application_id=a AND 'data.'||entity_kind=record_kind AND state='completed' AND created_at<clock_timestamp()-make_interval(days=>days);
 DELETE FROM public.app_idempotency WHERE tenant_id=t AND application_id=a AND created_at<clock_timestamp()-make_interval(days=>days);
 RETURN jsonb_build_object('count',cardinality(ids),'after',after_value,'local_copies_purged',true,'external_copies','adapter_receipts_required','backups','expiry_required');
END $f$;
REVOKE ALL ON FUNCTION app_purge_data_records(text,uuid,integer) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app_purge_data_records(text,uuid,integer) TO kyro_app;
-- Definer gets only the caller's application even when its owner is not a superuser.
DO $policies$
DECLARE n text;
BEGIN
 FOREACH n IN ARRAY ARRAY['app_memberships','app_sessions','app_records','app_record_history','app_data_relationships','app_data_imports','app_idempotency'] LOOP
  EXECUTE format('CREATE POLICY purge_owner_scope ON public.%I TO %I USING(tenant_id=public.kyro_app_tenant_id() AND application_id=public.kyro_app_application_id()) WITH CHECK(tenant_id=public.kyro_app_tenant_id() AND application_id=public.kyro_app_application_id())',n,current_user);
 END LOOP;
END $policies$;
