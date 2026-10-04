# Reproducible runtime test with a fake key only; never reads the DPAPI vault.
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
$repo=Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$state=Join-Path ([Environment]::GetFolderPath('UserProfile')) '.kyro/nebius-p1'
$env:KYRO_NEBIUS_STATE=$state.Replace('\','/')
$compose=Join-Path $repo 'compose.p1.nebius.yaml'
$registry=Get-Content -LiteralPath (Join-Path $state 'models.json') -Raw | ConvertFrom-Json
if ($registry.destinations[0].qualified) { throw 'canary_requires_disabled_registry' }
$proof=[ordered]@{at=[DateTimeOffset]::Now.ToString('o');kind='worker_stdin_tmpfs_canary';real_api_key_used=$false;passed=$false}
$container=$null
try {
    & docker compose --file $compose up -d worker
    if ($LASTEXITCODE -ne 0) { throw 'canary_start_failed' }
    $container=(& docker compose --file $compose ps -q worker).Trim()
    $ready=$false
    for ($attempt=0;$attempt -lt 100;$attempt++) {
        & docker exec $container test -p /run/kyro-secrets/key.pipe 2>$null
        if ($LASTEXITCODE -eq 0) { $ready=$true;break }
        Start-Sleep -Milliseconds 100
    }
    if (-not $ready) { throw 'canary_pipe_missing' }
    $canary='fake-worker-canary-nebius-qualification-only'
    $bytes=[Text.Encoding]::UTF8.GetBytes($canary)
    $process=[Diagnostics.Process]::new();$process.StartInfo=[Diagnostics.ProcessStartInfo]::new('docker')
    foreach ($argument in @('exec','-i',$container,'sh','-c','cat > /run/kyro-secrets/key.pipe')) { $process.StartInfo.ArgumentList.Add($argument) }
    $process.StartInfo.UseShellExecute=$false;$process.StartInfo.RedirectStandardInput=$true;$process.StartInfo.RedirectStandardError=$true;$process.StartInfo.RedirectStandardOutput=$true;$process.StartInfo.CreateNoWindow=$true
    try {
        $process.Start() | Out-Null
        $process.StandardInput.BaseStream.Write($bytes,0,$bytes.Length);$process.StandardInput.Close()
        if (-not $process.WaitForExit(10000) -or $process.ExitCode -ne 0) { throw 'canary_delivery_failed' }
    } finally { [Array]::Clear($bytes,0,$bytes.Length);$process.Dispose() }
    Start-Sleep -Seconds 2
    $info=(& docker inspect $container | ConvertFrom-Json)[0]
    $logs=(& docker logs $container 2>&1) -join "`n"
    $stat=(& docker exec $container stat -c '%a %u %h' /run/kyro-secrets/nebius_api_key) -join ''
    $caps=(& docker exec $container sh -c 'grep -E "CapEff|CapBnd|NoNewPrivs" /proc/1/status') -join "`n"
    # The admin password remains a mounted file, never an argument or printed value.
    $connections=(& docker compose --file $compose exec -T postgres sh -ec 'export PGPASSWORD="$(cat /run/secrets/postgres_password)"; exec psql -X -qAt -U kyro_admin -d kyro_nebius -c "SELECT count(*) FROM pg_stat_activity a JOIN pg_stat_ssl s USING(pid) WHERE a.usename=''kyro_worker'' AND a.client_addr=''10.248.73.3'' AND s.ssl"') -join ''
    if ($LASTEXITCODE -ne 0) { throw 'canary_database_probe_failed' }
    $proof.running=$info.State.Running;$proof.uid=$info.Config.User;$proof.read_only=$info.HostConfig.ReadonlyRootfs
    $proof.cap_drop=$info.HostConfig.CapDrop;$proof.cap_add=$info.HostConfig.CapAdd;$proof.security_opt=$info.HostConfig.SecurityOpt
    $proof.cpu_nano=$info.HostConfig.NanoCpus;$proof.memory_bytes=$info.HostConfig.Memory;$proof.pids_limit=$info.HostConfig.PidsLimit
    $proof.key_stat=$stat;$proof.process_security=$caps;$proof.tls_worker_connections=[int]$connections
    $proof.tmpfs_present=$info.HostConfig.Tmpfs.PSObject.Properties.Name -contains '/run/kyro-secrets'
    $proof.docker_socket_absent=@($info.Mounts | Where-Object Destination -eq '/var/run/docker.sock').Count -eq 0
    $proof.canary_absent_from_logs=-not $logs.Contains($canary)
    $proof.canary_absent_from_docker_environment=-not (($info.Config.Env -join "`n").Contains($canary))
    $proof.passed=$proof.running -and $proof.read_only -and $stat -eq '400 10001 1' -and [int]$connections -gt 0 -and
        $proof.tmpfs_present -and $proof.docker_socket_absent -and $proof.canary_absent_from_logs -and $proof.canary_absent_from_docker_environment -and
        ($caps -match 'CapEff:\s+0+') -and ($caps -match 'CapBnd:\s+0+') -and ($caps -match 'NoNewPrivs:\s+1')
} finally {
    & docker compose --file $compose stop worker
    & docker compose --file $compose rm -f worker
    $proof.worker_removed=@(& docker compose --file $compose ps --all -q worker).Count -eq 0
    $stamp=([DateTimeOffset]::Now.ToString('yyyyMMddTHHmmss'))
    $proof | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $repo "docs/suivi/preuves/nebius-worker-$stamp.json") -Encoding utf8
    $proof | ConvertTo-Json -Depth 8
}
if (-not $proof.passed -or -not $proof.worker_removed) { throw 'canary_verification_failed' }
