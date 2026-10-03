-- Event sequence numbers are global to a project, while event rows are
-- environment-filtered at read time. Return physical retention bounds and
-- the project counter together so hidden events do not look like purged rows.
CREATE FUNCTION public.kyro_event_history_bounds(target_project UUID)
RETURNS TABLE (earliest_sequence BIGINT, latest_sequence BIGINT)
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$
    SELECT min(e.sequence), p.event_sequence
    FROM public.projects p
    LEFT JOIN public.events e ON e.project_id = p.id
    WHERE p.id = target_project
      AND session_user IN ('kyro_api', 'kyro_worker')
      AND public.kyro_actor_id() IS NOT NULL
      AND public.kyro_environment() IN ('development', 'production')
      AND public.kyro_project_visible(target_project) IS TRUE
      AND public.kyro_actor_has_action(target_project, 'read') IS TRUE
    GROUP BY p.event_sequence
$$;

REVOKE ALL ON FUNCTION public.kyro_event_history_bounds(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.kyro_event_history_bounds(UUID)
    TO kyro_api, kyro_worker;
