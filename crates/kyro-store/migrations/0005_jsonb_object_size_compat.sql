-- PostgreSQL provides jsonb_object_keys but not jsonb_object_length.
-- This local immutable helper supports the bounded event payload check used
-- by the already-versioned append function without changing older checksums.
CREATE FUNCTION public.jsonb_object_length(value JSONB) RETURNS INTEGER
LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE SET search_path = pg_catalog
AS $$
    SELECT count(*)::INTEGER FROM pg_catalog.jsonb_object_keys(value)
$$;
REVOKE EXECUTE ON FUNCTION public.jsonb_object_length(JSONB) FROM PUBLIC;
