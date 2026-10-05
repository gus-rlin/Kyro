-- A cached command result is private data. Its intention remains durable even
-- when a later authority change makes the original projection unsafe to replay.
CREATE TABLE public.app_authority_global_epoch (
    singleton boolean PRIMARY KEY CHECK (singleton),
    revision bigint NOT NULL CHECK (revision >= 0)
);
INSERT INTO public.app_authority_global_epoch VALUES (true, 0);
ALTER TABLE public.app_authority_global_epoch ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_authority_global_epoch FORCE ROW LEVEL SECURITY;
CREATE POLICY authority_global_read ON public.app_authority_global_epoch FOR SELECT TO kyro_app USING (true);

CREATE TABLE public.app_authority_epochs (
    tenant_id uuid NOT NULL,
    application_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision >= 0),
    PRIMARY KEY (tenant_id, application_id),
    FOREIGN KEY (tenant_id, application_id) REFERENCES public.app_applications(tenant_id, id) ON DELETE CASCADE
);
ALTER TABLE public.app_authority_epochs ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.app_authority_epochs FORCE ROW LEVEL SECURITY;
CREATE POLICY authority_epoch_read ON public.app_authority_epochs FOR SELECT TO kyro_app
USING (tenant_id = public.kyro_app_tenant_id() AND application_id = public.kyro_app_application_id());
GRANT SELECT ON public.app_authority_global_epoch, public.app_authority_epochs TO kyro_app;

ALTER TABLE public.app_idempotency
    ADD COLUMN authority_global_epoch bigint CHECK (authority_global_epoch >= 0),
    ADD COLUMN authority_epoch bigint CHECK (authority_epoch >= 0),
    ADD COLUMN authorization_digest bytea CHECK (octet_length(authorization_digest) = 32);
-- Existing receipts deliberately remain unbound: they must never disclose an
-- old projection or cause a second execution after upgrading from 0035.

CREATE FUNCTION public.app_advance_authority_epoch() RETURNS void LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, public AS $$
DECLARE t uuid := public.kyro_app_tenant_id(); a uuid := public.kyro_app_application_id();
BEGIN
    IF t IS NULL OR a IS NULL OR public.kyro_app_actor_id() IS NULL OR public.kyro_app_session_id() IS NULL THEN
        RAISE EXCEPTION 'application context required' USING ERRCODE = '42501';
    END IF;
    INSERT INTO public.app_authority_epochs(tenant_id, application_id, revision) VALUES (t, a, 1)
    ON CONFLICT (tenant_id, application_id) DO UPDATE SET revision = app_authority_epochs.revision + 1;
END $$;
REVOKE ALL ON FUNCTION public.app_advance_authority_epoch() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.app_advance_authority_epoch() TO kyro_app;

CREATE OR REPLACE FUNCTION public.app_authority_fence() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, public AS $$
DECLARE t uuid := public.kyro_app_tenant_id(); a uuid := public.kyro_app_application_id();
BEGIN
    IF t IS NULL OR a IS NULL OR TG_TABLE_NAME IN ('app_principals','app_tenants','app_applications') THEN
        PERFORM pg_advisory_xact_lock(hashtextextended('app-authority-global:v1',0));
        UPDATE public.app_authority_global_epoch SET revision = revision + 1 WHERE singleton;
    ELSE
        PERFORM pg_advisory_xact_lock_shared(hashtextextended('app-authority-global:v1',0));
        PERFORM pg_advisory_xact_lock(hashtextextended('app-authority:'||t::text||':'||a::text,0));
        INSERT INTO public.app_authority_epochs(tenant_id, application_id, revision) VALUES (t,a,1)
        ON CONFLICT (tenant_id,application_id) DO UPDATE SET revision = app_authority_epochs.revision + 1;
    END IF;
    RETURN NULL;
END $$;

-- Maintenance SQL can change embedded ownership, teams or field visibility in
-- any domain table. Unscoped maintenance takes the global fence before tuples;
-- scoped runtime mutations retain the existing application-before-row order.
CREATE FUNCTION public.app_operator_receipt_fence() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, public AS $$
BEGIN
    IF public.kyro_app_tenant_id() IS NULL OR public.kyro_app_application_id() IS NULL THEN
        PERFORM pg_advisory_xact_lock(hashtextextended('app-authority-global:v1',0));
        UPDATE public.app_authority_global_epoch SET revision = revision + 1 WHERE singleton;
    END IF;
    RETURN NULL;
END $$;
REVOKE ALL ON FUNCTION public.app_operator_receipt_fence() FROM PUBLIC;
DO $$ DECLARE tbl text; BEGIN
    FOR tbl IN SELECT c.relname FROM pg_class c WHERE c.relnamespace = 'public'::regnamespace
        AND c.relkind = 'r' AND c.relname LIKE 'app\_%' ESCAPE '\'
        AND c.relname NOT IN ('app_authority_epochs','app_authority_global_epoch')
        AND NOT EXISTS (SELECT 1 FROM pg_trigger tr WHERE tr.tgrelid = c.oid AND tr.tgname = 'authority_fence')
    LOOP
        EXECUTE format('CREATE TRIGGER operator_receipt_fence BEFORE INSERT OR UPDATE OR DELETE ON public.%I FOR EACH STATEMENT EXECUTE FUNCTION public.app_operator_receipt_fence()',tbl);
    END LOOP;
END $$;
