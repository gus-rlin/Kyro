-- Revision zero is the initial project state. Keep the composite job FK while
-- allowing jobs to reference that initial immutable specification.
ALTER TABLE public.app_revisions
    DROP CONSTRAINT app_revisions_revision_check,
    ADD CONSTRAINT app_revisions_revision_nonnegative_check CHECK (revision >= 0);

ALTER TABLE public.jobs
    DROP CONSTRAINT jobs_source_revision_check,
    ADD CONSTRAINT jobs_source_revision_nonnegative_check CHECK (source_revision >= 0);

-- Settlement updates need a read of the current aggregate row as well as
-- column-limited writes to reserved_units/spent_units.
GRANT SELECT ON public.project_budgets TO kyro_worker;

-- Only an owner of the row's actual organization may remove a membership.
CREATE POLICY memberships_owner_remove ON public.memberships FOR DELETE TO kyro_api
    USING (public.kyro_has_org_owner(organization_id, public.kyro_actor_id()));
GRANT DELETE ON public.memberships TO kyro_api;
