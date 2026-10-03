-- Require every durable external effect to name its budget reservation.
-- The reciprocal foreign keys stay deferred, allowing both rows to be created
-- atomically in either order within the same transaction.
ALTER TABLE public.effects
    ALTER COLUMN reservation_id SET NOT NULL;
