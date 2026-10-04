param([ValidateSet('Prepare','RefreshPins','Start','Stop','Status')][string]$Action = 'Status')
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$runtimeAction = $Action
. (Join-Path $PSScriptRoot 'nebius-vault.ps1') -Action Library
$Action = $runtimeAction
$repo = Split-Path -Parent $PSScriptRoot
$state = Join-Path ([Environment]::GetFolderPath('UserProfile')) '.kyro\nebius-p1'
$env:KYRO_NEBIUS_STATE = $state.Replace('\','/')
$compose = Join-Path $repo 'compose.p1.nebius.yaml'
$utf8 = [Text.UTF8Encoding]::new($false)

function Write-State([string]$Name, [string]$Value) { [IO.File]::WriteAllText((Join-Path $state $Name), $Value, $utf8) }
function Write-Json([string]$Name, $Value) { Write-State $Name (($Value | ConvertTo-Json -Depth 30) + "`n") }
function Read-Json([string]$Name) { Get-Content -LiteralPath (Join-Path $state $Name) -Raw | ConvertFrom-Json }
function Invoke-Compose([string[]]$Arguments) {
    & docker compose --project-name kyro-nebius-p1 --file $compose @Arguments
    if ($LASTEXITCODE -ne 0) { throw 'compose_failed' }
}
function New-Password { [Convert]::ToHexString([Security.Cryptography.RandomNumberGenerator]::GetBytes(32)).ToLowerInvariant() }
function Protect-State {
    [IO.Directory]::CreateDirectory($state) | Out-Null
    if ((Get-Item -LiteralPath $state).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'state_reparse_point' }
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
    # Docker Desktop's file broker runs as SYSTEM. The API key is never in this directory.
    & icacls $state /inheritance:r /grant:r ('*'+$sid.Value+':(OI)(CI)F') '*S-1-5-18:(OI)(CI)F' >$null
    if ($LASTEXITCODE -ne 0) { throw 'state_acl_failed' }
}
function Get-PublicPins {
    $addresses = @([Net.Dns]::GetHostAddresses('api.tokenfactory.nebius.com') | Where-Object AddressFamily -eq InterNetwork | ForEach-Object IPAddressToString | Sort-Object -Unique)
    if ($addresses.Count -eq 0 -or $addresses.Count -gt 16) { throw 'dns_pin_count' }
    foreach ($address in $addresses) {
        $bytes = [Net.IPAddress]::Parse($address).GetAddressBytes()
        if ($bytes[0] -in @(0,10,127) -or $bytes[0] -ge 224 -or ($bytes[0] -eq 169 -and $bytes[1] -eq 254) -or
            ($bytes[0] -eq 172 -and $bytes[1] -ge 16 -and $bytes[1] -le 31) -or ($bytes[0] -eq 192 -and $bytes[1] -eq 168) -or
            ($bytes[0] -eq 100 -and $bytes[1] -ge 64 -and $bytes[1] -le 127) -or ($bytes[0] -eq 198 -and $bytes[1] -in @(18,19,51)) -or
            ($bytes[0] -eq 192 -and $bytes[1] -in @(0,2)) -or ($bytes[0] -eq 203 -and $bytes[1] -eq 0)) { throw 'dns_non_public_pin' }
    }
    return ,$addresses
}
function Write-Pins($Addresses) {
    $ips = $Addresses -join ', '
    Write-State 'egress.nft' @"
table inet kyro_egress {
  set nebius_v4 { type ipv4_addr; elements = { $ips }; }
  chain input { type filter hook input priority 0; policy drop; ct state established,related accept; }
  chain forward { type filter hook forward priority 0; policy drop; }
  chain output {
    type filter hook output priority 0; policy drop;
    udp dport 53 counter reject
    tcp dport 53 counter reject
    meta nfproto ipv6 counter reject
    ct state established,related accept
    ip daddr 10.248.73.2 tcp dport 5432 counter accept
    ip daddr @nebius_v4 tcp dport 443 counter accept
    counter reject
  }
}
"@
    $registry = Read-Json 'models.json'
    $registry.destinations[0].pinned_addresses = @($Addresses | ForEach-Object { "${_}:443" })
    Write-Json 'models.json' $registry
    Write-Json 'pins.json' @{ resolved_at=[DateTimeOffset]::Now.ToString('o'); host='api.tokenfactory.nebius.com'; ipv4=$Addresses; ipv6='denied' }
}
function Import-Catalog {
    $plain = Read-NebiusVault
    $handler = [Net.Http.HttpClientHandler]::new(); $handler.UseProxy=$false; $handler.AllowAutoRedirect=$false
    $client = [Net.Http.HttpClient]::new($handler); $client.Timeout=[TimeSpan]::FromSeconds(20)
    try {
        $client.DefaultRequestHeaders.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer',[Text.Encoding]::UTF8.GetString($plain))
        $response = $client.GetAsync('https://api.tokenfactory.nebius.com/v1/models?verbose=true').GetAwaiter().GetResult()
        if (-not $response.IsSuccessStatusCode) { throw ('catalog_http_' + [int]$response.StatusCode) }
        $raw = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
        if ($raw.Length -gt 1048576 -or $raw.Contains([Text.Encoding]::UTF8.GetString($plain))) { throw 'catalog_invalid' }
        $catalog = $raw | ConvertFrom-Json
        $models = @($catalog.data | Where-Object { $_.id -cmatch '^nvidia/.*[Nn]emotron' } | ForEach-Object {
            [pscustomobject]@{ id=$_.id; context_length=$_.context_length; quantization=$_.quantization; regions=$_.regions;
                supported_features=$_.supported_features; supported_sampling_parameters=$_.supported_sampling_parameters; pricing=$_.pricing;
                profile_usd=2048*[decimal]$_.pricing.prompt+512*[decimal]$_.pricing.completion; weights_version=$null; json_schema='unconfirmed' }
        })
        if ($models.Count -eq 0) { throw 'catalog_no_nvidia_model' }
        Write-Json 'catalog.json' @{ checked_at=[DateTimeOffset]::Now.ToString('o'); endpoint='https://api.tokenfactory.nebius.com/v1/models?verbose=true'; models=$models }
        # Ordinal ordering is deterministic, including uppercase provider identifiers.
        $minimum = ($models | Measure-Object profile_usd -Minimum).Minimum
        $ties = @($models | Where-Object profile_usd -eq $minimum)
        $ids = [string[]]@($ties | ForEach-Object id); [Array]::Sort($ids,[StringComparer]::Ordinal)
        return ($ties | Where-Object id -CEQ $ids[0])
    } finally { [Array]::Clear($plain,0,$plain.Length); $client.Dispose(); $handler.Dispose() }
}
function Check-Qualification {
    $qualification = Read-Json 'qualification.json'
    $registry = Read-Json 'models.json'
    $destination = $registry.destinations[0]
    $vaultHash = (Get-FileHash -LiteralPath (Get-NebiusVaultPath) -Algorithm SHA256).Hash.ToLowerInvariant()
    $age = [DateTimeOffset]::Now - [DateTimeOffset]::Parse($qualification.checked_at)
    if ($qualification.endpoint -cne $destination.base_url -or $qualification.model -cne $destination.models[0].id -or
        $qualification.vault_sha256 -cne $vaultHash -or $age.TotalDays -lt 0 -or $age.TotalDays -gt 30 -or
        $qualification.zero_retention_confirmed -ne $true -or $qualification.json_schema_supported -ne $true -or
        $qualification.max_completion_tokens_includes_reasoning -ne $true) { throw 'qualification_unconfirmed' }
    $source = [Uri]$qualification.source
    if ($source.Scheme -ne 'https' -or $source.Host -notin @('nebius.com','docs.nebius.com','docs.tokenfactory.nebius.com')) { throw 'qualification_source_invalid' }
    $destination.qualified = $true; $destination.retention_seconds=0
    $destination.nebius.json_schema=$true; $destination.nebius.bounded_completion=$true
    $destination.nebius.retention_evidence=$qualification.source
    Write-Json 'models.json' $registry
}

if ($Action -eq 'Status') {
    [pscustomobject]@{ state_present=(Test-Path -LiteralPath $state); vault_present=(Test-Path -LiteralPath (Get-NebiusVaultPath)); qualification_present=(Test-Path -LiteralPath (Join-Path $state 'qualification.json')) }
    return
}
if ($Action -eq 'Stop') {
    Invoke-Compose @('stop','worker'); Invoke-Compose @('rm','-f','worker')
    Invoke-Compose @('stop','oidc','api','egress','postgres')
    return
}
if ($Action -eq 'Prepare') {
    Protect-State
    if (Test-Path -LiteralPath (Join-Path $state 'campaign.json')) { throw 'state_already_prepared_preserve_budget' }
    $model = Import-Catalog
    if ($model.context_length -gt 262144 -or [decimal]$model.pricing.request -ne 0) { throw 'model_cost_not_bounded' }
    $schema = @{ type='object'; properties=@{ summary=@{type='string';maxLength=300};items=@{type='array';items=@{type='string';maxLength=100};maxItems=3} };required=@('summary','items');additionalProperties=$false }
    $pricing = @{ version=('nebius-'+(Get-Date -Format 'yyyy-MM-dd'));effective_date=(Get-Date -Format 'yyyy-MM-dd'); currency='USD';unit='usd';unit_scale=1000000000;
        input_units_per_million_tokens=[long]([decimal]$model.pricing.prompt*1000000000000000);output_units_per_million_tokens=[long]([decimal]$model.pricing.completion*1000000000000000) }
    Write-Json 'models.json' @{format_version=1;destinations=@(@{ id='nebius-nvidia';provider='nebius';kind='cloud';base_url='https://api.tokenfactory.nebius.com/v1/';allowed_host='api.tokenfactory.nebius.com';pinned_addresses=@();secret_ref='file:KYRO_MODEL_API_KEY_FILE';qualified=$false;retention_seconds=$null;
        nebius=@{json_schema=$false;bounded_completion=$true;retention_evidence=$null;wire_overhead_tokens=1024;context_tokens=[int]$model.context_length};
        models=@(@{id=$model.id;version=$null;output_schema=@{id='nebius-p1-output';version='1';schema=$schema};pricing=$pricing;max_input_bytes=4096;max_input_tokens=[int]$model.context_length;max_output_tokens=512;max_deadline_ms=30000;max_response_bytes=16000}) })}
    Write-Pins (Get-PublicPins)
    $xml = [xml](Invoke-WebRequest -Uri 'https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml').Content
    $rate = [decimal]($xml.Envelope.Cube.Cube.Cube | Where-Object currency -eq USD).rate
    if ($rate -le 0 -or ((Get-Date)-[datetime]$xml.Envelope.Cube.Cube.time).TotalDays -gt 7) { throw 'fx_stale_or_invalid' }
    # Forty percent stays inside the EUR ceiling, covering taxes and FX/price uncertainty.
    $ceiling = [long][Math]::Floor($rate*0.60*1000000000)
    $campaign = @{ version=1; ceiling_eur=1; currency='USD';unit_scale=1000000000;limit_units=$ceiling;eur_usd=$rate;fx_date=$xml.Envelope.Cube.Cube.time;fx_source='https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml';margin_fraction=0.40;max_calls=3;attempts=0;stopped_on_uncertainty=$false;project_id=$null;pending=$false }
    $admin=New-Password; $api=New-Password; $worker=New-Password
    Write-State 'postgres_password' $admin
    Write-State 'init.sql' "CREATE ROLE kyro_api LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD '$api';`nCREATE ROLE kyro_worker LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD '$worker';`n"
    foreach ($role in @(@('admin',$admin),@('api',$api),@('worker',$worker))) {
        # The worker shares a DNS-denied namespace and connects to the fixed database IP.
        $databaseHost = if ($role[0] -eq 'worker') { '10.248.73.2' } else { 'postgres' }
        Write-State ($role[0]+'_database_url') ('postgresql://kyro_'+$role[0]+':'+$role[1]+'@'+$databaseHost+':5432/kyro_nebius?sslmode=verify-full&sslrootcert=/run/secrets/postgres_ca')
    }
    [IO.Directory]::CreateDirectory((Join-Path $state 'tls')) | Out-Null
    & docker run --rm --network none --entrypoint sh --mount "type=bind,source=$state,target=/state" kyro-nebius-guard:local -ec 'umask 077; openssl req -x509 -newkey rsa:3072 -nodes -days 30 -subj /CN=Kyro-Nebius-P1-CA -keyout /state/tls/ca.key -out /state/tls/ca.crt 2>/dev/null; openssl req -newkey rsa:3072 -nodes -subj /CN=postgres -keyout /state/tls/server.key -out /state/tls/server.csr 2>/dev/null; printf "subjectAltName=DNS:postgres,IP:10.248.73.2\nextendedKeyUsage=serverAuth\n" > /state/tls/server.ext; openssl x509 -req -in /state/tls/server.csr -CA /state/tls/ca.crt -CAkey /state/tls/ca.key -CAcreateserial -days 30 -extfile /state/tls/server.ext -out /state/tls/server.crt 2>/dev/null; chmod 644 /state/tls/ca.crt /state/tls/server.crt'
    if ($LASTEXITCODE -ne 0) { throw 'tls_generation_failed' }
    Write-State 'api.env' @"
KYRO_ENV=development
KYRO_BIND=0.0.0.0:58090
KYRO_MAX_CONNECTIONS=4
KYRO_MAX_BODY_BYTES=49152
KYRO_SYNTHETIC_PROVIDERS=true
KYRO_OIDC_ISSUER=http://127.0.0.1:59090/issuer
KYRO_OIDC_AUTHORIZATION_ENDPOINT=http://127.0.0.1:59090/oidc/authorize
KYRO_OIDC_TOKEN_ENDPOINT=http://127.0.0.1:59090/oidc/token
KYRO_OIDC_JWKS_URI=http://127.0.0.1:59090/oidc/jwks
KYRO_OIDC_REDIRECT_URI=http://127.0.0.1:58090/v1/auth/callback
KYRO_OIDC_CLIENT_ID=kyro-e2e-client
KYRO_OIDC_SYNTHETIC_PROVIDER=true
KYRO_AUTH_UI_ORIGIN=http://127.0.0.1:58090
KYRO_MODEL_REGISTRY_PATH=/app/config/models.nebius.json
KYRO_MODEL_ALLOW_SYNTHETIC_LOOPBACK=0
RUST_LOG=kyro_api=info
"@
    Write-Json 'campaign.json' $campaign
    Write-Output 'Configuration isolée préparée ; génération désactivée tant que la qualification est absente.'
    return
}
if ($Action -eq 'RefreshPins') {
    Invoke-Compose @('stop','worker','egress')
    Invoke-Compose @('rm','-f','worker','egress')
    Write-Pins (Get-PublicPins)
    Write-Output 'Adresses renouvelées explicitement ; relancer Start après vérification.'
    return
}
if ($Action -eq 'Start') {
    $campaign=Read-Json 'campaign.json'
    if ($campaign.pending -or $campaign.stopped_on_uncertainty -or $campaign.attempts -ge $campaign.max_calls) { throw 'campaign_closed_or_uncertain' }
    Check-Qualification
    try {
    Invoke-Compose @('up','-d','--wait','postgres','egress')
    Invoke-Compose @('--profile','migration','run','--rm','migrate')
    Invoke-Compose @('up','-d','--force-recreate','api','oidc')
    Invoke-Compose @('up','-d','worker')
    $container = (& docker compose --project-name kyro-nebius-p1 --file $compose ps -q worker).Trim()
    $ready=$false
    for ($index=0; $index -lt 100; $index++) {
        & docker exec $container test -p /run/kyro-secrets/key.pipe 2>$null
        if ($LASTEXITCODE -eq 0) { $ready=$true; break }
        Start-Sleep -Milliseconds 100
    }
    if (-not $ready) { Invoke-Compose @('stop','worker'); throw 'worker_secret_pipe_absent' }
    $plain=Read-NebiusVault
    $process=[Diagnostics.Process]::new(); $process.StartInfo=[Diagnostics.ProcessStartInfo]::new('docker')
    foreach ($argument in @('exec','-i',$container,'sh','-c','cat > /run/kyro-secrets/key.pipe')) { $process.StartInfo.ArgumentList.Add($argument) }
    $process.StartInfo.UseShellExecute=$false; $process.StartInfo.RedirectStandardInput=$true; $process.StartInfo.RedirectStandardOutput=$true; $process.StartInfo.RedirectStandardError=$true; $process.StartInfo.CreateNoWindow=$true
    try {
        $process.Start() | Out-Null
        $process.StandardInput.BaseStream.Write($plain,0,$plain.Length); $process.StandardInput.Close()
        if (-not $process.WaitForExit(10000) -or $process.ExitCode -ne 0) { throw 'worker_secret_delivery_failed' }
    } finally { [Array]::Clear($plain,0,$plain.Length); $process.Dispose() }
    Write-Output 'Worker isolé lancé ; qualification via verify-nebius-p1.mjs, puis Stop obligatoire.'
    } catch {
        Invoke-Compose @('stop','worker')
        Invoke-Compose @('rm','-f','worker')
        throw
    }
}
