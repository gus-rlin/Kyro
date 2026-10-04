[CmdletBinding()]
param(
    [ValidateSet('Local', 'Production')]
    [string]$TargetEnvironment = 'Local',
    [Parameter(Mandatory)][string]$BackupFile,
    [Parameter(Mandatory)][ValidatePattern('^kyro_restore_[a-z0-9_]{1,50}$')][string]$TargetDatabase,
    [ValidatePattern('^[a-z][a-z0-9_]{0,62}$')][string]$SourceDatabase,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedSha256,
    [string]$ProductionEnvFile
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot)).TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
$expectedImage = 'postgres:18.6-alpine@sha256:77f585114c32fbca283dc835b0596f4e52b51b4c6662d7810b2f4084f60a1873'
$volumeName = if ($TargetEnvironment -eq 'Local') { 'kyro-p1-ops_postgres-data' } else { 'kyro-p1-production_postgres-data' }
$projectName = if ($TargetEnvironment -eq 'Local') { 'kyro-p1-ops' } else { 'kyro-p1-production' }
$containerName = if ($TargetEnvironment -eq 'Local') { 'kyro-p1-ops-postgres-1' } else { 'kyro-p1-production-postgres-1' }
$composeFile = Join-Path $repoRoot $(if ($TargetEnvironment -eq 'Local') { 'compose.p1.yaml' } else { 'compose.p1.production.yaml' })
$envFile = Join-Path $repoRoot $(if ($TargetEnvironment -eq 'Local') { '.env.p1.local.example' } else { '.env.p1.production.example' })
$containerId = $null
$remoteArchivePath = "/tmp/kyro-p1-restore-$([Guid]::NewGuid().ToString('N')).dump"
$outputFullPath = [IO.Path]::GetFullPath($BackupFile)

function Invoke-Docker {
    param([Parameter(Mandatory)][string[]]$Arguments)

    $output = & docker @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "Docker operation failed (exit $LASTEXITCODE). Output is suppressed to avoid logging database contents."
    }
    return $output
}

function Invoke-AdminSql {
    param(
        [Parameter(Mandatory)][string]$Database,
        [Parameter(Mandatory)][string]$Sql
    )

    $remoteSqlPath = "/tmp/kyro-p1-restore-$([Guid]::NewGuid().ToString('N')).sql"
    $localSqlPath = Join-Path $env:TEMP ("kyro-p1-restore-$([Guid]::NewGuid().ToString('N')).sql")
    $utf8 = [Text.UTF8Encoding]::new($false)
    [IO.File]::WriteAllText($localSqlPath, $Sql, $utf8)

    try {
        Invoke-Docker -Arguments @('cp', $localSqlPath, "$($containerId):$remoteSqlPath") | Out-Null
        $shell = 'if [ -r /run/secrets/postgres_admin_password ]; then password=$(cat /run/secrets/postgres_admin_password); export PGPASSWORD="$password"; unset password; fi; exec psql -X -q --set=ON_ERROR_STOP=1 --no-align --tuples-only --username="' + $script:databaseUser + '" --dbname="' + $Database + '" --file="' + $remoteSqlPath + '"'
        return Invoke-Docker -Arguments @('exec', $containerId, 'sh', '-ec', $shell)
    }
    finally {
        if ($containerId) {
            & docker exec $containerId sh -ec "rm -f $remoteSqlPath" 2>$null | Out-Null
        }
        [IO.File]::Delete($localSqlPath)
    }
}

function Assert-Schema {
    param([Parameter(Mandatory)][string]$Database)

    $requiredColumns = @'
WITH required(table_name, column_name) AS (
    VALUES
        ('actors', 'id'), ('actors', 'issuer'), ('actors', 'subject'),
        ('organizations', 'id'), ('organizations', 'name'), ('organizations', 'created_by'), ('organizations', 'created_at'),
        ('memberships', 'organization_id'), ('memberships', 'actor_id'), ('memberships', 'role'), ('memberships', 'created_by'), ('memberships', 'created_at'),
        ('projects', 'id'), ('projects', 'current_revision'), ('projects', 'event_sequence'),
        ('capability_grants', 'id'), ('capability_grants', 'project_id'), ('capability_grants', 'limits'),
        ('app_revisions', 'project_id'), ('app_revisions', 'revision'),
        ('change_commands', 'project_id'), ('change_commands', 'idempotency_key'), ('change_commands', 'fingerprint'), ('change_commands', 'result'), ('change_commands', 'command_id'), ('change_commands', 'created_at'),
        ('decisions', 'id'), ('decisions', 'project_id'), ('decisions', 'revision'), ('decisions', 'actor_id'), ('decisions', 'kind'), ('decisions', 'payload'), ('decisions', 'created_at'),
        ('jobs', 'id'), ('jobs', 'status'), ('jobs', 'environment'), ('jobs', 'generation'),
        ('jobs', 'lease_owner'), ('jobs', 'lease_until'), ('jobs', 'cancel_requested'),
        ('jobs', 'result'), ('jobs', 'error_code'), ('jobs', 'attempts'), ('jobs', 'max_attempts'), ('jobs', 'deadline'), ('jobs', 'updated_at'),
        ('effects', 'id'), ('effects', 'job_id'), ('effects', 'generation'), ('effects', 'status'),
        ('effects', 'destination'), ('effects', 'fingerprint'), ('effects', 'intent'), ('effects', 'result'), ('effects', 'reservation_id'), ('effects', 'updated_at'),
        ('budget_reservations', 'id'), ('budget_reservations', 'project_id'), ('budget_reservations', 'job_id'), ('budget_reservations', 'effect_id'),
        ('budget_reservations', 'idempotency_key'), ('budget_reservations', 'units'), ('budget_reservations', 'status'), ('budget_reservations', 'expires_at'), ('budget_reservations', 'created_at'), ('budget_reservations', 'updated_at'),
        ('project_budgets', 'project_id'), ('project_budgets', 'reserved_units'), ('project_budgets', 'spent_units'),
        ('project_budgets', 'limit_units'), ('project_budgets', 'currency'), ('project_budgets', 'unit_scale'), ('project_budgets', 'updated_at'),
        ('usage_ledger', 'id'), ('usage_ledger', 'project_id'), ('usage_ledger', 'job_id'), ('usage_ledger', 'reservation_id'),
        ('usage_ledger', 'units'), ('usage_ledger', 'kind'), ('usage_ledger', 'provider'), ('usage_ledger', 'model'), ('usage_ledger', 'metadata'), ('usage_ledger', 'recorded_at'),
        ('events', 'project_id'), ('events', 'sequence'), ('events', 'type'), ('events', 'payload'), ('events', 'actor_id'), ('events', 'created_at'),
        ('outbox_events', 'id'), ('outbox_events', 'project_id'), ('outbox_events', 'event_sequence'), ('outbox_events', 'topic'),
        ('outbox_events', 'payload'), ('outbox_events', 'available_at'), ('outbox_events', 'delivered_at'), ('outbox_events', 'attempts'), ('outbox_events', 'created_at'),
        ('sessions', 'id'), ('sessions', 'token_hash'), ('sessions', 'csrf_hash'), ('sessions', 'expires_at'), ('sessions', 'revoked_at'), ('sessions', 'created_at'),
        ('login_flows', 'id'),
        ('runtime_control', 'id'), ('runtime_control', 'external_sends_enabled'), ('runtime_control', 'updated_at')
)
SELECT string_agg(required.table_name || '.' || required.column_name, ', ' ORDER BY required.table_name, required.column_name)
FROM required
LEFT JOIN information_schema.columns actual
    ON actual.table_schema = 'public'
   AND actual.table_name = required.table_name
   AND actual.column_name = required.column_name
WHERE actual.column_name IS NULL;
'@
    $missing = (Invoke-AdminSql -Database $Database -Sql $requiredColumns | ForEach-Object { ([string]$_).Trim() } | Where-Object { $_ }) -join ', '
    if ($missing) {
        throw "Restored target schema does not satisfy the P1 restore contract; missing columns: $missing. The target database was retained and no workers were started."
    }
}

function Get-IntegrityManifest {
    param([Parameter(Mandatory)][string]$Database)

    $sql = @'
SELECT 'actors|' || count(*)::text || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, issuer, subject, created_at FROM public.actors) q
UNION ALL SELECT 'organizations|' || count(*)::text || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, name, created_by, created_at FROM public.organizations) q
UNION ALL SELECT 'memberships|' || count(*)::text || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.organization_id, q.actor_id)), md5('')) FROM (SELECT organization_id, actor_id, role, created_by, created_at FROM public.memberships) q
UNION ALL SELECT 'projects|' || count(*)::text || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, organization_id, name, current_revision, event_sequence, data_policy, limits, created_by, created_at, updated_at FROM public.projects) q
UNION ALL SELECT 'capability_grants|' || count(*)::text || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, actor_id, project_id, actions, resources, environment, limits, expires_at, revoked_at, created_by, created_at FROM public.capability_grants) q
UNION ALL SELECT 'app_revisions|' || count(*)::text || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.project_id, q.revision)), md5('')) FROM (SELECT project_id, revision, spec, created_by, created_at FROM public.app_revisions) q
UNION ALL SELECT 'change_commands|' || count(*)::text || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.project_id, q.idempotency_key)), md5('')) FROM (SELECT project_id, idempotency_key, fingerprint, result, command_id, created_at FROM public.change_commands) q
UNION ALL SELECT 'decisions|' || count(*)::text || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, project_id, revision, actor_id, kind, payload, created_at FROM public.decisions) q
UNION ALL SELECT 'jobs|' || count(*)::text || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, project_id, actor_id, environment, source_revision, payload, attempts, max_attempts, deadline, cancel_requested, created_at FROM public.jobs) q
UNION ALL SELECT 'effects|' || count(*)::text || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, job_id, project_id, generation, destination, fingerprint, intent, result, reservation_id, created_at FROM public.effects) q
UNION ALL SELECT 'budget_reservations|' || count(*)::text || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, project_id, job_id, effect_id, idempotency_key, units, status, expires_at, created_at, updated_at FROM public.budget_reservations) q
UNION ALL SELECT 'project_budgets|' || count(*)::text || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.project_id)), md5('')) FROM (SELECT project_id, limit_units, reserved_units, spent_units, currency, unit_scale, updated_at FROM public.project_budgets) q
UNION ALL SELECT 'usage_ledger|' || count(*)::text || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, project_id, job_id, reservation_id, units, kind, provider, model, metadata, recorded_at FROM public.usage_ledger) q
UNION ALL SELECT 'events|' || count(*)::text || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.project_id, q.sequence)), md5('')) FROM (SELECT project_id, sequence, type, payload, actor_id, created_at FROM public.events) q
UNION ALL SELECT 'outbox_events|' || count(*)::text || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, project_id, event_sequence, topic, payload, available_at, delivered_at, attempts, created_at FROM public.outbox_events) q
UNION ALL SELECT 'sessions|' || count(*)::text || '|' || COALESCE(md5(string_agg(md5(to_jsonb(q)::text), '' ORDER BY q.id)), md5('')) FROM (SELECT id, token_hash, actor_id, csrf_hash, expires_at, created_at FROM public.sessions) q;
'@
    $rows = Invoke-AdminSql -Database $Database -Sql $sql
    $manifest = @{}
    foreach ($row in $rows) {
        $parts = ([string]$row).Trim() -split '\|'
        if ($parts.Count -ne 3 -or $parts[1] -notmatch '^\d+$' -or $parts[2] -notmatch '^[a-f0-9]{32}$') {
            throw 'Could not read the redacted source/target integrity manifest.'
        }
        $manifest[$parts[0]] = "$($parts[1])|$($parts[2])"
    }
    if ($manifest.Count -ne 16) {
        throw 'The source/target integrity manifest was incomplete.'
    }
    return $manifest
}

if ($TargetEnvironment -eq 'Production') {
    if (-not $ProductionEnvFile -or -not (Test-Path -LiteralPath $ProductionEnvFile -PathType Leaf)) {
        throw 'Production restore requires -ProductionEnvFile with deployment paths; secret values must remain in mounted files.'
    }
    $envFile = (Resolve-Path -LiteralPath $ProductionEnvFile).Path
}
elseif ($ProductionEnvFile) {
    throw '-ProductionEnvFile is valid only with -TargetEnvironment Production.'
}

if (-not (Test-Path -LiteralPath $composeFile -PathType Leaf) -or -not (Test-Path -LiteralPath $envFile -PathType Leaf)) {
    throw 'The selected Compose or environment file is missing.'
}
if (-not (Test-Path -LiteralPath $outputFullPath -PathType Leaf)) {
    throw 'The backup archive does not exist.'
}
$pathComparison = if ([IO.Path]::DirectorySeparatorChar -eq '\') { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
if ($outputFullPath.StartsWith($repoRoot + [IO.Path]::DirectorySeparatorChar, $pathComparison) -or
    [string]::Equals($outputFullPath, $repoRoot, $pathComparison)) {
    throw 'Restore archives must be read from outside the repository.'
}
if ((Get-Item -LiteralPath $outputFullPath).Length -eq 0) {
    throw 'The backup archive is empty.'
}
$actualSha256 = (Get-FileHash -LiteralPath $outputFullPath -Algorithm SHA256).Hash
if ($actualSha256 -ne $ExpectedSha256.ToUpperInvariant()) {
    throw 'Backup SHA-256 does not match -ExpectedSha256; no database was created.'
}

$composeArgs = @('compose', '--project-name', $projectName, '--env-file', $envFile, '--file', $composeFile)
$containerJson = Invoke-Docker -Arguments @('inspect', $containerName)
$container = @((($containerJson -join [Environment]::NewLine) | ConvertFrom-Json))[0]
$containerId = $container.Id
$labels = $container.Config.Labels
$state = "$($container.State.Status)|$($container.State.Health.Status)"
$dataMount = @($container.Mounts | Where-Object { $_.Destination -eq '/var/lib/postgresql' })
if ($state -ne 'running|healthy' -or
    $labels.'com.docker.compose.project' -ne $projectName -or
    $labels.'com.docker.compose.service' -ne 'postgres' -or
    $container.Config.Image -ne $expectedImage -or
    $dataMount.Count -ne 1 -or
    $dataMount[0].Type -ne 'volume' -or
    $dataMount[0].Name -ne $volumeName) {
    throw "PostgreSQL container '$containerName' is not healthy or does not belong to the selected P1 project; it was left untouched."
}

$containerEnvironment = @{}
foreach ($entry in $container.Config.Env) {
    $separator = $entry.IndexOf('=')
    if ($separator -gt 0) {
        $containerEnvironment[$entry.Substring(0, $separator)] = $entry.Substring($separator + 1)
    }
}
$script:databaseUser = $containerEnvironment['POSTGRES_USER']
if (-not $script:databaseUser -or $script:databaseUser -notmatch '^[a-z][a-z0-9_]{0,62}$') {
    throw 'The selected container does not have a valid database administrator user.'
}
if ($TargetEnvironment -eq 'Local') {
    $portBindings = @($container.HostConfig.PortBindings.'5432/tcp')
    if ($containerEnvironment['POSTGRES_DB'] -ne 'kyro_p1' -or
        $script:databaseUser -ne 'kyro_admin' -or
        $portBindings.Count -ne 1 -or
        $portBindings[0].HostIp -ne '127.0.0.1') {
        throw 'The selected local container does not match the dedicated synthetic P1 instance.'
    }
}
else {
    $secretMount = @($container.Mounts | Where-Object { $_.Destination -eq '/run/secrets/postgres_admin_password' })
    $tlsMount = @($container.Mounts | Where-Object { $_.Destination -eq '/run/postgres/tls' })
    $hbaMount = @($container.Mounts | Where-Object { $_.Destination -eq '/etc/postgresql/pg_hba.conf' })
    $publishedPorts = @($container.HostConfig.PortBindings.PSObject.Properties | Where-Object { @($_.Value | Where-Object { $_ }).Count -gt 0 })
    $commandText = [string]::Join(' ', $container.Config.Cmd)
    if ($secretMount.Count -ne 1 -or $secretMount[0].RW -or
        $tlsMount.Count -ne 1 -or $tlsMount[0].RW -or
        $hbaMount.Count -ne 1 -or $hbaMount[0].RW -or
        $publishedPorts.Count -ne 0 -or
        $containerEnvironment['POSTGRES_PASSWORD_FILE'] -ne '/run/secrets/postgres_admin_password' -or
        $containerEnvironment['POSTGRES_INITDB_ARGS'] -notmatch '--auth-host=scram-sha-256' -or
        $containerEnvironment['POSTGRES_HOST_AUTH_METHOD'] -or
        $commandText -notmatch 'ssl=on' -or
        $commandText -notmatch 'ssl_cert_file=/run/postgres/tls/server.crt' -or
        $commandText -notmatch 'ssl_key_file=/run/postgres/tls/server.key' -or
        $commandText -notmatch 'hba_file=/etc/postgresql/pg_hba.conf') {
        throw 'Production restore requires SCRAM, TLS, read-only secret/configuration mounts and no host-published PostgreSQL port.'
    }
}

$composeContainerIds = Invoke-Docker -Arguments ($composeArgs + @('ps', '--all', '--quiet', 'postgres'))
$composeIds = @($composeContainerIds | ForEach-Object { ([string]$_).Trim() } | Where-Object { $_ })
if ($composeIds.Count -ne 1 -or -not $containerId.StartsWith($composeIds[0], [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Compose did not resolve exactly the managed PostgreSQL container; no service was started.'
}

if ($SourceDatabase -and $SourceDatabase -eq $TargetDatabase) {
    throw 'Source and target database names must differ.'
}
$adminState = (Invoke-AdminSql -Database 'postgres' -Sql "SELECT CASE WHEN r.rolsuper THEN 'ok' ELSE 'invalid' END FROM pg_catalog.pg_roles r WHERE r.rolname = current_user;") -join ''
if ($adminState.Trim() -ne 'ok') {
    throw 'The selected database administrator is not a superuser; restore was not started.'
}
$runtimeRoles = (Invoke-AdminSql -Database 'postgres' -Sql "SELECT count(*) FROM pg_catalog.pg_roles WHERE rolname IN ('kyro_api', 'kyro_worker') AND rolcanlogin AND NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole AND NOT rolinherit AND NOT rolreplication AND NOT rolbypassrls;") -join ''
if ($runtimeRoles.Trim() -ne '2') {
    throw 'Expected globally existing kyro_api and kyro_worker login roles without administrative privileges; restore was not started.'
}
$targetExists = (Invoke-AdminSql -Database 'postgres' -Sql "SELECT CASE WHEN EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datname = '$TargetDatabase') THEN 'yes' ELSE 'no' END;") -join ''
if ($targetExists.Trim() -ne 'no') {
    throw "Target database '$TargetDatabase' already exists; restore never drops or cleans an existing database."
}
if ($SourceDatabase) {
    $sourceExists = (Invoke-AdminSql -Database 'postgres' -Sql "SELECT CASE WHEN EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datname = '$SourceDatabase') THEN 'yes' ELSE 'no' END;") -join ''
    if ($sourceExists.Trim() -ne 'yes') {
        throw "Source database '$SourceDatabase' does not exist on this PostgreSQL service."
    }
    Assert-Schema -Database $SourceDatabase
}

$sourceManifest = $null
if ($SourceDatabase) {
    # Stop writers while making this comparison; the dump itself is a consistent snapshot.
    $sourceManifest = Get-IntegrityManifest -Database $SourceDatabase
}

$remoteArchivePath = "/tmp/kyro-p1-restore-$([Guid]::NewGuid().ToString('N')).dump"
Invoke-Docker -Arguments @('cp', $outputFullPath, "$($containerId):$remoteArchivePath") | Out-Null
try {
    Invoke-Docker -Arguments @('exec', $containerId, 'pg_restore', '--list', $remoteArchivePath) | Out-Null
    $databaseExistsSql = "SELECT CASE WHEN EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datname = '$TargetDatabase') THEN 'yes' ELSE 'no' END;"
    if (((Invoke-AdminSql -Database 'postgres' -Sql $databaseExistsSql) -join '').Trim() -ne 'no') {
        throw "Target database '$TargetDatabase' appeared during preflight; it was not changed."
    }
    $createDatabaseSql = 'CREATE DATABASE "' + $TargetDatabase + '" WITH OWNER "' + $script:databaseUser + '" TEMPLATE template0 ENCODING ''UTF8'';'
    Invoke-AdminSql -Database 'postgres' -Sql $createDatabaseSql | Out-Null

    $restoreShell = 'if [ -r /run/secrets/postgres_admin_password ]; then password=$(cat /run/secrets/postgres_admin_password); export PGPASSWORD="$password"; unset password; fi; exec pg_restore --no-owner --exit-on-error --single-transaction --username="' + $script:databaseUser + '" --dbname="' + $TargetDatabase + '" "' + $remoteArchivePath + '"'
    Invoke-Docker -Arguments @('exec', $containerId, 'sh', '-ec', $restoreShell) | Out-Null

    # Validate the restored schema before applying any recovery SQL.
    Assert-Schema -Database $TargetDatabase

    $disableSendsSql = @'
BEGIN;
DO $restore$
DECLARE changed_rows INTEGER;
BEGIN
    UPDATE public.runtime_control
       SET external_sends_enabled = FALSE, updated_at = clock_timestamp()
     WHERE id = 1;
    GET DIAGNOSTICS changed_rows = ROW_COUNT;
    IF changed_rows <> 1 THEN
        RAISE EXCEPTION 'runtime control singleton missing or duplicated';
    END IF;
END
$restore$;
COMMIT;
'@
    Invoke-AdminSql -Database $TargetDatabase -Sql $disableSendsSql | Out-Null

    $recoverySql = @'
BEGIN;
DO $preflight$
BEGIN
    IF EXISTS (
        SELECT 1 FROM public.jobs j
        LEFT JOIN public.effects e ON e.job_id = j.id
        WHERE j.status IN ('pending', 'running')
          AND e.job_id IS NOT NULL
          AND (e.status = 'succeeded'
               OR (e.status = 'prepared' AND j.deadline > statement_timestamp() AND j.attempts < j.max_attempts))
          AND j.generation = 9223372036854775807
    ) OR EXISTS (
        SELECT 1 FROM public.jobs j
        WHERE j.status = 'running'
          AND NOT EXISTS (SELECT 1 FROM public.effects e WHERE e.job_id = j.id)
          AND j.generation = 9223372036854775807
    ) THEN
        RAISE EXCEPTION 'job generation cannot be advanced safely';
    END IF;
END
$preflight$;

UPDATE public.effects
   SET status = 'unknown', updated_at = clock_timestamp()
 WHERE status = 'sending';

UPDATE public.jobs j
   SET status = 'unknown',
       result = jsonb_build_object('effect_id', e.id, 'status', 'unknown'),
       error_code = 'lease_lost', lease_owner = NULL, lease_until = NULL,
       updated_at = clock_timestamp()
  FROM public.effects e
 WHERE e.job_id = j.id AND e.status = 'unknown';

UPDATE public.jobs j
   SET status = 'pending',
       generation = j.generation + 1,
       result = NULL, error_code = NULL, lease_owner = NULL, lease_until = NULL,
       updated_at = clock_timestamp()
  FROM public.effects e
 WHERE e.job_id = j.id AND e.status = 'succeeded' AND j.status IN ('pending', 'running');

UPDATE public.jobs j
   SET status = CASE WHEN e.status = 'failed' THEN 'failed' ELSE 'cancelled' END,
       result = NULL,
       error_code = CASE WHEN e.status = 'failed' THEN 'execution_failed' ELSE 'cancelled' END,
       lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
  FROM public.effects e
 WHERE e.job_id = j.id AND e.status IN ('failed', 'cancelled') AND j.status IN ('pending', 'running');

UPDATE public.jobs j
   SET status = CASE
           WHEN j.deadline <= statement_timestamp() THEN 'failed'
           WHEN j.attempts >= j.max_attempts THEN 'failed'
           ELSE 'pending'
       END,
       generation = CASE
           WHEN j.deadline <= statement_timestamp() OR j.attempts >= j.max_attempts THEN j.generation
           ELSE j.generation + 1
       END,
       result = NULL,
       error_code = CASE
           WHEN j.deadline <= statement_timestamp() THEN 'deadline_expired'
           WHEN j.attempts >= j.max_attempts THEN 'attempts_exceeded'
           ELSE NULL
       END,
       lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
  FROM public.effects e
 WHERE e.job_id = j.id AND e.status = 'prepared' AND j.status IN ('pending', 'running');

UPDATE public.jobs j
   SET status = CASE
           WHEN j.deadline <= statement_timestamp() THEN 'failed'
           WHEN j.attempts >= j.max_attempts THEN 'failed'
           ELSE 'pending'
       END,
       generation = CASE
           WHEN j.deadline <= statement_timestamp() OR j.attempts >= j.max_attempts THEN j.generation
           ELSE j.generation + 1
       END,
       result = NULL,
       error_code = CASE
           WHEN j.deadline <= statement_timestamp() THEN 'deadline_expired'
           WHEN j.attempts >= j.max_attempts THEN 'attempts_exceeded'
           ELSE NULL
       END,
       lease_owner = NULL, lease_until = NULL, updated_at = clock_timestamp()
 WHERE j.status = 'running'
   AND NOT EXISTS (SELECT 1 FROM public.effects e WHERE e.job_id = j.id);

UPDATE public.sessions
   SET revoked_at = COALESCE(revoked_at, clock_timestamp())
 WHERE revoked_at IS NULL;
DELETE FROM public.login_flows;

DO $verify$
BEGIN
    IF (SELECT external_sends_enabled FROM public.runtime_control WHERE id = 1) IS DISTINCT FROM FALSE
       OR EXISTS (SELECT 1 FROM public.effects WHERE status = 'sending')
       OR EXISTS (SELECT 1 FROM public.jobs WHERE status = 'running')
       OR EXISTS (
           SELECT 1
           FROM public.effects e
           JOIN public.jobs j ON j.id = e.job_id
           WHERE e.status = 'unknown'
             AND (j.status <> 'unknown'
                  OR j.result IS DISTINCT FROM jsonb_build_object('effect_id', e.id, 'status', 'unknown')
                  OR j.error_code IS DISTINCT FROM 'lease_lost'
                  OR j.lease_owner IS NOT NULL
                  OR j.lease_until IS NOT NULL)
       )
       OR EXISTS (SELECT 1 FROM public.sessions WHERE revoked_at IS NULL)
       OR EXISTS (SELECT 1 FROM public.login_flows)
    THEN
        RAISE EXCEPTION 'post-restore safety invariant failed';
    END IF;
END
$verify$;
COMMIT;
'@
    Invoke-AdminSql -Database $TargetDatabase -Sql $recoverySql | Out-Null

    $targetManifest = Get-IntegrityManifest -Database $TargetDatabase
    if ($sourceManifest) {
        $mismatches = @($sourceManifest.Keys | Where-Object { $sourceManifest[$_] -ne $targetManifest[$_] } | Sort-Object)
        if ($mismatches.Count -gt 0) {
            throw "Source/target integrity mismatch in: $($mismatches -join ', '). Target '$TargetDatabase' was retained with external sends disabled."
        }
        Write-Output 'Source/target integrity fingerprints match for grants, revisions, jobs, effects, budgets, reservations, ledger, events, outbox, projects, actors and sessions.'
    }
    else {
        Write-Output 'No live source database was supplied; target schema and safety invariants were checked, source-row comparison was skipped.'
    }

    $summary = (Invoke-AdminSql -Database $TargetDatabase -Sql "SELECT 'external_sends_enabled=' || external_sends_enabled || '|running_jobs=' || (SELECT count(*) FROM public.jobs WHERE status = 'running') || '|sending_effects=' || (SELECT count(*) FROM public.effects WHERE status = 'sending') || '|active_sessions=' || (SELECT count(*) FROM public.sessions WHERE revoked_at IS NULL) || '|login_flows=' || (SELECT count(*) FROM public.login_flows) FROM public.runtime_control WHERE id = 1;") -join ''
    if ($summary.Trim() -ne 'external_sends_enabled=false|running_jobs=0|sending_effects=0|active_sessions=0|login_flows=0') {
        throw "Post-restore safety summary did not match; target '$TargetDatabase' was retained."
    }
    Write-Output "Restored trusted custom archive to new database '$TargetDatabase'."
    Write-Output "Archive SHA-256: $actualSha256. Runtime control is disabled; no worker service was started."
    Write-Output $summary
    Write-Output 'Previous event/outbox rows and sequences were preserved; clients must fetch a full snapshot and reset their event cursor before resuming.'
}
finally {
    if ($containerId) {
        & docker exec $containerId sh -ec "rm -f $remoteArchivePath" 2>$null | Out-Null
    }
}
