-- Provisional chat output is private, bounded, and fenced by the actual worker lease.
CREATE TABLE public.chat_stream_chunks (
    project_id UUID NOT NULL REFERENCES public.projects(id) ON DELETE CASCADE,
    job_id UUID NOT NULL REFERENCES public.jobs(id) ON DELETE CASCADE,
    effect_id UUID NOT NULL REFERENCES public.effects(id) ON DELETE CASCADE,
    generation BIGINT NOT NULL CHECK (generation > 0),
    lease_owner UUID NOT NULL,
    chunk_index BIGINT NOT NULL CHECK (chunk_index BETWEEN 1 AND 1024),
    text TEXT NOT NULL CHECK (octet_length(text) BETWEEN 1 AND 4096),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (job_id, generation, chunk_index)
);

CREATE FUNCTION public.kyro_chat_lease_active(p UUID, j UUID, e UUID, g BIGINT, o UUID)
RETURNS BOOLEAN LANGUAGE SQL VOLATILE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT session_user = 'kyro_worker'
      AND EXISTS (
        SELECT 1 FROM public.jobs job
        JOIN public.projects project ON project.id = job.project_id
        JOIN public.effects effect ON effect.job_id = job.id AND effect.project_id = job.project_id
        JOIN public.budget_reservations reservation ON reservation.id = effect.reservation_id AND reservation.effect_id = effect.id
        WHERE job.id = j AND job.project_id = p AND effect.id = e
          AND job.actor_id = public.kyro_actor_id() AND job.environment = public.kyro_environment()
          AND job.status = 'running' AND NOT job.cancel_requested
          AND job.generation = g AND job.lease_owner = o::TEXT
          AND job.lease_until > clock_timestamp() AND job.deadline > clock_timestamp()
          AND project.current_revision = job.source_revision
          AND job.payload->'request'->'input'->>'purpose' = 'conversation'
          AND effect.generation = g AND effect.status = 'sending' AND reservation.status = 'held'
          AND effect.intent->'registration'->>'output_mode' = 'text_chat'
          AND public.kyro_actor_has_action(p, 'execute') AND public.kyro_actor_has_action(p, 'model')
      )
$$;
REVOKE ALL ON FUNCTION public.kyro_chat_lease_active(UUID, UUID, UUID, BIGINT, UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.kyro_chat_lease_active(UUID, UUID, UUID, BIGINT, UUID) TO kyro_worker;

CREATE FUNCTION public.kyro_bound_chat_chunk() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public AS $$
DECLARE previous_index BIGINT; previous_bytes BIGINT;
BEGIN
    -- Same project -> job lock order as queue/accounting; serializes concurrent appends.
    PERFORM 1 FROM public.projects WHERE id = NEW.project_id FOR UPDATE;
    PERFORM 1 FROM public.jobs WHERE id = NEW.job_id FOR UPDATE;
    IF NOT public.kyro_chat_lease_active(NEW.project_id, NEW.job_id, NEW.effect_id, NEW.generation, NEW.lease_owner) THEN
        RAISE EXCEPTION 'chat lease inactive' USING ERRCODE = '42501';
    END IF;
    SELECT COALESCE(MAX(chunk_index), 0), COALESCE(SUM(octet_length(text)), 0)
      INTO previous_index, previous_bytes FROM public.chat_stream_chunks
      WHERE job_id = NEW.job_id AND generation = NEW.generation;
    IF NEW.chunk_index <> previous_index + 1 OR previous_bytes + octet_length(NEW.text) > 32768 THEN
        RAISE EXCEPTION 'chat stream bound exceeded' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION public.kyro_bound_chat_chunk() FROM PUBLIC;
CREATE TRIGGER bound_chat_chunk BEFORE INSERT ON public.chat_stream_chunks
    FOR EACH ROW EXECUTE FUNCTION public.kyro_bound_chat_chunk();

ALTER TABLE public.chat_stream_chunks ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.chat_stream_chunks FORCE ROW LEVEL SECURITY;
CREATE POLICY chat_chunks_read ON public.chat_stream_chunks FOR SELECT TO kyro_api, kyro_worker
    USING (public.kyro_actor_has_action(project_id, 'read') AND EXISTS (
        SELECT 1 FROM public.jobs j WHERE j.id = job_id AND j.project_id = chat_stream_chunks.project_id
          AND j.actor_id = public.kyro_actor_id() AND j.environment = public.kyro_environment()
    ));
CREATE POLICY chat_chunks_insert ON public.chat_stream_chunks FOR INSERT TO kyro_worker
    WITH CHECK (public.kyro_chat_lease_active(project_id, job_id, effect_id, generation, lease_owner));
GRANT SELECT ON public.chat_stream_chunks TO kyro_api;
GRANT SELECT, INSERT ON public.chat_stream_chunks TO kyro_worker;
