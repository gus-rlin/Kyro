-- A financial resource has one provider operation even when several callers
-- present different HTTP idempotency keys. Unknown outcomes stay in this slot.
ALTER TABLE app_connector_calls ADD COLUMN subject_key text CHECK(subject_key IS NULL OR length(subject_key) BETWEEN 1 AND 100);
CREATE UNIQUE INDEX app_connector_subject_once ON app_connector_calls(tenant_id,application_id,adapter_id,subject_key) WHERE subject_key IS NOT NULL;
