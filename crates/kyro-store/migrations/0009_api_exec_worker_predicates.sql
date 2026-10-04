-- These predicates are referenced by policies shared with kyro_api. They
-- remain worker-only in effect because each returns false unless
-- session_user is kyro_worker; granting EXECUTE lets the API evaluate RLS.
GRANT EXECUTE ON FUNCTION public.kyro_worker_can_access_job(UUID, UUID),
    public.kyro_worker_can_access_effect(UUID, UUID),
    public.kyro_worker_can_account_project(UUID),
    public.kyro_worker_can_access_reservation(UUID, UUID, UUID)
TO kyro_api;
