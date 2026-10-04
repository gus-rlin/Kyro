-- PL/pgSQL treats a parameter and an unqualified table column both named
-- project_id as ambiguous. Prefer the declared function argument in the
-- append helper; every table access in its body is otherwise alias-qualified.
DO $migration$
DECLARE
    definition TEXT;
    patched_definition TEXT;
    anchor TEXT := E'AS $function$\nDECLARE';
BEGIN
    SELECT pg_catalog.pg_get_functiondef(
        'public.kyro_append_event(uuid,text,jsonb)'::pg_catalog.regprocedure
    ) INTO definition;

    IF position('#variable_conflict use_variable' IN definition) = 0 THEN
        IF position(anchor IN definition) = 0 THEN
            RAISE EXCEPTION 'append-event function source did not match expected shape';
        END IF;
        patched_definition := replace(
            definition,
            anchor,
            E'AS $function$\n#variable_conflict use_variable\nDECLARE'
        );
        EXECUTE patched_definition;
    END IF;
END
$migration$;
