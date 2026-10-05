-- Scheduling state is shared by the qualified Rust blocks, never generated SQL.
CREATE TABLE app_sched_establishments (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL,
 owner_id uuid NOT NULL, name text NOT NULL CHECK(length(name) BETWEEN 1 AND 240), timezone text NOT NULL,
 version bigint NOT NULL CHECK(version>0), created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id), FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id),
 FOREIGN KEY(tenant_id,owner_id) REFERENCES app_principals(tenant_id,id)
);
CREATE TABLE app_sched_resources (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL,
 establishment_id uuid NOT NULL, owner_id uuid NOT NULL, category text NOT NULL CHECK(category IN ('staff','room','equipment','service')),
 name text NOT NULL CHECK(length(name) BETWEEN 1 AND 240), capacity integer NOT NULL CHECK(capacity BETWEEN 1 AND 10000),
 active boolean NOT NULL DEFAULT true, version bigint NOT NULL CHECK(version>0), created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id), FOREIGN KEY(tenant_id,application_id,establishment_id) REFERENCES app_sched_establishments(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,owner_id) REFERENCES app_principals(tenant_id,id), CHECK(category='service' OR capacity=1)
);
CREATE TABLE app_sched_policies (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), resource_id uuid NOT NULL,
 version bigint NOT NULL CHECK(version>0), cancel_before_minutes integer NOT NULL CHECK(cancel_before_minutes>=0),
 reschedule_before_minutes integer NOT NULL CHECK(reschedule_before_minutes>=0), created_by uuid NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), PRIMARY KEY(tenant_id,application_id,resource_id,version),
 FOREIGN KEY(tenant_id,application_id,resource_id) REFERENCES app_sched_resources(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,created_by) REFERENCES app_principals(tenant_id,id)
);
CREATE TABLE app_sched_availability (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL,
 resource_id uuid NOT NULL, weekday smallint NOT NULL CHECK(weekday BETWEEN 0 AND 6), timezone text NOT NULL,
 local_start time NOT NULL, local_end time NOT NULL, valid_from date NOT NULL, valid_until date,
 dst_policy text NOT NULL CHECK(dst_policy='reject'), created_by uuid NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), PRIMARY KEY(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,application_id,resource_id) REFERENCES app_sched_resources(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,created_by) REFERENCES app_principals(tenant_id,id), CHECK(local_start<local_end), CHECK(valid_until IS NULL OR valid_until>=valid_from)
);
CREATE TABLE app_sched_recurrences (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL,
 resource_id uuid NOT NULL, availability_id uuid NOT NULL, timezone text NOT NULL, starts_on date NOT NULL, ends_on date NOT NULL,
 weekdays smallint[] NOT NULL CHECK(cardinality(weekdays) BETWEEN 1 AND 7), local_start time NOT NULL,
 duration_minutes integer NOT NULL CHECK(duration_minutes BETWEEN 1 AND 1440), dst_policy text NOT NULL CHECK(dst_policy IN ('reject','skip')),
 max_occurrences integer NOT NULL CHECK(max_occurrences BETWEEN 1 AND 366), created_by uuid NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), PRIMARY KEY(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,application_id,resource_id) REFERENCES app_sched_resources(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,application_id,availability_id) REFERENCES app_sched_availability(tenant_id,application_id,id),
 CHECK(ends_on>=starts_on AND ends_on-starts_on<=366)
);
CREATE TABLE app_sched_recurrence_exceptions (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), recurrence_id uuid NOT NULL, local_date date NOT NULL,
 PRIMARY KEY(tenant_id,application_id,recurrence_id,local_date), FOREIGN KEY(tenant_id,application_id,recurrence_id) REFERENCES app_sched_recurrences(tenant_id,application_id,id)
);
CREATE TABLE app_sched_slots (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL,
 resource_id uuid NOT NULL, availability_id uuid, recurrence_id uuid, starts_at timestamptz NOT NULL, ends_at timestamptz NOT NULL,
 capacity integer NOT NULL CHECK(capacity BETWEEN 1 AND 10000), reserved_units integer NOT NULL DEFAULT 0 CHECK(reserved_units>=0),
 policy_version bigint NOT NULL, open boolean NOT NULL DEFAULT true, version bigint NOT NULL CHECK(version>0),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), PRIMARY KEY(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,application_id,resource_id,policy_version) REFERENCES app_sched_policies(tenant_id,application_id,resource_id,version),
 FOREIGN KEY(tenant_id,application_id,availability_id) REFERENCES app_sched_availability(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,application_id,recurrence_id) REFERENCES app_sched_recurrences(tenant_id,application_id,id),
 CHECK(ends_at>starts_at), CHECK(reserved_units<=capacity)
);
CREATE INDEX app_sched_slots_window ON app_sched_slots(tenant_id,application_id,resource_id,starts_at,ends_at);
CREATE TABLE app_sched_bookings (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, slot_id uuid NOT NULL,
 principal_id uuid NOT NULL, units integer NOT NULL CHECK(units BETWEEN 1 AND 10000), status text NOT NULL CHECK(status IN ('confirmed','cancelled')),
 idempotency_key text NOT NULL CHECK(length(idempotency_key) BETWEEN 1 AND 200), policy_version bigint NOT NULL,
 version bigint NOT NULL CHECK(version>0), created_at timestamptz NOT NULL DEFAULT clock_timestamp(), cancelled_at timestamptz,
 PRIMARY KEY(tenant_id,application_id,id), UNIQUE(tenant_id,application_id,principal_id,idempotency_key),
 FOREIGN KEY(tenant_id,application_id,slot_id) REFERENCES app_sched_slots(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,principal_id) REFERENCES app_principals(tenant_id,id)
);
CREATE TABLE app_sched_booking_changes (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, booking_id uuid NOT NULL,
 actor_id uuid NOT NULL, change_kind text NOT NULL, from_slot_id uuid, to_slot_id uuid, policy_version bigint NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), PRIMARY KEY(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,application_id,booking_id) REFERENCES app_sched_bookings(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,actor_id) REFERENCES app_principals(tenant_id,id)
);
CREATE TABLE app_sched_waitlist (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, slot_id uuid NOT NULL,
 principal_id uuid NOT NULL, units integer NOT NULL CHECK(units BETWEEN 1 AND 10000), status text NOT NULL CHECK(status IN ('waiting','promoted','cancelled')),
 idempotency_key text NOT NULL, position bigint GENERATED ALWAYS AS IDENTITY, booking_id uuid,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id), UNIQUE(tenant_id,application_id,principal_id,idempotency_key),
 FOREIGN KEY(tenant_id,application_id,slot_id) REFERENCES app_sched_slots(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,application_id,booking_id) REFERENCES app_sched_bookings(tenant_id,application_id,id), FOREIGN KEY(tenant_id,principal_id) REFERENCES app_principals(tenant_id,id)
);
CREATE INDEX app_sched_waitlist_fifo ON app_sched_waitlist(tenant_id,application_id,slot_id,position) WHERE status='waiting';
CREATE TABLE app_sched_assignments (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, slot_id uuid NOT NULL,
 resource_id uuid NOT NULL, units integer NOT NULL CHECK(units BETWEEN 1 AND 10000), created_by uuid NOT NULL, created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id), UNIQUE(tenant_id,application_id,slot_id,resource_id),
 FOREIGN KEY(tenant_id,application_id,slot_id) REFERENCES app_sched_slots(tenant_id,application_id,id), FOREIGN KEY(tenant_id,application_id,resource_id) REFERENCES app_sched_resources(tenant_id,application_id,id)
);
CREATE TABLE app_sched_attendance (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, booking_id uuid NOT NULL,
 principal_id uuid NOT NULL, checked_in_by uuid NOT NULL, checked_in_at timestamptz NOT NULL, checked_out_by uuid, checked_out_at timestamptz,
 PRIMARY KEY(tenant_id,application_id,id), UNIQUE(tenant_id,application_id,booking_id),
 FOREIGN KEY(tenant_id,application_id,booking_id) REFERENCES app_sched_bookings(tenant_id,application_id,id), CHECK(checked_out_at IS NULL OR checked_out_at>=checked_in_at)
);
CREATE TABLE app_sched_calendar_connections (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, principal_id uuid NOT NULL,
 provider text NOT NULL, account_ref text NOT NULL, scopes text[] NOT NULL, origin_marker text NOT NULL, version bigint NOT NULL CHECK(version>0),
 active boolean NOT NULL DEFAULT true, created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id), FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id), FOREIGN KEY(tenant_id,principal_id) REFERENCES app_principals(tenant_id,id)
);
CREATE TABLE app_sched_calendar_events (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, connection_id uuid NOT NULL,
 external_event_id text NOT NULL, remote_version text NOT NULL, title text NOT NULL, starts_at timestamptz NOT NULL, ends_at timestamptz NOT NULL,
 source_origin text, version bigint NOT NULL CHECK(version>0), created_at timestamptz NOT NULL DEFAULT clock_timestamp(), updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id), UNIQUE(tenant_id,application_id,connection_id,external_event_id),
 FOREIGN KEY(tenant_id,application_id,connection_id) REFERENCES app_sched_calendar_connections(tenant_id,application_id,id), CHECK(ends_at>starts_at)
);
CREATE TABLE app_sched_calendar_outbox (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, connection_id uuid NOT NULL,
 object_id uuid NOT NULL, object_version bigint NOT NULL CHECK(object_version>0), dedupe_key text NOT NULL,
 state text NOT NULL CHECK(state IN ('pending','sending','unknown','succeeded','failed')), payload jsonb NOT NULL CHECK(octet_length(payload::text)<=65536),
 version bigint NOT NULL DEFAULT 1, attempts integer NOT NULL DEFAULT 0 CHECK(attempts>=0), lease_until timestamptz, provider_event_id text,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id), UNIQUE(tenant_id,application_id,connection_id,dedupe_key),
 FOREIGN KEY(tenant_id,application_id,connection_id) REFERENCES app_sched_calendar_connections(tenant_id,application_id,id)
);
DO $migration$
DECLARE table_name text;
BEGIN
 FOREACH table_name IN ARRAY ARRAY['app_sched_establishments','app_sched_resources','app_sched_policies','app_sched_availability','app_sched_recurrences','app_sched_recurrence_exceptions','app_sched_slots','app_sched_bookings','app_sched_booking_changes','app_sched_waitlist','app_sched_assignments','app_sched_attendance','app_sched_calendar_connections','app_sched_calendar_events','app_sched_calendar_outbox'] LOOP
  EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY',table_name);
  EXECUTE format('ALTER TABLE public.%I FORCE ROW LEVEL SECURITY',table_name);
  EXECUTE format('CREATE POLICY app_scope ON public.%I TO kyro_app USING(tenant_id=public.kyro_app_tenant_id() AND application_id=public.kyro_app_application_id()) WITH CHECK(tenant_id=public.kyro_app_tenant_id() AND application_id=public.kyro_app_application_id())',table_name);
  EXECUTE format('GRANT SELECT,INSERT,UPDATE ON public.%I TO kyro_app',table_name);
 END LOOP;
END $migration$;
GRANT DELETE ON app_sched_assignments TO kyro_app;
REVOKE UPDATE ON app_sched_policies,app_sched_recurrences,app_sched_recurrence_exceptions,app_sched_booking_changes FROM kyro_app;
GRANT USAGE,SELECT ON SEQUENCE app_sched_waitlist_position_seq TO kyro_app;
