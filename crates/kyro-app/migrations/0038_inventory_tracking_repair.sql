-- Old draft updates could enable tracking without creating a stock balance.
-- Repair published products too, without resetting existing stock or reservations.
SELECT pg_advisory_xact_lock(hashtextextended('app-authority-global:v1',0));
INSERT INTO public.app_commerce_inventory
    (tenant_id, application_id, product_id, on_hand, reserved, version)
SELECT tenant_id, application_id, id, 0, 0, 1
FROM public.app_commerce_products WHERE inventory_tracked
ON CONFLICT (tenant_id, application_id, product_id) DO NOTHING;
