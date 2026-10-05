CREATE TABLE public.app_sched_attendance_proofs (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT public.kyro_app_application_id(),
 id uuid NOT NULL, token_hash bytea NOT NULL CHECK(octet_length(token_hash)=32),
 booking_id uuid NOT NULL, booking_version bigint NOT NULL CHECK(booking_version>0),
 issued_by uuid NOT NULL, expires_at timestamptz NOT NULL, consumed_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id), UNIQUE(tenant_id,application_id,token_hash),
 FOREIGN KEY(tenant_id,application_id,booking_id) REFERENCES public.app_sched_bookings(tenant_id,application_id,id)
);
ALTER TABLE public.app_sched_attendance_proofs ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_sched_attendance_proofs FORCE ROW LEVEL SECURITY;
CREATE POLICY app_scope ON public.app_sched_attendance_proofs TO kyro_app
 USING(tenant_id=public.kyro_app_tenant_id() AND application_id=public.kyro_app_application_id())
 WITH CHECK(tenant_id=public.kyro_app_tenant_id() AND application_id=public.kyro_app_application_id());
GRANT SELECT,INSERT,UPDATE,DELETE ON public.app_sched_attendance_proofs TO kyro_app;
CREATE INDEX attendance_proof_booking ON public.app_sched_attendance_proofs
 (tenant_id,application_id,booking_id,expires_at);
