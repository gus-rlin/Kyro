-- Only the guarded retention entry point can erase cross-recipient copies.
-- UUID matching is deliberately conservative: a matching textual UUID may also
-- invalidate a derived reply. Keys and accounting remain to prevent re-execution.
CREATE FUNCTION app_private_mentions_ids(payload jsonb, ids uuid[]) RETURNS boolean
LANGUAGE sql IMMUTABLE SET search_path=pg_catalog AS $$
 SELECT EXISTS(SELECT 1 FROM unnest(ids) AS i WHERE position(i::text IN payload::text)>0)
$$;
REVOKE ALL ON FUNCTION app_private_mentions_ids(jsonb,uuid[]) FROM PUBLIC;

DO $$ DECLARE n text; BEGIN
 FOREACH n IN ARRAY ARRAY['app_search_sources','app_search_chunks','app_ai_requests','app_ai_effects',
  'app_analytics_facts','app_analytics_snapshots','app_analytics_alerts','app_analytics_exports',
  'app_analytics_reports','app_notifications','app_notification_heads','app_presence','app_private_exports',
  'app_connector_calls','app_outbox','app_quotas'] LOOP
  EXECUTE format('CREATE POLICY retention_owner_scope ON public.%I TO %I USING(tenant_id=public.kyro_app_tenant_id() AND application_id=public.kyro_app_application_id()) WITH CHECK(tenant_id=public.kyro_app_tenant_id() AND application_id=public.kyro_app_application_id())',n,current_user);
 END LOOP;
END $$;

CREATE OR REPLACE FUNCTION app_purge_data_records(record_kind text,after_id uuid,page_size integer)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $$
DECLARE
 t uuid:=public.kyro_app_tenant_id(); a uuid:=public.kyro_app_application_id(); p uuid:=public.kyro_app_actor_id();
 ids uuid[]; copies uuid[]; ai_ids uuid[]; job_ids uuid[]; call_ids uuid[]; export_ids uuid[]; import_ids uuid[];
 days integer; cutoff timestamptz; after_value uuid; n bigint; removed_chunks bigint;
 ai_units bigint; ai_tokens bigint; export_reserved bigint; export_used bigint;
 slots bigint; jobs_reserved bigint; connector_reserved bigint; imports_reserved bigint;
 counts jsonb:='{}';
BEGIN
 -- This fence precedes every row lock. The dispatcher also selects exclusive
 -- mode, avoiding an upgrade deadlock between simultaneous purges.
 PERFORM pg_advisory_xact_lock(hashtextextended('app-authority:'||t::text||':'||a::text,0));
 IF NOT EXISTS(SELECT 1 FROM public.app_memberships m JOIN public.app_sessions s
  ON s.tenant_id=m.tenant_id AND s.application_id=m.application_id AND s.principal_id=m.principal_id
  WHERE m.tenant_id=t AND m.application_id=a AND m.principal_id=p AND m.status='active'
  AND m.role IN ('owner','admin','security.admin') AND s.id=public.kyro_app_session_id()
  AND s.api_key_id IS NULL AND s.revoked_at IS NULL AND s.expires_at>clock_timestamp()
  AND s.mfa_at>clock_timestamp()-interval '5 minutes') THEN
  RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='purge denied';
 END IF;
 IF record_kind NOT LIKE 'data.%' OR page_size NOT BETWEEN 1 AND 100 THEN
  RAISE EXCEPTION USING ERRCODE='23514',MESSAGE='invalid purge scope';
 END IF;
 SELECT (data->>'days')::integer INTO days FROM public.app_records
  WHERE tenant_id=t AND application_id=a AND kind='security.retention' AND data->>'kind'=record_kind;
 IF days IS NULL OR days NOT BETWEEN 1 AND 3650 THEN
  RAISE EXCEPTION USING ERRCODE='42501',MESSAGE='retention policy required';
 END IF;
 cutoff:=clock_timestamp()-make_interval(days=>days);
 SELECT COALESCE(array_agg(id ORDER BY id),'{}'::uuid[]) INTO ids FROM
  (SELECT id FROM public.app_records WHERE tenant_id=t AND application_id=a AND kind=record_kind
   AND (after_id IS NULL OR id>after_id) AND updated_at<cutoff ORDER BY id LIMIT page_size FOR UPDATE) selected;
 after_value:=COALESCE(ids[cardinality(ids)],after_id); copies:=ids;

 -- Search chunks cascade. Capture their IDs to redact cached indexing replies.
 WITH d AS(DELETE FROM public.app_search_sources WHERE tenant_id=t AND application_id=a
  AND (expires_at<=clock_timestamp() OR (source->>'kind'=record_kind AND source->>'id'=ANY(SELECT i::text FROM unnest(ids) i)))
  RETURNING id,chunk_count)
 SELECT COALESCE(array_agg(id),'{}'::uuid[]),COALESCE(sum(chunk_count),0),count(*) INTO ai_ids,removed_chunks,n FROM d;
 copies:=copies||ai_ids; counts:=counts||jsonb_build_object('search_sources',n);
 UPDATE public.app_quotas SET used_value=used_value-removed_chunks
  WHERE tenant_id=t AND application_id=a AND quota_key='search_chunks' AND removed_chunks>0;

 SELECT COALESCE(array_agg(id),'{}'::uuid[]),COALESCE(array_agg(job_id) FILTER(WHERE job_id IS NOT NULL),'{}'::uuid[])
  INTO ai_ids,job_ids FROM public.app_ai_requests WHERE tenant_id=t AND application_id=a
  AND (expires_at<=clock_timestamp() OR public.app_private_mentions_ids(source_bindings,ids));
 copies:=copies||ai_ids;
 SELECT COALESCE(sum(reserved_units),0),COALESCE(sum(reserved_tokens),0) INTO ai_units,ai_tokens
  FROM public.app_ai_effects WHERE tenant_id=t AND application_id=a AND request_id=ANY(ai_ids)
  AND status='prepared' AND reservation_status='held';
 UPDATE public.app_ai_effects SET response=NULL,
  status=CASE WHEN status='prepared' THEN 'cancelled' WHEN status='sending' THEN 'unknown' ELSE status END,
  reservation_status=CASE WHEN status='prepared' AND reservation_status='held' THEN 'released' ELSE reservation_status END,
  failure_code=CASE WHEN status IN ('prepared','sending') THEN 'private_data_purged' ELSE failure_code END,
  updated_at=clock_timestamp() WHERE tenant_id=t AND application_id=a AND request_id=ANY(ai_ids);
 -- Intent contains only hashes, model/pricing registration and accounting; no prompt.
 UPDATE public.app_quotas SET reserved_value=reserved_value-CASE quota_key WHEN 'ai_budget_units' THEN ai_units ELSE ai_tokens END
  WHERE tenant_id=t AND application_id=a AND quota_key IN ('ai_budget_units','ai_tokens');
 UPDATE public.app_ai_requests r SET specification='{}',source_bindings='[]',result=NULL,correction=NULL,
  expires_at=LEAST(expires_at,clock_timestamp()),state=CASE WHEN EXISTS(SELECT 1 FROM public.app_ai_effects e
   WHERE e.tenant_id=t AND e.application_id=a AND e.request_id=r.id AND e.status='unknown') THEN 'unknown' ELSE 'cancelled' END
  WHERE r.tenant_id=t AND r.application_id=a AND r.id=ANY(ai_ids);
 GET DIAGNOSTICS n=ROW_COUNT; counts:=counts||jsonb_build_object('ai_requests',n);

 WITH d AS(DELETE FROM public.app_analytics_facts WHERE tenant_id=t AND application_id=a
  AND (expires_at<=clock_timestamp() OR public.app_private_mentions_ids(source,ids)) RETURNING id)
 SELECT COALESCE(array_agg(id),'{}'::uuid[]),count(*) INTO ai_ids,n FROM d;
 copies:=copies||ai_ids; counts:=counts||jsonb_build_object('analytics_facts',n);
 UPDATE public.app_quotas SET used_value=used_value-n WHERE tenant_id=t AND application_id=a AND quota_key='analytics_facts' AND n>0;
 WITH d AS(DELETE FROM public.app_analytics_snapshots WHERE tenant_id=t AND application_id=a
  AND (expires_at<=clock_timestamp() OR public.app_private_mentions_ids(bindings,copies)) RETURNING id)
 SELECT COALESCE(array_agg(id),'{}'::uuid[]),count(*) INTO ai_ids,n FROM d;
 copies:=copies||ai_ids; counts:=counts||jsonb_build_object('analytics_snapshots',n);
 UPDATE public.app_quotas SET used_value=used_value-n WHERE tenant_id=t AND application_id=a AND quota_key='analytics_snapshots' AND n>0;
 WITH d AS(DELETE FROM public.app_analytics_alerts WHERE tenant_id=t AND application_id=a
  AND (expires_at<=clock_timestamp() OR public.app_private_mentions_ids(bindings,copies)) RETURNING id)
 SELECT COALESCE(array_agg(id),'{}'::uuid[]),count(*) INTO ai_ids,n FROM d;
 copies:=copies||ai_ids; counts:=counts||jsonb_build_object('analytics_alerts',n);
 SELECT COALESCE(array_agg(id),'{}'::uuid[]),
  COALESCE(array_agg(job_id) FILTER(WHERE job_id IS NOT NULL),'{}'::uuid[]),
  COALESCE(sum(reserved_bytes) FILTER(WHERE state IN ('captured','processing')),0),
  COALESCE(sum(octet_length(artifact)) FILTER(WHERE state='ready'),0)
  INTO export_ids,ai_ids,export_reserved,export_used FROM public.app_analytics_exports
  WHERE tenant_id=t AND application_id=a AND (expires_at<=clock_timestamp() OR public.app_private_mentions_ids(bindings,copies));
 job_ids:=job_ids||ai_ids; copies:=copies||export_ids;
 DELETE FROM public.app_analytics_exports WHERE tenant_id=t AND application_id=a AND id=ANY(export_ids);
 GET DIAGNOSTICS n=ROW_COUNT; counts:=counts||jsonb_build_object('analytics_exports',n);
 UPDATE public.app_quotas SET reserved_value=reserved_value-export_reserved,used_value=used_value-export_used
  WHERE tenant_id=t AND application_id=a AND quota_key='export_bytes';
 WITH d AS(DELETE FROM public.app_analytics_reports WHERE tenant_id=t AND application_id=a
  AND (expires_at<=clock_timestamp() OR public.app_private_mentions_ids(bindings,copies)) RETURNING id,job_id)
 SELECT COALESCE(array_agg(id),'{}'::uuid[]),COALESCE(array_agg(job_id) FILTER(WHERE job_id IS NOT NULL),'{}'::uuid[]),count(*)
  INTO ai_ids,export_ids,n FROM d;
 copies:=copies||ai_ids; job_ids:=job_ids||export_ids; counts:=counts||jsonb_build_object('analytics_reports',n);

 WITH d AS(DELETE FROM public.app_notifications WHERE tenant_id=t AND application_id=a
  AND (expires_at<=clock_timestamp() OR (source_kind=record_kind AND source_id=ANY(ids))) RETURNING id,recipient_id,sequence),
 h AS(UPDATE public.app_notification_heads h SET purged_through=GREATEST(h.purged_through,d.last)
  FROM(SELECT recipient_id,max(sequence) AS last FROM d GROUP BY recipient_id)d
  WHERE h.tenant_id=t AND h.application_id=a AND h.recipient_id=d.recipient_id)
 SELECT COALESCE(array_agg(id),'{}'::uuid[]),count(*) INTO ai_ids,n FROM d;
 copies:=copies||ai_ids; counts:=counts||jsonb_build_object('notifications',n);
 UPDATE public.app_quotas SET used_value=used_value-n WHERE tenant_id=t AND application_id=a AND quota_key='notifications' AND n>0;
 DELETE FROM public.app_presence WHERE tenant_id=t AND application_id=a AND expires_at<=clock_timestamp();
 GET DIAGNOSTICS n=ROW_COUNT; counts:=counts||jsonb_build_object('presence',n);
 -- Personal exports have no per-resource provenance: invalidate all on a data purge.
 WITH d AS(DELETE FROM public.app_private_exports WHERE tenant_id=t AND application_id=a
  AND (expires_at<=clock_timestamp() OR cardinality(ids)>0) RETURNING id)
 SELECT COALESCE(array_agg(id),'{}'::uuid[]),count(*) INTO ai_ids,n FROM d;
 copies:=copies||ai_ids; counts:=counts||jsonb_build_object('personal_exports',n);

 SELECT COALESCE(array_agg(c.id),'{}'::uuid[]) INTO call_ids FROM public.app_connector_calls c
  LEFT JOIN public.app_outbox o ON o.tenant_id=c.tenant_id AND o.application_id=c.application_id AND o.id=c.outbox_id
  WHERE c.tenant_id=t AND c.application_id=a AND
  (c.expires_at<=clock_timestamp() OR public.app_private_mentions_ids(o.payload,copies));
 -- A committed claim can already have emitted. Only pending outboxes release a reserve.
 SELECT COALESCE(sum(c.reserved_units),0) INTO connector_reserved FROM public.app_connector_calls c
  JOIN public.app_outbox o ON o.tenant_id=c.tenant_id AND o.application_id=c.application_id AND o.id=c.outbox_id
  WHERE c.tenant_id=t AND c.application_id=a AND c.id=ANY(call_ids) AND c.state='queued' AND o.state='pending';
 UPDATE public.app_connector_calls c SET request_cipher=decode(repeat('00',28),'hex'),result=NULL,
  expires_at=LEAST(c.expires_at,clock_timestamp()),error_code='private_data_purged',
  state=CASE WHEN c.state='queued' THEN CASE WHEN EXISTS(SELECT 1 FROM public.app_outbox o
   WHERE o.tenant_id=t AND o.application_id=a AND o.id=c.outbox_id AND o.state='pending') THEN 'cancelled' ELSE 'unknown' END ELSE c.state END
  WHERE c.tenant_id=t AND c.application_id=a AND c.id=ANY(call_ids);
 GET DIAGNOSTICS n=ROW_COUNT; counts:=counts||jsonb_build_object('connector_calls',n); copies:=copies||call_ids;
 UPDATE public.app_quotas SET reserved_value=reserved_value-connector_reserved
  WHERE tenant_id=t AND application_id=a AND quota_key='connector_budget_units';
 UPDATE public.app_outbox SET payload='{"purged":true}',receipt=NULL,
  state=CASE WHEN state='pending' THEN 'failed' WHEN state='claimed' THEN 'unknown' ELSE state END
  WHERE tenant_id=t AND application_id=a AND public.app_private_mentions_ids(payload,copies);

 SELECT COALESCE(array_agg(import_id),'{}'::uuid[]),COALESCE(sum(reserved_units),0) INTO import_ids,imports_reserved
  FROM public.app_data_imports WHERE tenant_id=t AND application_id=a AND 'data.'||entity_kind=record_kind
  AND (expires_at<=clock_timestamp() OR public.app_private_mentions_ids(payload,ids));
 DELETE FROM public.app_data_imports WHERE tenant_id=t AND application_id=a AND import_id=ANY(import_ids);
 GET DIAGNOSTICS n=ROW_COUNT; counts:=counts||jsonb_build_object('imports',n); copies:=copies||import_ids;
 UPDATE public.app_quotas SET reserved_value=reserved_value-imports_reserved
  WHERE tenant_id=t AND application_id=a AND quota_key='imports';

 SELECT count(*) FILTER(WHERE state='leased'),count(*) FILTER(WHERE NOT reservation_settled)
  INTO slots,jobs_reserved FROM public.app_jobs WHERE tenant_id=t AND application_id=a
  AND (id=ANY(job_ids) OR public.app_private_mentions_ids(specification,copies) OR public.app_private_mentions_ids(result,copies))
  AND state NOT IN ('completed','cancelled');
 UPDATE public.app_quotas SET reserved_value=reserved_value-CASE quota_key WHEN 'jobs' THEN jobs_reserved ELSE slots END
  WHERE tenant_id=t AND application_id=a AND quota_key IN ('jobs','job_slots');
 UPDATE public.app_jobs SET specification='{"purged":true}',result=NULL,
  state=CASE WHEN state='completed' THEN 'completed' ELSE 'cancelled' END,reservation_settled=true,
  lease_id=NULL,lease_owner=NULL,lease_until=NULL,deadline=LEAST(deadline,clock_timestamp()),error_code='private_data_purged'
  WHERE tenant_id=t AND application_id=a AND (id=ANY(job_ids) OR public.app_private_mentions_ids(specification,copies) OR public.app_private_mentions_ids(result,copies));
 GET DIAGNOSTICS n=ROW_COUNT; counts:=counts||jsonb_build_object('jobs',n);
 UPDATE public.app_job_schedules SET specification='{"purged":true}',enabled=false
  WHERE tenant_id=t AND application_id=a AND public.app_private_mentions_ids(specification,copies);

 UPDATE public.app_idempotency SET response='{"purged":true,"repeat_execution":false}'
  WHERE tenant_id=t AND application_id=a AND (public.app_private_mentions_ids(response,copies) OR created_at<cutoff)
  AND response<>'{"purged":true,"repeat_execution":false}'::jsonb;
 GET DIAGNOSTICS n=ROW_COUNT; counts:=counts||jsonb_build_object('redacted_replies',n);
 DELETE FROM public.app_data_relationships WHERE tenant_id=t AND application_id=a
  AND ((source_kind=record_kind AND source_id=ANY(ids)) OR (target_kind=record_kind AND target_id=ANY(ids)));
 DELETE FROM public.app_record_history WHERE tenant_id=t AND application_id=a AND kind=record_kind AND record_id=ANY(ids);
 DELETE FROM public.app_records WHERE tenant_id=t AND application_id=a AND kind=record_kind AND id=ANY(ids);
 RETURN jsonb_build_object('count',cardinality(ids),'after',after_value,'local_copies_purged',true,'copies',counts,
  'audit','identifiers_and_hashes_retained','idempotency','keys_retained_replies_redacted',
  'unknown_effects','accounting_and_reservations_retained','personal_exports','all_invalidated_on_data_purge',
  'external_copies','adapter_receipts_required','backups','expiry_required');
END $$;
REVOKE ALL ON FUNCTION app_purge_data_records(text,uuid,integer) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app_purge_data_records(text,uuid,integer) TO kyro_app;
