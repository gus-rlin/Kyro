CREATE TABLE app_notification_templates (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, version bigint NOT NULL CHECK(version>0),
 definition jsonb NOT NULL CHECK(octet_length(definition::text)<=32768), definition_hash bytea NOT NULL CHECK(octet_length(definition_hash)=32),
 approved boolean NOT NULL DEFAULT false, revoked boolean NOT NULL DEFAULT false,
 created_by uuid NOT NULL, approved_by uuid, created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id,version), FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id),
 CHECK(NOT approved OR approved_by IS NOT NULL)
);
CREATE TABLE app_notification_heads (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), recipient_id uuid NOT NULL,
 sequence bigint NOT NULL CHECK(sequence>0), purged_through bigint NOT NULL DEFAULT 0 CHECK(purged_through>=0 AND purged_through<=sequence),
 PRIMARY KEY(tenant_id,application_id,recipient_id), FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id)
);
CREATE TABLE app_notifications (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, recipient_id uuid NOT NULL, sequence bigint NOT NULL CHECK(sequence>0),
 source_kind text NOT NULL, source_id uuid NOT NULL, source_version bigint NOT NULL CHECK(source_version>0), projection_hash bytea NOT NULL CHECK(octet_length(projection_hash)=32),
 template_id uuid NOT NULL, template_version bigint NOT NULL,
 rendered jsonb NOT NULL CHECK(octet_length(rendered::text)<=65536), read_at timestamptz, expires_at timestamptz NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id), UNIQUE(tenant_id,application_id,recipient_id,sequence),
 FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id),
 FOREIGN KEY(tenant_id,recipient_id) REFERENCES app_principals(tenant_id,id),
 FOREIGN KEY(tenant_id,application_id,template_id,template_version) REFERENCES app_notification_templates(tenant_id,application_id,id,version)
);
CREATE TABLE app_channel_members (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), channel_id uuid NOT NULL, principal_id uuid NOT NULL,
 active boolean NOT NULL DEFAULT true, joined_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,channel_id,principal_id), FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id),
 FOREIGN KEY(tenant_id,principal_id) REFERENCES app_principals(tenant_id,id)
);
CREATE TABLE app_channel_messages (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, channel_id uuid NOT NULL,
 author_id uuid NOT NULL, sequence bigint GENERATED ALWAYS AS IDENTITY, body text NOT NULL CHECK(octet_length(body) BETWEEN 1 AND 8192),
 attachments uuid[] NOT NULL DEFAULT '{}', created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id), FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id), CHECK(cardinality(attachments)<=4)
);
CREATE INDEX app_channel_messages_page ON app_channel_messages(tenant_id,application_id,channel_id,sequence);
CREATE TABLE app_presence (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), channel_id uuid NOT NULL, principal_id uuid NOT NULL,
 status text NOT NULL CHECK(status IN ('available','busy','away')), expires_at timestamptz NOT NULL,
 PRIMARY KEY(tenant_id,application_id,channel_id,principal_id), FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id)
);
DO $$ DECLARE tbl text; BEGIN
 FOREACH tbl IN ARRAY ARRAY['app_notification_templates','app_notification_heads','app_channel_members'] LOOP
  EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY',tbl); EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY',tbl);
  EXECUTE format('CREATE POLICY scoped ON %I USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id())',tbl);
  EXECUTE format('GRANT SELECT,INSERT,UPDATE,DELETE ON %I TO kyro_app',tbl);
 END LOOP;
END $$;
ALTER TABLE app_notifications ENABLE ROW LEVEL SECURITY; ALTER TABLE app_notifications FORCE ROW LEVEL SECURITY;
CREATE POLICY recipient_read ON app_notifications FOR SELECT USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND recipient_id=kyro_app_actor_id());
CREATE POLICY sender_insert ON app_notifications FOR INSERT WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id());
CREATE POLICY recipient_update ON app_notifications FOR UPDATE USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND recipient_id=kyro_app_actor_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND recipient_id=kyro_app_actor_id());
CREATE POLICY recipient_delete ON app_notifications FOR DELETE USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND recipient_id=kyro_app_actor_id());
GRANT SELECT,INSERT,UPDATE,DELETE ON app_notifications TO kyro_app;
DO $$ DECLARE tbl text; BEGIN
 FOREACH tbl IN ARRAY ARRAY['app_channel_messages','app_presence'] LOOP
  EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY',tbl); EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY',tbl);
  EXECUTE format('CREATE POLICY member_read ON %I FOR SELECT USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND EXISTS(SELECT 1 FROM app_channel_members m WHERE m.channel_id=%I.channel_id AND m.principal_id=kyro_app_actor_id() AND m.active))',tbl,tbl);
  EXECUTE format('GRANT SELECT,INSERT,UPDATE,DELETE ON %I TO kyro_app',tbl);
 END LOOP;
END $$;
CREATE POLICY message_insert ON app_channel_messages FOR INSERT WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND author_id=kyro_app_actor_id() AND EXISTS(SELECT 1 FROM app_channel_members m WHERE m.channel_id=app_channel_messages.channel_id AND m.principal_id=kyro_app_actor_id() AND m.active));
CREATE POLICY presence_own ON app_presence FOR ALL USING(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id()) WITH CHECK(tenant_id=kyro_app_tenant_id() AND application_id=kyro_app_application_id() AND principal_id=kyro_app_actor_id() AND EXISTS(SELECT 1 FROM app_channel_members m WHERE m.channel_id=app_presence.channel_id AND m.principal_id=kyro_app_actor_id() AND m.active));
GRANT USAGE,SELECT ON SEQUENCE app_channel_messages_sequence_seq TO kyro_app;
CREATE TRIGGER authority_fence BEFORE INSERT OR UPDATE OR DELETE ON app_channel_members FOR EACH STATEMENT EXECUTE FUNCTION app_authority_fence();
CREATE TRIGGER authority_fence BEFORE UPDATE OR DELETE ON app_notification_templates FOR EACH STATEMENT EXECUTE FUNCTION app_authority_fence();
