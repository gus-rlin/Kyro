-- Before the visibility fence covered schema migration and resource archival,
-- a receipt could retain private fields while still matching the current epoch.
-- Keep the durable intention, but invalidate every pre-fix response atomically.
SELECT pg_advisory_xact_lock(hashtextextended('app-authority-global:v1', 0));
UPDATE public.app_authority_global_epoch SET revision = revision + 1 WHERE singleton;
