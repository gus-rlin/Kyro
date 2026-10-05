-- B122 accepts explicitly priced one-time products as well as subscriptions.
ALTER TABLE app_commerce_prices DROP CONSTRAINT app_commerce_prices_check1;
ALTER TABLE app_commerce_prices ADD CONSTRAINT app_price_interval_valid
 CHECK(interval_unit IN ('one_time','month','year') AND interval_count BETWEEN 1 AND 120);
