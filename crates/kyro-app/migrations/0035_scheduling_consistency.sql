-- Additive correction: Rust exposes explicit fold/gap policies, which must also
-- be representable in PostgreSQL. Already applied migrations stay unchanged.
ALTER TABLE public.app_sched_recurrences DROP CONSTRAINT app_sched_recurrences_dst_policy_check;
ALTER TABLE public.app_sched_recurrences ADD CONSTRAINT app_sched_recurrences_dst_policy_check
 CHECK(dst_policy IN ('reject','skip','earlier','later','shift_forward'));
-- A slow, older provider snapshot cannot replace the most recently requested
-- snapshot. Export versions remain independent of this read generation.
ALTER TABLE public.app_sched_calendar_connections ADD COLUMN read_generation bigint NOT NULL DEFAULT 0 CHECK(read_generation>=0);
