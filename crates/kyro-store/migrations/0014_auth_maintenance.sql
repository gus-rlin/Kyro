-- Authentication cleanup is API-only and may remove rows only after their
-- one-time flow is consumed or their retention deadline has passed.
GRANT DELETE ON public.login_flows, public.sessions TO kyro_api;

CREATE POLICY login_flows_delete_maintenance ON public.login_flows
    AS RESTRICTIVE FOR DELETE TO kyro_api
    USING (consumed_at IS NOT NULL OR expires_at <= pg_catalog.clock_timestamp());

CREATE POLICY sessions_delete_maintenance ON public.sessions
    AS RESTRICTIVE FOR DELETE TO kyro_api
    USING (revoked_at IS NOT NULL OR expires_at <= pg_catalog.clock_timestamp());
