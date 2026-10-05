-- P2 control-plane metadata only. Application databases have their own schema.
CREATE TABLE public.factory_catalogue_state (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK(singleton),
    revision BIGINT NOT NULL CHECK(revision>0),
    catalogue_digest TEXT NOT NULL CHECK(catalogue_digest ~ '^[a-f0-9]{64}$'),
    signed_catalogue JSONB NOT NULL CHECK(jsonb_typeof(signed_catalogue)='object' AND octet_length(signed_catalogue::TEXT)<=8388608),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
ALTER TABLE public.factory_catalogue_state ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.factory_catalogue_state FORCE ROW LEVEL SECURITY;
-- The global catalogue contains public code/contracts, never project inputs,
-- credentials, host paths, or application data. Only the operator can change it.
CREATE POLICY factory_catalogue_read ON public.factory_catalogue_state FOR SELECT TO kyro_api,kyro_worker USING(TRUE);
CREATE POLICY factory_catalogue_owner ON public.factory_catalogue_state TO CURRENT_USER USING(TRUE) WITH CHECK(TRUE);
GRANT SELECT ON public.factory_catalogue_state TO kyro_api,kyro_worker;

CREATE FUNCTION public.kyro_factory_catalogue_monotonic() RETURNS TRIGGER
LANGUAGE plpgsql SET search_path=pg_catalog,public AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION USING ERRCODE='23514',MESSAGE='catalogue history cannot be rewound'; END IF;
 IF NEW.revision<=OLD.revision OR NEW.catalogue_digest=OLD.catalogue_digest THEN
   RAISE EXCEPTION USING ERRCODE='23514',MESSAGE='catalogue revision must advance';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER factory_catalogue_monotonic BEFORE UPDATE OR DELETE ON public.factory_catalogue_state
FOR EACH ROW EXECUTE FUNCTION public.kyro_factory_catalogue_monotonic();

CREATE TABLE public.factory_artifacts (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES public.projects(id) ON DELETE RESTRICT,
    job_id UUID NOT NULL UNIQUE REFERENCES public.jobs(id) ON DELETE RESTRICT,
    job_generation BIGINT NOT NULL CHECK(job_generation>0),
    lease_owner UUID NOT NULL,
    application_id UUID NOT NULL,
    environment TEXT NOT NULL CHECK(environment IN ('development','production')),
    source_revision BIGINT NOT NULL CHECK(source_revision>=0),
    image_digest TEXT NOT NULL CHECK(image_digest ~ '^sha256:[a-f0-9]{64}$'),
    lock_digest TEXT NOT NULL CHECK(lock_digest ~ '^[a-f0-9]{64}$'),
    source_digest TEXT NOT NULL CHECK(source_digest ~ '^[a-f0-9]{64}$'),
    evidence_digest TEXT NOT NULL CHECK(evidence_digest ~ '^[a-f0-9]{64}$'),
    release_digest TEXT NOT NULL CHECK(release_digest ~ '^[a-f0-9]{64}$'),
    signed_release JSONB NOT NULL CHECK(jsonb_typeof(signed_release)='object' AND octet_length(signed_release::TEXT)<=16384),
    signed_evidence JSONB NOT NULL CHECK(jsonb_typeof(signed_evidence)='object' AND octet_length(signed_evidence::TEXT)<=32768),
    source_manifest JSONB NOT NULL CHECK(jsonb_typeof(source_manifest)='object' AND octet_length(source_manifest::TEXT)<=524288),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY(project_id,source_revision) REFERENCES public.app_revisions(project_id,revision)
    ,FOREIGN KEY(job_id,project_id) REFERENCES public.jobs(id,project_id)
);
ALTER TABLE public.factory_artifacts ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.factory_artifacts FORCE ROW LEVEL SECURITY;
CREATE POLICY factory_artifacts_read ON public.factory_artifacts FOR SELECT TO kyro_api,kyro_worker
USING(environment=public.kyro_environment() AND public.kyro_actor_has_action(project_id,'read'));
CREATE POLICY factory_artifacts_append ON public.factory_artifacts FOR INSERT TO kyro_worker
WITH CHECK(environment=public.kyro_environment()
 AND public.kyro_actor_has_action(project_id,'execute') AND public.kyro_actor_has_action(project_id,'read')
 AND EXISTS(SELECT 1 FROM public.jobs j WHERE j.id=job_id AND j.project_id=factory_artifacts.project_id
   AND j.actor_id=public.kyro_actor_id() AND j.environment=factory_artifacts.environment
   AND j.source_revision=factory_artifacts.source_revision AND j.payload->>'kind'='build_application'
   AND j.generation=factory_artifacts.job_generation AND j.lease_owner=factory_artifacts.lease_owner::TEXT
   AND j.status='running' AND NOT j.cancel_requested AND j.deadline>clock_timestamp() AND j.lease_until>clock_timestamp()));
CREATE POLICY factory_artifacts_owner ON public.factory_artifacts TO CURRENT_USER USING(TRUE) WITH CHECK(TRUE);
GRANT SELECT ON public.factory_artifacts TO kyro_api,kyro_worker;
GRANT INSERT ON public.factory_artifacts TO kyro_worker;

CREATE FUNCTION public.kyro_factory_artifact_immutable() RETURNS TRIGGER
LANGUAGE plpgsql SET search_path=pg_catalog,public AS $$
BEGIN RAISE EXCEPTION USING ERRCODE='23514',MESSAGE='factory artifact is immutable'; END $$;
CREATE TRIGGER factory_artifact_immutable BEFORE UPDATE OR DELETE ON public.factory_artifacts
FOR EACH ROW EXECUTE FUNCTION public.kyro_factory_artifact_immutable();

-- SELECT FOR SHARE requires update privilege. This helper keeps that privilege
-- with the owner and lets a fresh authorized job fence operator revocations.
CREATE FUNCTION public.kyro_lock_factory_catalogue(target_project UUID,expected_revision BIGINT,expected_digest TEXT)
RETURNS BOOLEAN LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,public AS $$
DECLARE actual_revision BIGINT; actual_digest TEXT;
BEGIN
 IF session_user NOT IN ('kyro_api','kyro_worker') OR NOT public.kyro_actor_has_action(target_project,'execute')
   OR NOT public.kyro_actor_has_action(target_project,'read') THEN RETURN FALSE; END IF;
 SELECT revision,catalogue_digest INTO actual_revision,actual_digest FROM public.factory_catalogue_state
 WHERE singleton FOR SHARE;
 RETURN COALESCE(actual_revision=expected_revision AND actual_digest=expected_digest,FALSE);
END $$;
REVOKE ALL ON FUNCTION public.kyro_lock_factory_catalogue(UUID,BIGINT,TEXT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.kyro_lock_factory_catalogue(UUID,BIGINT,TEXT) TO kyro_api,kyro_worker;

-- Keep P1's existing admissions unchanged, add only the new closed job kind.
CREATE POLICY change_commands_insert_factory ON public.change_commands FOR INSERT TO kyro_api
WITH CHECK(jsonb_typeof(result->'job_id')='string' AND (result-'job_id')='{}'::JSONB
 AND public.kyro_actor_has_action(project_id,'execute') AND public.kyro_actor_has_action(project_id,'read')
 AND EXISTS(SELECT 1 FROM public.jobs j WHERE j.id::TEXT=change_commands.result->>'job_id'
   AND j.project_id=change_commands.project_id AND j.actor_id=public.kyro_actor_id()
   AND j.environment=public.kyro_environment() AND j.status='pending' AND j.payload->>'kind'='build_application'));
