ALTER TABLE app_document_usage ADD COLUMN reserved_files bigint NOT NULL DEFAULT 0 CHECK(reserved_files>=0);
ALTER TABLE app_document_usage ADD COLUMN reserved_bytes bigint NOT NULL DEFAULT 0 CHECK(reserved_bytes>=0);
ALTER TABLE app_document_usage ADD CHECK(file_count+reserved_files<=file_limit AND bytes_used+reserved_bytes<=byte_limit);
CREATE TABLE app_document_uploads (
 tenant_id uuid NOT NULL,application_id uuid NOT NULL DEFAULT kyro_app_application_id(),id uuid NOT NULL,
 principal_id uuid NOT NULL DEFAULT kyro_app_actor_id(),session_id uuid NOT NULL DEFAULT kyro_app_session_id(),
 filename text NOT NULL,media_type text NOT NULL,expected_size integer NOT NULL CHECK(expected_size BETWEEN 1 AND 5242880),
 expected_hash bytea NOT NULL CHECK(octet_length(expected_hash)=32),content bytea NOT NULL DEFAULT ''::bytea,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),expires_at timestamptz NOT NULL DEFAULT clock_timestamp()+interval '10 minutes',
 PRIMARY KEY(tenant_id,application_id,id),FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id),
 FOREIGN KEY(tenant_id,application_id,session_id) REFERENCES app_sessions(tenant_id,application_id,id),
 CHECK(octet_length(content)<=expected_size),CHECK(expires_at<=created_at+interval '10 minutes')
);
ALTER TABLE app_document_uploads ENABLE ROW LEVEL SECURITY;ALTER TABLE app_document_uploads FORCE ROW LEVEL SECURITY;
CREATE POLICY own_upload ON app_document_uploads TO kyro_app
 USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id())
 WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id());
GRANT SELECT,INSERT,UPDATE,DELETE ON app_document_uploads TO kyro_app;
ALTER TABLE app_document_download_tokens ADD COLUMN next_offset bigint NOT NULL DEFAULT 0 CHECK(next_offset BETWEEN 0 AND 5242880);
ALTER TABLE app_document_download_tokens ADD COLUMN completed_at timestamptz;
