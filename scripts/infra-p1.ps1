[CmdletBinding()]
param(
    [ValidateSet('Start', 'Status', 'Migrate')]
    [string]$Action = 'Start',
    [ValidateSet('Host', 'Docker')]
    [string]$MigrationRunner = 'Host',
    [ValidatePattern('^kyro_p1(?:_ops_[a-z0-9_]{1,50})?$')]
    [string]$Database = 'kyro_p1'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
$composeFile = Join-Path $repoRoot 'compose.p1.yaml'
$envFile = Join-Path $repoRoot '.env.p1.local.example'
$projectName = 'kyro-p1-ops'
$containerName = 'kyro-p1-ops-postgres-1'
$volumeName = 'kyro-p1-ops_postgres-data'
$expectedImage = 'postgres:18.6-alpine@sha256:77f585114c32fbca283dc835b0596f4e52b51b4c6662d7810b2f4084f60a1873'
$migrationImage = 'rust:1.96.1-slim-bookworm@sha256:e18a79fc84dfcfc3ab5ba72290398a644c135c97eaa881447fddc354ee4701a3'
$databaseName = $Database
$databaseUser = 'kyro_admin'
$manifestFile = Join-Path $repoRoot 'Cargo.toml'
$lockFile = Join-Path $repoRoot 'Cargo.lock'
$bindPort = $null

function Invoke-Docker {
    param([Parameter(Mandatory)][string[]]$Arguments)

    $output = & docker @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        $details = $output -join [Environment]::NewLine
        throw "docker $($Arguments -join ' ') failed (exit $LASTEXITCODE): $details"
    }
    return $output
}

function Assert-OwnedResources {
    $containerId = & docker inspect --format '{{.Id}}' $containerName 2>$null
    if ($LASTEXITCODE -eq 0) {
        $containerInfoJson = Invoke-Docker -Arguments @('inspect', $containerName)
        $containerInfo = @((($containerInfoJson -join [Environment]::NewLine) | ConvertFrom-Json))[0]
        $labels = $containerInfo.Config.Labels
        $mountedVolume = @($containerInfo.Mounts | Where-Object { $_.Destination -eq '/var/lib/postgresql' })
        $portBindings = @($containerInfo.HostConfig.PortBindings.'5432/tcp')

        if ($labels.'com.docker.compose.project' -ne $projectName -or
            $labels.'com.docker.compose.service' -ne 'postgres' -or
            $containerInfo.Config.Image -ne $expectedImage -or
            $mountedVolume.Count -ne 1 -or
            $mountedVolume[0].Type -ne 'volume' -or
            $mountedVolume[0].Name -ne $volumeName -or
            $portBindings.Count -ne 1 -or
            $portBindings[0].HostIp -ne '127.0.0.1' -or
            $portBindings[0].HostPort -ne [string]$bindPort) {
            throw "Container '$containerName' exists but does not match this managed P1 instance; it was left untouched."
        }
    }
    elseif ($LASTEXITCODE -ne 1) {
        throw 'Docker could not inspect the expected PostgreSQL container.'
    }

    $volume = & docker volume inspect --format '{{json .Labels}}' $volumeName 2>$null
    if ($LASTEXITCODE -eq 0) {
        $volumeLabels = ($volume -join [Environment]::NewLine) | ConvertFrom-Json
        if ($volumeLabels.'com.docker.compose.project' -ne $projectName -or
            $volumeLabels.'com.docker.compose.volume' -ne 'postgres-data') {
            throw "Volume '$volumeName' exists but is not owned by this Compose project; it was left untouched."
        }
    }
    elseif ($LASTEXITCODE -ne 1) {
        throw 'Docker could not inspect the expected PostgreSQL volume.'
    }
}

function Test-PortAvailable {
    param([Parameter(Mandatory)][int]$Port)

    $publishedContainers = @(& docker ps --no-trunc --filter "publish=$Port" --format '{{.ID}}')
    if ($LASTEXITCODE -ne 0) {
        throw "Docker could not check whether port $Port is already published."
    }

    $managedId = & docker inspect --format '{{.Id}}' $containerName 2>$null
    $otherPublished = @($publishedContainers | Where-Object { $_ -and $_ -ne $managedId })
    if ($otherPublished.Count -gt 0) {
        return $false
    }

    if (-not $managedId) {
        $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Parse('127.0.0.1'), $Port)
        try {
            $listener.Start()
            return $true
        }
        catch [Net.Sockets.SocketException] {
            return $false
        }
        finally {
            $listener.Stop()
        }
    }

    return $true
}

function Resolve-BindPort {
    $existingInfoJson = & docker inspect $containerName 2>$null
    if ($LASTEXITCODE -eq 0) {
        $existingInfo = @((($existingInfoJson -join [Environment]::NewLine) | ConvertFrom-Json))[0]
        $existingPortBindings = @($existingInfo.HostConfig.PortBindings.'5432/tcp')
        if ($existingPortBindings.Count -ne 1 -or $existingPortBindings[0].HostIp -ne '127.0.0.1') {
            throw "Container '$containerName' has unexpected port bindings; it was left untouched."
        }
        $existingPort = [int]$existingPortBindings[0].HostPort

        if ($env:KYRO_P1_BIND_PORT -and [int]$env:KYRO_P1_BIND_PORT -ne $existingPort) {
            throw "The owned database already uses port $existingPort. Set KYRO_P1_BIND_PORT to that value or use a separately named project; its data was left untouched."
        }
        $env:KYRO_P1_BIND_PORT = [string]$existingPort
        return $existingPort
    }
    if ($LASTEXITCODE -ne 1) {
        throw 'Docker could not inspect the expected PostgreSQL container.'
    }

    if ($env:KYRO_P1_BIND_PORT) {
        $requestedPort = 0
        if (-not [int]::TryParse($env:KYRO_P1_BIND_PORT, [ref]$requestedPort) -or $requestedPort -lt 1024 -or $requestedPort -gt 65535) {
            throw 'KYRO_P1_BIND_PORT must be an integer between 1024 and 65535.'
        }
        if (-not (Test-PortAvailable -Port $requestedPort)) {
            throw "Port 127.0.0.1:$requestedPort is already in use; existing services were left untouched."
        }
        return $requestedPort
    }

    if (Test-PortAvailable -Port 55432) {
        $env:KYRO_P1_BIND_PORT = '55432'
        return 55432
    }
    for ($candidate = 55440; $candidate -le 55539; $candidate++) {
        if (Test-PortAvailable -Port $candidate) {
            $env:KYRO_P1_BIND_PORT = [string]$candidate
            return $candidate
        }
    }

    throw 'No free port was found in 55440-55539; set KYRO_P1_BIND_PORT to a free loopback port and retry.'
}

function Wait-ForHealthyDatabase {
    for ($attempt = 0; $attempt -lt 60; $attempt++) {
        $containerId = & docker compose --project-name $projectName --file $composeFile ps --all --quiet postgres 2>$null
        if ($LASTEXITCODE -eq 0 -and $containerId) {
            $health = & docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' $containerId 2>$null
            if ($LASTEXITCODE -eq 0 -and $health -eq 'healthy') {
                return $containerId
            }
            if ($LASTEXITCODE -eq 0 -and $health -eq 'exited') {
                $logs = & docker logs --tail 80 $containerId 2>&1
                throw "PostgreSQL exited before becoming healthy:`n$($logs -join [Environment]::NewLine)"
            }
        }
        Start-Sleep -Seconds 1
    }

    $logs = & docker compose --project-name $projectName --file $composeFile logs --tail 80 postgres 2>&1
    throw "PostgreSQL did not become healthy within 60 seconds:`n$($logs -join [Environment]::NewLine)"
}

if (-not (Test-Path -LiteralPath $composeFile -PathType Leaf)) {
    throw "Compose file not found: $composeFile"
}
if (-not (Test-Path -LiteralPath $envFile -PathType Leaf)) {
    throw "Local environment example not found: $envFile"
}
if ($Action -eq 'Migrate' -and
    (-not (Test-Path -LiteralPath $manifestFile -PathType Leaf) -or
     -not (Test-Path -LiteralPath $lockFile -PathType Leaf))) {
    throw 'The locked Cargo workspace is not available; migration was not started.'
}
if ($Action -ne 'Migrate' -and $databaseName -ne 'kyro_p1') {
    throw '-Database is only supported with -Action Migrate and must use the kyro_p1_ops_ prefix.'
}

if ($Action -eq 'Status') {
    $existingContainer = & docker inspect --format '{{.Id}}' $containerName 2>$null
    if ($LASTEXITCODE -ne 0) {
        throw "No managed P1 PostgreSQL container '$containerName' exists. Run -Action Start first."
    }
}

$bindPort = Resolve-BindPort
Assert-OwnedResources

if ($Action -eq 'Start' -or $Action -eq 'Migrate') {
    if (-not (Test-PortAvailable -Port $bindPort)) {
        throw "Port 127.0.0.1:$bindPort is already in use; existing services were left untouched."
    }
    Invoke-Docker -Arguments @('compose', '--project-name', $projectName, '--env-file', $envFile, '--file', $composeFile, 'config', '--quiet') | Out-Null
    Invoke-Docker -Arguments @('compose', '--project-name', $projectName, '--env-file', $envFile, '--file', $composeFile, 'up', '--detach', 'postgres') | Out-Null
}

$containerId = Wait-ForHealthyDatabase
$databaseExists = Invoke-Docker -Arguments @(
    'exec', $containerId, 'psql', '-X', '-q', '--no-align', '--tuples-only', '-U', $databaseUser,
    '-d', 'postgres', '-c', "SELECT CASE WHEN EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datname = '$databaseName') THEN 'yes' ELSE 'no' END;"
)
if (($databaseExists -join '').Trim() -ne 'yes') {
    throw "Database '$databaseName' does not exist; this script never creates or drops migration databases."
}
$version = Invoke-Docker -Arguments @('exec', $containerId, 'psql', '-X', '-v', 'ON_ERROR_STOP=1', '-U', $databaseUser, '-d', $databaseName, '-At', '-c', 'SHOW server_version;')
$imageId = Invoke-Docker -Arguments @('inspect', '--format', '{{.Image}}', $containerId)
$mountedVolume = Invoke-Docker -Arguments @(
    'inspect', '--format', '{{range .Mounts}}{{if eq .Destination "/var/lib/postgresql"}}{{.Name}}{{end}}{{end}}', $containerId
)

if (($version -join '').Trim() -ne '18.6') {
    throw "Expected PostgreSQL 18.6; observed '$($version -join ' ')'."
}
if (($mountedVolume -join '').Trim() -ne $volumeName) {
    throw "Expected volume '$volumeName'; observed '$($mountedVolume -join ' ')'."
}

if ($Action -eq 'Migrate') {
    $adminUrl = "postgresql://$databaseUser@127.0.0.1:$bindPort/${databaseName}?sslmode=disable"
    if ($MigrationRunner -eq 'Docker') {
        $containerAdminUrl = "postgresql://$databaseUser@host.docker.internal:$bindPort/${databaseName}?sslmode=disable"
        $migrationOutput = Invoke-Docker -Arguments @(
            'run', '--rm', '--platform', 'linux/amd64', '--network', 'bridge',
            '--mount', "type=bind,source=$repoRoot,target=/workspace,readonly",
            '--workdir', '/workspace', '--env', "KYRO_DATABASE_ADMIN_URL=$containerAdminUrl",
            '--env', 'CARGO_TARGET_DIR=/tmp/kyro-p1-target', $migrationImage,
            'cargo', 'run', '--locked', '--manifest-path', '/workspace/Cargo.toml', '--package', 'kyro-store', '--bin', 'kyro-migrate'
        )
        $migrationOutput | ForEach-Object { Write-Output $_ }
    }
    else {
        $previousAdminUrl = $env:KYRO_DATABASE_ADMIN_URL
        try {
            $env:KYRO_DATABASE_ADMIN_URL = $adminUrl
            $migrationOutput = & cargo run --locked --manifest-path $manifestFile --package kyro-store --bin kyro-migrate 2>&1
            if ($LASTEXITCODE -ne 0) {
                throw "kyro-migrate failed (exit $LASTEXITCODE): $($migrationOutput -join [Environment]::NewLine)"
            }
            $migrationOutput | ForEach-Object { Write-Output $_ }
        }
        finally {
            if ($null -eq $previousAdminUrl) {
                Remove-Item Env:\KYRO_DATABASE_ADMIN_URL -ErrorAction SilentlyContinue
            }
            else {
                $env:KYRO_DATABASE_ADMIN_URL = $previousAdminUrl
            }
        }
    }

    $roleQuery = "SELECT r.rolname || '|login=' || r.rolcanlogin || '|super=' || r.rolsuper || '|createdb=' || r.rolcreatedb || '|createrole=' || r.rolcreaterole || '|inherit=' || r.rolinherit || '|bypassrls=' || r.rolbypassrls || '|owner=' || (d.datdba = r.oid) FROM pg_roles r CROSS JOIN pg_database d WHERE d.datname = '$databaseName' AND r.rolname IN ('kyro_api', 'kyro_worker') ORDER BY r.rolname;"
    $roles = Invoke-Docker -Arguments @('exec', $containerId, 'psql', '-X', '-v', 'ON_ERROR_STOP=1', '-U', $databaseUser, '-d', 'postgres', '-At', '-c', $roleQuery)
    [string[]]$roleRows = @($roles | ForEach-Object { ([string]$_).Trim() } | Where-Object { $_ })
    [string[]]$invalidRoleRows = @($roleRows | Where-Object { $_ -notmatch '\|login=true\|super=false\|createdb=false\|createrole=false\|inherit=false\|bypassrls=false\|owner=false$' })
    if ($roleRows.Count -ne 2 -or
        $invalidRoleRows.Count -ne 0) {
        throw "Expected kyro_api and kyro_worker to be login roles without administrative privileges, ownership, inheritance, or BYPASSRLS; observed: $($roleRows -join ', ')"
    }
    Write-Output "Migration roles: $($roleRows -join '; ')"
}

Write-Output "PostgreSQL $($version -join '') is healthy on 127.0.0.1:$bindPort."
Write-Output "Image: $expectedImage (runtime image ID: $($imageId -join ''))."
Write-Output "Data volume: $volumeName."
Write-Output "KYRO_P1_BIND_PORT=$bindPort"
Write-Output "KYRO_DATABASE_ADMIN_URL=postgresql://kyro_admin@127.0.0.1:$bindPort/${databaseName}?sslmode=disable"
Write-Output "KYRO_DATABASE_URL=postgresql://kyro_api@127.0.0.1:$bindPort/${databaseName}?sslmode=disable"
Write-Output "KYRO_WORKER_DATABASE_URL=postgresql://kyro_worker@127.0.0.1:$bindPort/${databaseName}?sslmode=disable"
