[CmdletBinding()]
param(
    [ValidateSet('Local', 'Production')]
    [string]$TargetEnvironment = 'Local',
    [Parameter(Mandatory)][string]$OutputFile,
    [ValidatePattern('^[a-z][a-z0-9_]{0,62}$')][string]$Database,
    [string]$ProductionEnvFile
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot)).TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
$expectedImage = 'postgres:18.6-alpine@sha256:77f585114c32fbca283dc835b0596f4e52b51b4c6662d7810b2f4084f60a1873'
$volumeName = if ($TargetEnvironment -eq 'Local') { 'kyro-p1-ops_postgres-data' } else { 'kyro-p1-production_postgres-data' }
$projectName = if ($TargetEnvironment -eq 'Local') { 'kyro-p1-ops' } else { 'kyro-p1-production' }
$composeFile = Join-Path $repoRoot $(if ($TargetEnvironment -eq 'Local') { 'compose.p1.yaml' } else { 'compose.p1.production.yaml' })
$envFile = Join-Path $repoRoot $(if ($TargetEnvironment -eq 'Local') { '.env.p1.local.example' } else { '.env.p1.production.example' })
$databaseName = $null
$databaseUser = $null
$containerId = $null
$remoteDumpPath = "/tmp/kyro-p1-backup-$([Guid]::NewGuid().ToString('N')).dump"
$outputFullPath = [IO.Path]::GetFullPath($OutputFile)
$completed = $false

function Invoke-Docker {
    param([Parameter(Mandatory)][string[]]$Arguments)

    $output = & docker @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "docker $($Arguments -join ' ') failed (exit $LASTEXITCODE): $($output -join [Environment]::NewLine)"
    }
    return $output
}

if ($TargetEnvironment -eq 'Production') {
    if (-not $ProductionEnvFile -or -not (Test-Path -LiteralPath $ProductionEnvFile -PathType Leaf)) {
        throw 'Production backups require -ProductionEnvFile pointing to the deployment env file (paths only; secrets remain mounted files).'
    }
    $envFile = (Resolve-Path -LiteralPath $ProductionEnvFile).Path
}
elseif ($ProductionEnvFile) {
    throw '-ProductionEnvFile is only valid with -TargetEnvironment Production.'
}

if (-not (Test-Path -LiteralPath $composeFile -PathType Leaf) -or -not (Test-Path -LiteralPath $envFile -PathType Leaf)) {
    throw 'The selected Compose or environment example file is missing.'
}
$pathComparison = if ([IO.Path]::DirectorySeparatorChar -eq '\') { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
if ($outputFullPath.StartsWith($repoRoot + [IO.Path]::DirectorySeparatorChar, $pathComparison) -or
    [string]::Equals($outputFullPath, $repoRoot, $pathComparison)) {
    throw 'Backups must be written outside the repository.'
}
if (Test-Path -LiteralPath $outputFullPath) {
    throw "Backup destination already exists; refusing to overwrite: $outputFullPath"
}

$composeArgs = @('compose', '--project-name', $projectName, '--env-file', $envFile, '--file', $composeFile)
$containerIds = Invoke-Docker -Arguments ($composeArgs + @('ps', '--all', '--quiet', 'postgres'))
if (-not $containerIds) {
    throw "No existing PostgreSQL service was found for '$TargetEnvironment'; backup never starts a service."
}
$containerId = ([string]($containerIds | Select-Object -First 1)).Trim()

$state = Invoke-Docker -Arguments @('inspect', '--format', '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{end}}', $containerId)
if (($state -join '').Trim() -ne 'running|healthy') {
    throw "The selected PostgreSQL service is not healthy ($($state -join ' ')); backup never starts a service."
}
$containerJson = Invoke-Docker -Arguments @('inspect', $containerId)
$container = @((($containerJson -join [Environment]::NewLine) | ConvertFrom-Json))[0]
$labels = $container.Config.Labels
$dataMount = @($container.Mounts | Where-Object { $_.Destination -eq '/var/lib/postgresql' })
if ($labels.'com.docker.compose.project' -ne $projectName -or
    $labels.'com.docker.compose.service' -ne 'postgres' -or
    $container.Config.Image -ne $expectedImage -or
    $dataMount.Count -ne 1 -or
    $dataMount[0].Type -ne 'volume' -or
    $dataMount[0].Name -ne $volumeName) {
    throw "PostgreSQL container '$containerId' does not match the selected managed project; it was left untouched."
}

$containerEnvironment = @{}
foreach ($entry in $container.Config.Env) {
    $separator = $entry.IndexOf('=')
    if ($separator -gt 0) {
        $containerEnvironment[$entry.Substring(0, $separator)] = $entry.Substring($separator + 1)
    }
}
$databaseName = $containerEnvironment['POSTGRES_DB']
$databaseUser = $containerEnvironment['POSTGRES_USER']
if (-not $databaseName -or -not $databaseUser) {
    throw 'The managed PostgreSQL service does not declare its initialized database and admin user.'
}
$initializedDatabase = $databaseName
if ($Database) {
    $databaseName = $Database
}
if ($TargetEnvironment -eq 'Local') {
    if ($initializedDatabase -ne 'kyro_p1' -or $databaseUser -ne 'kyro_admin') {
        throw 'The local P1 service database/user do not match the expected synthetic instance.'
    }
    $bindings = @($container.HostConfig.PortBindings.'5432/tcp')
    if ($bindings.Count -ne 1 -or $bindings[0].HostIp -ne '127.0.0.1' -or [int]$bindings[0].HostPort -lt 1024) {
        throw 'The local PostgreSQL service is not bound to one loopback port.'
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
        throw 'The production service does not match the expected read-only secret, SCRAM, TLS, and private-network configuration.'
    }
}

$databaseName = ([string]$databaseName).Trim()
$databaseUser = ([string]$databaseUser).Trim()
if ($databaseName -notmatch '^[a-zA-Z_][a-zA-Z0-9_]{0,62}$' -or $databaseUser -notmatch '^[a-zA-Z_][a-zA-Z0-9_]{0,62}$') {
    throw 'The selected database or admin username contains unsupported identifier characters.'
}

$databaseExistsSql = "SELECT CASE WHEN EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datname = '$databaseName') THEN 'yes' ELSE 'no' END;"
$databaseExistsBody = "exec psql -X -q --no-align --tuples-only --username=`"$databaseUser`" --dbname=postgres --command=`"$databaseExistsSql`""
if ($TargetEnvironment -eq 'Production') {
    $databaseExistsBody = "password=`$(cat /run/secrets/postgres_admin_password); export PGPASSWORD=`"`$password`"; unset password; $databaseExistsBody"
}
$databaseExists = (Invoke-Docker -Arguments @('exec', $containerId, 'sh', '-ec', $databaseExistsBody) | ForEach-Object { ([string]$_).Trim() }) -join ''
if ($databaseExists -ne 'yes') {
    throw "Database '$databaseName' does not exist on the selected managed PostgreSQL service."
}

$outputDirectory = Split-Path -Parent $outputFullPath
[void][IO.Directory]::CreateDirectory($outputDirectory)

try {
    $dumpBody = "exec pg_dump --format=custom --dbname=$databaseName --username=$databaseUser --file=$remoteDumpPath"
    if ($TargetEnvironment -eq 'Production') {
        $dumpBody = "password=`$(cat /run/secrets/postgres_admin_password); export PGPASSWORD=`"`$password`"; unset password; $dumpBody"
    }
    Invoke-Docker -Arguments @('exec', $containerId, 'sh', '-ec', $dumpBody) | Out-Null
    Invoke-Docker -Arguments @('cp', "$($containerId):$remoteDumpPath", $outputFullPath) | Out-Null
    $hash = Get-FileHash -LiteralPath $outputFullPath -Algorithm SHA256
    $length = (Get-Item -LiteralPath $outputFullPath).Length
    if ($length -eq 0) {
        throw 'pg_dump produced an empty file.'
    }
    $completed = $true
    Write-Output "Backup created: $outputFullPath"
    Write-Output "Format: PostgreSQL custom archive; bytes: $length; SHA-256: $($hash.Hash.ToLowerInvariant())"
}
finally {
    if ($containerId) {
        & docker exec $containerId sh -ec "rm -f $remoteDumpPath" 2>$null | Out-Null
    }
    if (-not $completed -and (Test-Path -LiteralPath $outputFullPath)) {
        Remove-Item -LiteralPath $outputFullPath -Force
    }
}
