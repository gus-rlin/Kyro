-- Runtime grant checks need a stable row lock, while UPDATE policies remain
-- organization-owner-only. Lock membership first so removal cannot race use.
CREATE FUNCTION public.kyro_lock_actor_grants(
    target_actor UUID,
    target_project UUID,
    target_actions TEXT[]
)
RETURNS TABLE (
    id UUID,
    actions TEXT[],
    resources TEXT[],
    environment TEXT,
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    limits JSONB
)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
BEGIN
    IF session_user NOT IN ('kyro_api', 'kyro_worker')
       OR target_actor IS NULL
       OR target_project IS NULL
       OR public.kyro_actor_id() IS DISTINCT FROM target_actor
       OR public.kyro_environment() IS NULL
       OR public.kyro_environment() NOT IN ('development', 'production')
       OR target_actions IS NULL
       OR pg_catalog.cardinality(target_actions) NOT BETWEEN 1 AND 6
       OR pg_catalog.array_position(target_actions, NULL) IS NOT NULL
       OR (target_actions <@ ARRAY['read', 'write', 'execute', 'model', 'manage', 'budget']::TEXT[]) IS NOT TRUE
       OR public.kyro_project_visible(target_project) IS NOT TRUE THEN
        RETURN;
    END IF;

    PERFORM 1
    FROM public.projects p
    JOIN public.memberships m ON m.organization_id = p.organization_id
    WHERE p.id = target_project
      AND m.actor_id = target_actor
    FOR SHARE OF m;
    IF NOT FOUND THEN
        RETURN;
    END IF;

    RETURN QUERY
    SELECT g.id, g.actions, g.resources, g.environment, g.expires_at, g.revoked_at, g.limits
    FROM public.capability_grants g
    WHERE g.actor_id = target_actor
      AND g.project_id = target_project
      AND g.environment = public.kyro_environment()
      AND (g.expires_at IS NULL OR g.expires_at > pg_catalog.clock_timestamp())
      AND g.revoked_at IS NULL
      AND (g.resources @> ARRAY['*']::TEXT[] OR g.resources @> ARRAY[target_project::TEXT]::TEXT[])
      AND g.actions && target_actions
    ORDER BY g.id
    FOR SHARE OF g;
END
$$;

REVOKE ALL ON FUNCTION public.kyro_lock_actor_grants(UUID, UUID, TEXT[]) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.kyro_lock_actor_grants(UUID, UUID, TEXT[])
    TO kyro_api, kyro_worker;
