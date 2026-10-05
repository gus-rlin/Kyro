-- Bearers were returned at the top level by invitation, preview and document
-- ticket issuance. Preserve the durable command receipt and its hash while
-- removing historical bearer values; no command is executed again.
UPDATE public.app_idempotency
SET response = (response - 'token') || jsonb_build_object('secret_not_replayed', true)
WHERE component_id IN ('B013', 'B020', 'B083') AND response ? 'token';
