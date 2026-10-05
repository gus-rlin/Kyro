CREATE TABLE app_commerce_products (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL,
 sku text NOT NULL CHECK(length(sku) BETWEEN 1 AND 96), name text NOT NULL CHECK(length(name) BETWEEN 1 AND 240), description text,
 admin_fields jsonb NOT NULL DEFAULT '{}', inventory_tracked boolean NOT NULL, status text NOT NULL CHECK(status IN ('draft','published','archived')),
 version bigint NOT NULL CHECK(version>0), created_by uuid NOT NULL, updated_by uuid NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id), UNIQUE(tenant_id,application_id,sku), FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id),
 FOREIGN KEY(tenant_id,created_by) REFERENCES app_principals(tenant_id,id), FOREIGN KEY(tenant_id,updated_by) REFERENCES app_principals(tenant_id,id)
);
CREATE TABLE app_commerce_prices (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, product_id uuid NOT NULL,
 currency text NOT NULL CHECK(currency ~ '^[A-Z]{3}$'), amount_minor bigint NOT NULL CHECK(amount_minor>=0),
 interval_unit text, interval_count integer, effective_at timestamptz NOT NULL, expires_at timestamptz, version bigint NOT NULL CHECK(version>0),
 created_by uuid NOT NULL, created_at timestamptz NOT NULL DEFAULT clock_timestamp(), PRIMARY KEY(tenant_id,application_id,id),
 UNIQUE(tenant_id,application_id,product_id,currency,version), FOREIGN KEY(tenant_id,application_id,product_id) REFERENCES app_commerce_products(tenant_id,application_id,id),
 CHECK(expires_at IS NULL OR expires_at>effective_at), CHECK((interval_unit IS NULL AND interval_count IS NULL) OR (interval_unit IN ('month','year') AND interval_count>0))
);
CREATE TABLE app_commerce_promotions (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, code text NOT NULL,
 discount_basis_points integer, fixed_discount_minor bigint, minimum_subtotal_minor bigint NOT NULL CHECK(minimum_subtotal_minor>=0),
 maximum_redemptions bigint CHECK(maximum_redemptions>0), redemption_count bigint NOT NULL DEFAULT 0 CHECK(redemption_count>=0),
 valid_from timestamptz NOT NULL, valid_until timestamptz, active boolean NOT NULL DEFAULT true,
 PRIMARY KEY(tenant_id,application_id,id), UNIQUE(tenant_id,application_id,code), FOREIGN KEY(tenant_id,application_id) REFERENCES app_applications(tenant_id,id),
 CHECK((discount_basis_points BETWEEN 1 AND 10000 AND fixed_discount_minor IS NULL) OR (discount_basis_points IS NULL AND fixed_discount_minor>0)),
 CHECK(maximum_redemptions IS NULL OR redemption_count<=maximum_redemptions), CHECK(valid_until IS NULL OR valid_until>valid_from)
);
CREATE TABLE app_commerce_quotes (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, principal_id uuid NOT NULL,
 currency text NOT NULL CHECK(currency ~ '^[A-Z]{3}$'), subtotal_minor bigint NOT NULL CHECK(subtotal_minor>=0), discount_minor bigint NOT NULL CHECK(discount_minor>=0),
 total_minor bigint NOT NULL CHECK(total_minor>=0), lines jsonb NOT NULL CHECK(jsonb_typeof(lines)='array' AND jsonb_array_length(lines) BETWEEN 1 AND 100), promotion_id uuid,
 status text NOT NULL CHECK(status IN ('open','converted')), expires_at timestamptz NOT NULL, created_by uuid NOT NULL, created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id), FOREIGN KEY(tenant_id,principal_id) REFERENCES app_principals(tenant_id,id),
 FOREIGN KEY(tenant_id,application_id,promotion_id) REFERENCES app_commerce_promotions(tenant_id,application_id,id), CHECK(total_minor=subtotal_minor-discount_minor)
);
CREATE TABLE app_commerce_orders (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, quote_id uuid NOT NULL, principal_id uuid NOT NULL,
 currency text NOT NULL, subtotal_minor bigint NOT NULL, discount_minor bigint NOT NULL, total_minor bigint NOT NULL CHECK(total_minor>=0), lines jsonb NOT NULL,
 status text NOT NULL CHECK(status IN ('awaiting_payment','paid','fulfilled','cancelled')), version bigint NOT NULL CHECK(version>0), created_by uuid NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id), UNIQUE(tenant_id,application_id,quote_id), FOREIGN KEY(tenant_id,application_id,quote_id) REFERENCES app_commerce_quotes(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,principal_id) REFERENCES app_principals(tenant_id,id), CHECK(total_minor=subtotal_minor-discount_minor)
);
CREATE TABLE app_commerce_redemptions (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), promotion_id uuid NOT NULL, principal_id uuid NOT NULL, order_id uuid NOT NULL,
 status text NOT NULL CHECK(status IN ('active','released')), created_at timestamptz NOT NULL DEFAULT clock_timestamp(), released_at timestamptz,
 PRIMARY KEY(tenant_id,application_id,promotion_id,order_id), FOREIGN KEY(tenant_id,application_id,promotion_id) REFERENCES app_commerce_promotions(tenant_id,application_id,id),
 FOREIGN KEY(tenant_id,application_id,order_id) REFERENCES app_commerce_orders(tenant_id,application_id,id)
);
CREATE TABLE app_commerce_inventory (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), product_id uuid NOT NULL,
 on_hand bigint NOT NULL CHECK(on_hand>=0), reserved bigint NOT NULL CHECK(reserved>=0), version bigint NOT NULL CHECK(version>0),
 updated_at timestamptz NOT NULL DEFAULT clock_timestamp(), PRIMARY KEY(tenant_id,application_id,product_id),
 FOREIGN KEY(tenant_id,application_id,product_id) REFERENCES app_commerce_products(tenant_id,application_id,id), CHECK(reserved<=on_hand)
);
CREATE TABLE app_commerce_inventory_reservations (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, order_id uuid NOT NULL, product_id uuid NOT NULL,
 quantity bigint NOT NULL CHECK(quantity>0), status text NOT NULL CHECK(status IN ('reserved','released','consumed')), created_at timestamptz NOT NULL DEFAULT clock_timestamp(), released_at timestamptz,
 PRIMARY KEY(tenant_id,application_id,id), UNIQUE(tenant_id,application_id,order_id,product_id),
 FOREIGN KEY(tenant_id,application_id,order_id) REFERENCES app_commerce_orders(tenant_id,application_id,id), FOREIGN KEY(tenant_id,application_id,product_id) REFERENCES app_commerce_products(tenant_id,application_id,id)
);
CREATE TABLE app_commerce_inventory_ledger (
 tenant_id uuid NOT NULL, application_id uuid NOT NULL DEFAULT kyro_app_application_id(), id uuid NOT NULL, product_id uuid NOT NULL, order_id uuid,
 kind text NOT NULL CHECK(kind IN ('adjust','reserve','release','consume')), delta_on_hand bigint NOT NULL, delta_reserved bigint NOT NULL,
 principal_id uuid NOT NULL, idempotency_key text NOT NULL, created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant_id,application_id,id), UNIQUE(tenant_id,application_id,product_id,idempotency_key),
 FOREIGN KEY(tenant_id,application_id,product_id) REFERENCES app_commerce_products(tenant_id,application_id,id), FOREIGN KEY(tenant_id,application_id,order_id) REFERENCES app_commerce_orders(tenant_id,application_id,id)
);
DO $migration$
DECLARE table_name text;
BEGIN
 FOREACH table_name IN ARRAY ARRAY['app_commerce_products','app_commerce_prices','app_commerce_promotions','app_commerce_quotes','app_commerce_orders','app_commerce_redemptions','app_commerce_inventory','app_commerce_inventory_reservations','app_commerce_inventory_ledger'] LOOP
  EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY',table_name);
  EXECUTE format('ALTER TABLE public.%I FORCE ROW LEVEL SECURITY',table_name);
  EXECUTE format('CREATE POLICY app_scope ON public.%I TO kyro_app USING(tenant_id=public.kyro_app_tenant_id() AND application_id=public.kyro_app_application_id()) WITH CHECK(tenant_id=public.kyro_app_tenant_id() AND application_id=public.kyro_app_application_id())',table_name);
  EXECUTE format('GRANT SELECT,INSERT,UPDATE ON public.%I TO kyro_app',table_name);
 END LOOP;
END $migration$;
REVOKE UPDATE ON app_commerce_inventory_ledger FROM kyro_app;
