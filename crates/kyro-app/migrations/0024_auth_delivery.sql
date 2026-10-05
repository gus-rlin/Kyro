-- A committed sending state is never retried. Empty terminal ciphertext means
-- the one-use link is no longer retained in a delivery copy.
ALTER TABLE app_auth_deliveries
 ADD COLUMN started_at timestamptz,
 ADD COLUMN settled_at timestamptz,
 ADD COLUMN provider_receipt uuid,
 ADD COLUMN error_code text CHECK(error_code IS NULL OR error_code IN
 ('delivery_interrupted','credential_inactive','delivery_payload_invalid',
  'delivery_before_send','delivery_rejected','delivery_unknown'));
CREATE INDEX app_auth_delivery_pending_idx
 ON app_auth_deliveries(tenant_id,application_id,created_at,id)
 WHERE state='pending';
