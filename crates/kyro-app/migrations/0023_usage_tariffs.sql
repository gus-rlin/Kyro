-- Historic receipts keep an unknown tariff; no price is reconstructed from a
-- current profile. New calls capture their public tariff before reservation.
ALTER TABLE app_connector_calls ADD COLUMN tariff_snapshot jsonb CHECK(tariff_snapshot IS NULL OR (jsonb_typeof(tariff_snapshot)='object' AND octet_length(tariff_snapshot::text)<=2048));
CREATE FUNCTION app_connector_immutable_tariff() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,public AS $$ BEGIN
 IF NEW.tariff_snapshot IS DISTINCT FROM OLD.tariff_snapshot OR NEW.profile_hash IS DISTINCT FROM OLD.profile_hash OR NEW.reserved_units<>OLD.reserved_units OR NEW.principal_id<>OLD.principal_id OR NEW.adapter_id<>OLD.adapter_id OR NEW.component_id<>OLD.component_id OR NEW.operation<>OLD.operation OR NEW.request_cipher IS DISTINCT FROM OLD.request_cipher OR NEW.tenant_id<>OLD.tenant_id OR NEW.application_id<>OLD.application_id OR NEW.id<>OLD.id THEN RAISE EXCEPTION USING ERRCODE='23514',MESSAGE='immutable connector receipt terms'; END IF;
 RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION app_connector_immutable_tariff() FROM PUBLIC;
CREATE TRIGGER immutable_tariff BEFORE UPDATE ON app_connector_calls FOR EACH ROW EXECUTE FUNCTION app_connector_immutable_tariff();
