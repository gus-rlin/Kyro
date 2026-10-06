param(
    [ValidateSet('Library','Prepare','Native','RefreshSchema','StrictMode','Seal','ObjectMode','ExpandCallSlots','Qualify','Status','Reserve','Deliver','Run')][string]$Action='Status',
    [string]$Phase='',
    [ValidateRange(1,100000000)][long]$PhaseLimitUnits=100000000,
    [ValidateRange(4,7)][int]$PhaseCalls=7,
    [string]$Container='',
    [ValidateSet('SevenRoles','Factory')][string]$Scenario='SevenRoles'
)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
$campaignAction=$Action
. (Join-Path $PSScriptRoot '../nebius-vault.ps1') -Action Library
$Action=$campaignAction
$state=Join-Path ([Environment]::GetFolderPath('UserProfile')) '.kyro/nebius-p3-20261006'
$utf8=[Text.UTF8Encoding]::new($false)
function Save-Json([string]$Name,$Value) {
    $target=Join-Path $state $Name
    $temporary=$target+'.'+[Guid]::NewGuid().ToString('N')+'.tmp'
    [IO.File]::WriteAllText($temporary,($Value | ConvertTo-Json -Depth 100)+"`n",$utf8)
    Move-Item -LiteralPath $temporary -Destination $target -Force
}
function Read-Json([string]$Name) { Get-Content -Raw -LiteralPath (Join-Path $state $Name) | ConvertFrom-Json }
function Get-QualificationReceipt([int]$HttpStatus,[string]$ExpectedModel,$Answer,[bool]$ContractVerified,[string]$ForbiddenValue) {
    if ([string]::IsNullOrEmpty($ForbiddenValue)) { throw 'qualification_secret_guard_required' }
    if ($HttpStatus -lt 100 -or $HttpStatus -gt 599 -or $ExpectedModel -cnotmatch '^nvidia/[A-Za-z0-9._-]{1,128}$') { throw 'qualification_receipt_invalid' }
    if ($ExpectedModel.Contains($ForbiddenValue)) { throw 'qualification_response_invalid' }
    $receipt=[ordered]@{http_status=$HttpStatus}
    # Bodies of failures and accessory provider fields are never evidence.
    if ($HttpStatus -lt 200 -or $HttpStatus -ge 300) { return $receipt }
    if ($Answer -isnot [pscustomobject] -or -not $Answer.PSObject.Properties['usage']) { throw 'qualification_usage_invalid' }
    $usage=$Answer.usage
    if ($usage -isnot [pscustomobject] -or -not $usage.PSObject.Properties['prompt_tokens'] -or -not $usage.PSObject.Properties['completion_tokens']) { throw 'qualification_usage_invalid' }
    foreach ($name in @('prompt_tokens','completion_tokens')) {
        $value=$usage.$name
        if ($value -isnot [int] -and $value -isnot [long]) { throw 'qualification_usage_invalid' }
        if ($value -lt 0 -or ($name -ceq 'prompt_tokens' -and $value -gt 262144) -or ($name -ceq 'completion_tokens' -and $value -gt 8192)) { throw 'qualification_usage_invalid' }
    }
    $receipt.id=$null
    if ($Answer.PSObject.Properties['id'] -and $Answer.id -is [string] -and $Answer.id -cmatch '^[A-Za-z0-9._:-]{1,192}$') { $receipt.id=$Answer.id }
    $receipt.model_matches_expected=[bool]($Answer.PSObject.Properties['model'] -and $Answer.model -is [string] -and $Answer.model -ceq $ExpectedModel)
    $receipt.model=if ($receipt.model_matches_expected) { $ExpectedModel } else { $null }
    $receipt.usage=[ordered]@{prompt_tokens=[long]$usage.prompt_tokens;completion_tokens=[long]$usage.completion_tokens;cached_prompt_tokens=$null}
    if ($usage.PSObject.Properties['prompt_tokens_details'] -and $usage.prompt_tokens_details -is [pscustomobject] -and $usage.prompt_tokens_details.PSObject.Properties['cached_tokens']) {
        $cached=$usage.prompt_tokens_details.cached_tokens
        if (($cached -is [int] -or $cached -is [long]) -and $cached -ge 0 -and $cached -le $usage.prompt_tokens) { $receipt.usage.cached_prompt_tokens=[long]$cached }
    }
    $receipt.finish_reason_is_stop=$false
    if ($Answer.PSObject.Properties['choices'] -and @($Answer.choices).Count -gt 0 -and $Answer.choices[0] -is [pscustomobject] -and $Answer.choices[0].PSObject.Properties['finish_reason']) {
        $receipt.finish_reason_is_stop=[bool]($Answer.choices[0].finish_reason -is [string] -and $Answer.choices[0].finish_reason -ceq 'stop')
    }
    $receipt.contract_verified=$ContractVerified
    if ($ContractVerified) {
        if (-not $receipt.model_matches_expected -or -not $receipt.finish_reason_is_stop) { throw 'qualification_contract_invalid' }
        # This fixed Review probe is written only after exact contract validation.
        $receipt.contract=[ordered]@{candidate_digest=('0'*64);approved=$false;findings=@()}
    }
    # Check the decoded projection as well as the raw transport: JSON Unicode
    # escapes must not let an echoed credential become a published opaque id.
    if (($receipt | ConvertTo-Json -Depth 8 -Compress).Contains($ForbiddenValue)) { throw 'qualification_response_invalid' }
    return $receipt
}
function Get-QualificationEstimate($Model,$Receipt) {
    # Usage cannot release a reserve until it is bound to the requested price.
    if (-not $Receipt.model_matches_expected -or $Receipt.model -cne $Model.id) { throw 'qualification_model_invalid' }
    if ($Receipt.usage.prompt_tokens -gt $Model.max_input_tokens) { throw 'qualification_usage_invalid' }
    return [long][Math]::Ceiling(([decimal]$Receipt.usage.prompt_tokens*$Model.pricing.input_units_per_million_tokens+[decimal]$Receipt.usage.completion_tokens*$Model.pricing.output_units_per_million_tokens)/1000000)
}
function Available-Units($Budget) {
    $used=[long]0
    foreach ($entry in $Budget.entries) {
        if ($entry.reserved_units -le 0 -or ($entry.estimated_units -ne $null -and ($entry.estimated_units -lt 0 -or $entry.estimated_units -gt $entry.reserved_units))) { throw 'invalid_budget_entry' }
        if ($entry.PSObject.Properties['sealed_units']) {
            if ($entry.kind -cne 'runtime-phase' -or $entry.sealed_units -lt 0 -or $entry.sealed_units -gt $entry.reserved_units -or -not $entry.seal.external_sends_disabled -or $entry.seal.currency -cne 'USD' -or $entry.seal.unit_scale -ne 1000000000) { throw 'invalid_phase_seal' }
            $used += [long]$entry.sealed_units
        } else {
            $used += if ($entry.estimated_units -ne $null) { [long]$entry.estimated_units } else { [long]$entry.reserved_units }
        }
    }
    return [long]$Budget.limit_units-$used
}
function Start-Phase($Budget,[string]$Id,[int]$ExpectedCalls) {
    if ($Id -cnotmatch '^[a-z][a-z0-9-]{0,29}$' -or $Budget.uncertain) { throw 'owned_phase_required' }
    $entry=@($Budget.entries | Where-Object id -CEQ $Id)
    if ($entry.Count -ne 1 -or $entry[0].status -cne 'reserved' -or $entry[0].kind -cne 'runtime-phase' -or $entry[0].reserved_units -lt 1 -or $entry[0].reserved_units -gt 100000000 -or $entry[0].max_calls -ne $ExpectedCalls) { throw 'phase_not_reserved' }
    # The filesystem claim survives a crash between acquisition and ledger save.
    $claim=[IO.File]::Open((Join-Path $state ($Id+'.started')),[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
    $claim.Dispose()
    $entry[0].status='running'; Save-Json 'campaign.json' $Budget
    return $entry[0]
}
function Reserve-Entry($Budget,[string]$Id,[long]$Units,[int]$Calls,[string]$Kind) {
    if ($Budget.uncertain -or @($Budget.entries | Where-Object id -CEQ $Id).Count -gt 0) { throw 'campaign_uncertain_or_duplicate' }
    $slots=0; foreach ($entry in $Budget.entries) { $slots += [int]$entry.max_calls }
    if ($Units -le 0 -or $Units -gt (Available-Units $Budget) -or $slots+$Calls -gt $Budget.max_calls) { throw 'campaign_hard_ceiling' }
    $Budget.entries=@($Budget.entries)+@([pscustomobject]@{id=$Id;kind=$Kind;reserved_units=$Units;estimated_units=$null;max_calls=$Calls;status='reserved';started_at=[DateTimeOffset]::Now.ToString('o')})
    Save-Json 'campaign.json' $Budget
}
if ($Action -eq 'Library') { return }
if ($Action -eq 'Prepare') {
    [IO.Directory]::CreateDirectory($state) | Out-Null
    if ((Get-Item -LiteralPath $state).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'campaign_directory_redirected' }
    $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User
    & icacls $state /inheritance:r /grant:r ('*'+$sid.Value+':(OI)(CI)F') '*S-1-5-18:(OI)(CI)F' >$null
    if ($LASTEXITCODE -ne 0) { throw 'campaign_acl_failed' }
} elseif (-not (Test-Path -LiteralPath (Join-Path $state 'campaign.json'))) { throw 'campaign_not_prepared' }
if ($Action -eq 'Status') {
    # Save-Json publishes an atomic snapshot. A read must remain available while
    # a long-running phase holds the exclusive mutation lock.
    $budget=Read-Json 'campaign.json'
    [ordered]@{limit_units=$budget.limit_units;available_units=(Available-Units $budget);currency=$budget.currency;unit_scale=$budget.unit_scale;uncertain=$budget.uncertain;entries=$budget.entries;retention_seconds=$null} | ConvertTo-Json -Depth 8
    return
}
$campaignLock=[IO.File]::Open((Join-Path $state 'campaign.lock'),[IO.FileMode]::OpenOrCreate,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
try {
    if ($Action -eq 'Prepare') {
        if (Test-Path -LiteralPath (Join-Path $state 'campaign.json')) { throw 'preserve_existing_campaign_budget' }
        $credential=Read-NebiusVault
        $handler=[Net.Http.HttpClientHandler]::new(); $handler.UseProxy=$false; $handler.AllowAutoRedirect=$false
        $client=[Net.Http.HttpClient]::new($handler); $client.Timeout=[TimeSpan]::FromSeconds(20)
        try {
            $client.DefaultRequestHeaders.Authorization=[Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer',$utf8.GetString($credential))
            $response=$client.GetAsync('https://api.tokenfactory.nebius.com/v1/models?verbose=true').GetAwaiter().GetResult()
            if (-not $response.IsSuccessStatusCode) { throw 'catalogue_refused' }
            $raw=$response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
            if ($raw.Length -gt 1048576 -or $raw.Contains($utf8.GetString($credential))) { throw 'catalogue_invalid' }
            $models=@(($raw | ConvertFrom-Json).data | Where-Object { $_.id -cmatch '^nvidia/' -and [int]$_.context_length -le 262144 -and [decimal]$_.pricing.request -eq 0 -and [decimal]$_.pricing.prompt -gt 0 -and [decimal]$_.pricing.completion -gt 0 })
            if ($models.Count -lt 2) { throw 'economic_roles_unavailable' }
            $sorted=@($models | Sort-Object @{Expression={ [decimal]$_.pricing.prompt+[decimal]$_.pricing.completion }},id)
            $cheapest=$sorted[0]; $highest=$sorted[-1]
            if ($cheapest.id -ceq $highest.id) { throw 'distinct_economic_roles_unavailable' }
            $modelRows=@($models | ForEach-Object {
                [ordered]@{id=$_.id;version=$null;output_mode='structured_json';protocol='chat';
                    output_schema=@{id='kyro-agent-contract';version='1';schema=@{type='object';required=@('contract');additionalProperties=$false;properties=@{contract=@{type='string';maxLength=32000}}}};
                    pricing=@{version='nebius-p3-'+(Get-Date -Format 'yyyy-MM-dd');effective_date=(Get-Date -Format 'yyyy-MM-dd');currency='USD';unit='usd';unit_scale=1000000000;input_units_per_million_tokens=[long]([decimal]$_.pricing.prompt*1000000000000000);output_units_per_million_tokens=[long]([decimal]$_.pricing.completion*1000000000000000)};
                    max_input_bytes=65536;max_input_tokens=[int]$_.context_length;max_output_tokens=8192;max_deadline_ms=120000;max_response_bytes=48000}
            })
            $pins=@([Net.Dns]::GetHostAddresses('api.tokenfactory.nebius.com') | Where-Object AddressFamily -eq InterNetwork | ForEach-Object { $_.IPAddressToString+':443' } | Sort-Object -Unique)
            if ($pins.Count -eq 0 -or $pins.Count -gt 16) { throw 'public_pins_unavailable' }
            Save-Json 'models.json' @{format_version=1;destinations=@(@{id='nebius-agents-recipe';provider='nebius';kind='cloud';base_url='https://api.tokenfactory.nebius.com/v1/';allowed_host='api.tokenfactory.nebius.com';pinned_addresses=$pins;secret_ref='file:KYRO_MODEL_API_KEY_FILE';qualified=$false;retention_seconds=$null;nebius=@{json_schema=$false;bounded_completion=$true;retention_evidence='https://docs.nebius.com/legal/token-factory';provider_standard_retention_accepted=$true;wire_overhead_tokens=1024;context_tokens=262144};models=$modelRows})}
            $roles=[ordered]@{}; foreach ($role in @('orchestrator','pixel','moka','kiwi','biscotte','review','security')) { $roles[$role]=@{destination_id='nebius-agents-recipe';model=if ($role -eq 'orchestrator') { $highest.id } else { $cheapest.id }} }
            Save-Json 'agents.json' @{synthetic=$false;poll_ms=1000;roles=$roles}
            Save-Json 'catalogue.json' @{checked_at=[DateTimeOffset]::Now.ToString('o');source='https://api.tokenfactory.nebius.com/v1/models?verbose=true';models=@($models | Select-Object id,context_length,quantization,regions,pricing);weights_version=$null}
        } finally { [Array]::Clear($credential,0,$credential.Length); $client.Dispose(); $handler.Dispose() }
        $xml=[xml](Invoke-WebRequest -Uri 'https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml').Content
        $rate=[decimal]($xml.Envelope.Cube.Cube.Cube | Where-Object currency -eq USD).rate
        $fxDate=[datetime]$xml.Envelope.Cube.Cube.time
        if ($rate -le 0 -or ((Get-Date)-$fxDate).TotalDays -gt 7 -or $fxDate -gt (Get-Date)) { throw 'fx_invalid_or_stale' }
        Save-Json 'campaign.json' @{version=1;ceiling_eur=1;currency='USD';unit_scale=1000000000;limit_units=[long][Math]::Floor($rate*0.60*1000000000);eur_usd=$rate;fx_date=$xml.Envelope.Cube.Cube.time;fx_source='https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml';margin_fraction=0.40;max_calls=32;uncertain=$false;entries=@();data_scope='Synthetic acceptance projects only';retention_seconds=$null;authorization='User real-call ceiling 1 EUR and explicit reuse of project credential; standard provider retention remains unknown'}
        Write-Output 'Campagne préparée, registre non qualifié, zéro inférence.'
        return
    }
    $budget=Read-Json 'campaign.json'
    if ($Action -eq 'ExpandCallSlots') {
        if ($budget.uncertain -or $budget.max_calls -notin @(32,64) -or $budget.ceiling_eur -ne 1) { throw 'campaign_slot_change_refused' }
        $budget.max_calls=[int]$budget.max_calls+32
        $budget | Add-Member -NotePropertyName slot_change -NotePropertyValue @{at=[DateTimeOffset]::Now.ToString('o');reason='Conservative phase slots include failed and unissued calls; increase internal bounded slot cap only, preserve all financial ceilings and holds'} -Force
        Save-Json 'campaign.json' $budget
        Write-Output ('Limite interne de slots portée à'+$budget.max_calls+' ; plafond monétaire et réserves strictement conservés.')
        return
    }
    if ($Action -eq 'Reserve') {
        if ($Phase -cnotmatch '^[a-z][a-z0-9-]{0,63}$') { throw 'phase_id_invalid' }
        Reserve-Entry $budget $Phase $PhaseLimitUnits $PhaseCalls 'runtime-phase'
        Write-Output ('Phase réservée : '+$Phase+'; plafond '+$PhaseLimitUnits+' nano-USD; '+$PhaseCalls+' appels maximum.')
        return
    }
    if ($budget.uncertain) { throw 'campaign_uncertain' }
    if ($Action -eq 'StrictMode') {
        $registry=Read-Json 'models.json'
        if ($registry.destinations[0].id -cne 'nebius-agents-recipe') { throw 'native_recipe_required' }
        foreach ($model in $registry.destinations[0].models) {
            $id='qualify-native-v2-'+($model.id -replace '[^A-Za-z0-9-]','-')
            $prior=@($budget.entries | Where-Object {$_.id -ceq $id -and $_.status -ceq 'qualified'})
            if ($prior.Count -ne 1 -or $model.output_schema.version -cne '2') { throw 'strict_transport_not_observed' }
            $probe=Read-Json ($id+'.json')
            $oldReview=$probe.request.response_format.json_schema.schema.properties.data.properties.contract.anyOf[2] | ConvertTo-Json -Depth 100 -Compress
            $newReview=$model.output_schema.schema.properties.contract.anyOf[2] | ConvertTo-Json -Depth 100 -Compress
            if ($oldReview -cne $newReview) { throw 'transport_probe_branch_changed' }
        }
        $registry.destinations[0].qualified=$true
        $registry.destinations[0].nebius.json_schema=$true
        $registry.destinations[0].nebius.json_object=$false
        Save-Json 'models.json' $registry
        Write-Output 'Transport strict déjà observé, sonde inchangée ; contrats métier renforcés à qualifier en intégration.'
        return
    }
    if ($Action -eq 'Seal') {
        if ($Phase -cnotmatch '^[a-z][a-z0-9-]{0,29}$') { throw 'owned_phase_required' }
        $entry=@($budget.entries | Where-Object id -CEQ $Phase)
        if ($entry.Count -ne 1 -or $entry[0].kind -cne 'runtime-phase' -or $entry[0].status -cnotmatch '^finished_exit_[0-9]+$' -or $entry[0].PSObject.Properties['sealed_units'] -or -not (Test-Path -LiteralPath (Join-Path $state ($Phase+'.started')))) { throw 'finished_unsealed_phase_required' }
        $database='kyro_p1_test_p3_live_'+$Phase.Replace('-','_')
        $sql=@'
BEGIN;
LOCK TABLE runtime_control, projects, project_budgets, jobs, effects, budget_reservations IN SHARE ROW EXCLUSIVE MODE;
DO $$ BEGIN
 IF (SELECT count(*) FROM projects) <> 1 OR (SELECT count(*) FROM project_budgets) <> 1 OR (SELECT count(*) FROM runtime_control WHERE id=1) <> 1
 OR EXISTS(SELECT 1 FROM projects WHERE (name NOT LIKE 'p3-live-%' AND name <> 'p3-protected-factory') OR NOT coalesce(data_policy->'allowed_destinations' = '["nebius-agents-recipe"]'::jsonb,false))
 OR EXISTS(SELECT 1 FROM jobs WHERE status NOT IN ('succeeded','failed','cancelled','expired','unknown'))
 OR EXISTS(SELECT 1 FROM effects WHERE status NOT IN ('succeeded','failed','unknown'))
 OR EXISTS(SELECT 1 FROM project_budgets WHERE currency <> 'USD' OR unit_scale <> 1000000000 OR spent_units < 0 OR reserved_units < 0 OR spent_units+reserved_units>limit_units
   OR reserved_units<>(SELECT coalesce(sum(units),0) FROM budget_reservations WHERE status='held'))
 THEN RAISE EXCEPTION 'phase_not_quiescent_or_invalid'; END IF;
END $$;
UPDATE runtime_control SET external_sends_enabled=false,updated_at=now() WHERE id=1;
UPDATE project_budgets SET limit_units=spent_units+reserved_units,configuration_version=configuration_version+1,updated_at=now();
SELECT json_build_object('database',current_database(),'project_id',project_id,'configuration_version',configuration_version,'currency',currency,'unit_scale',unit_scale,'spent_units',spent_units,'reserved_units',reserved_units,'limit_units',limit_units,'external_sends_disabled',NOT (SELECT external_sends_enabled FROM runtime_control WHERE id=1),'sealed_at',now()) FROM project_budgets;
COMMIT;
'@
        $raw=& docker exec kyro-p3-postgres-20261006 psql -U kyro_admin -d $database -qAt -v ON_ERROR_STOP=1 -c $sql
        if ($LASTEXITCODE -ne 0) { throw 'phase_database_seal_failed' }
        $seal=($raw -join "`n") | ConvertFrom-Json
        if ($seal.database -cne $database -or -not $seal.external_sends_disabled -or $seal.currency -cne 'USD' -or $seal.unit_scale -ne 1000000000 -or $seal.limit_units -ne ($seal.spent_units+$seal.reserved_units) -or $seal.limit_units -gt $entry[0].reserved_units) { throw 'phase_seal_receipt_invalid' }
        # Only the trusted database transaction can reclaim unissued capacity.
        # Unknown effects retain their full held reservations and cannot be resent.
        $entry[0] | Add-Member -NotePropertyName sealed_units -NotePropertyValue ([long]$seal.limit_units)
        $entry[0] | Add-Member -NotePropertyName seal -NotePropertyValue $seal
        Save-Json 'campaign.json' $budget
        Save-Json ($Phase+'-seal.json') $seal
        Write-Output ('Phase fermée aux émissions : '+$Phase+'; consommation connue et réserves maintenues '+$seal.limit_units+' nano-USD.')
        return
    }
    if ($Action -eq 'RefreshSchema') {
        $registry=Read-Json 'models.json'
        if (-not $registry.destinations[0].qualified -or -not $registry.destinations[0].nebius.json_object) { throw 'qualified_object_transport_required' }
        $schema=Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot '../../crates/kyro-agents/src/contract-v2.schema.json') | ConvertFrom-Json
        foreach ($model in $registry.destinations[0].models) {
            $oldReview=$model.output_schema.schema.properties.contract.anyOf[2] | ConvertTo-Json -Depth 100 -Compress
            $newReview=$schema.properties.contract.anyOf[2] | ConvertTo-Json -Depth 100 -Compress
            if ($oldReview -cne $newReview) { throw 'transport_probe_branch_changed' }
            $model.output_schema.schema=$schema
        }
        Save-Json 'models.json' $registry
        Save-Json 'schema-refresh.json' @{at=[DateTimeOffset]::Now.ToString('o');method='JSON object transport already observed; Review transport probe unchanged; role contracts and new schema still require actual acceptance';schema_sha256=(Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $PSScriptRoot '../../crates/kyro-agents/src/contract-v2.schema.json')).Hash.ToLowerInvariant()}
        Write-Output 'Schéma local renforcé, branche de sonde inchangée ; qualification métier encore requise.'
        return
    }
    if ($Action -eq 'Run') {
        if ($Container -cnotmatch '^kyro-p3-nvidia-[a-z0-9-]+$' -or $Phase -cnotmatch '^[a-z][a-z0-9-]{0,29}$') { throw 'owned_phase_required' }
        $expectedCalls=if ($Scenario -eq 'Factory') {4} else {7}
        $entry=Start-Phase $budget $Phase $expectedCalls
        $database='kyro_p1_test_p3_live_'+$Phase.Replace('-','_')
        $test=if ($Scenario -eq 'Factory') {'nvidia_builds_a_protected_verified_candidate'} else {'nvidia_qualifies_seven_roles_on_native_contracts'}
        $runArguments=@('exec','-e','PATH=/opt/rust/bin:/usr/local/bin:/usr/bin:/bin',
            '-e','CARGO_HOME=/opt/cargo','-e','CARGO_TARGET_DIR=/workspace/target','-e','CARGO_BUILD_JOBS=4',
            '-e','KYRO_P3_LIVE_CONFIRM=synthetic-only-1eur','-e',('KYRO_P3_LIVE_PHASE='+$Phase),
            '-e',('KYRO_TEST_DATABASE_ADMIN_URL=postgres://kyro_admin@kyro-p3-postgres-20261006/'+$database),
            '-e',('KYRO_TEST_DATABASE_URL=postgres://kyro_api@kyro-p3-postgres-20261006/'+$database),
            '-e',('KYRO_TEST_WORKER_DATABASE_URL=postgres://kyro_worker@kyro-p3-postgres-20261006/'+$database),
            '-e',('KYRO_P3_LIVE_REPORT=/tmp/kyro-p3-nvidia-'+$Phase+'.json'),
            '-e',('KYRO_P3_FACTORY_PROOF_DIR=/tmp/kyro-p3-factory-proof-'+$Phase))
        if (Test-Path -LiteralPath (Join-Path $state 'provider-root.pem')) {
            $runArguments+=@('-e','KYRO_MODEL_TLS_ROOT_CERTIFICATE_FILE=/p3-campaign/provider-root.pem')
        }
        $runArguments+=@($Container,'cargo','test','-p','kyro-agents','--test','nvidia','--locked','--offline','--',$test,'--ignored','--exact','--nocapture')
        & docker @runArguments
        $runExit=$LASTEXITCODE
        $entry.status='finished_exit_'+$runExit; Save-Json 'campaign.json' $budget
        # Its full ceiling stays held until an explicit trusted DB Seal. A model
        # report, retry or operator estimate cannot release it.
        if ($runExit -ne 0) { throw ('phase_failed_exit_'+$runExit) }
        return
    }
    if ($Action -eq 'Native') {
        $registry=Read-Json 'models.json'
        if ($registry.destinations[0].qualified) { throw 'preserve_qualified_registry' }
        $schema=Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot '../../crates/kyro-agents/src/contract-v2.schema.json') | ConvertFrom-Json
        foreach ($model in $registry.destinations[0].models) {
            $model.output_schema.version='2'; $model.output_schema.schema=$schema
        }
        Save-Json 'models.json' $registry
        Write-Output 'Protocole natif v2 préparé ; anciennes intentions et budget conservés.'
        return
    }
    if ($Action -eq 'ObjectMode') {
        $registry=Read-Json 'models.json'
        if (@($registry.destinations[0].models | Where-Object {$_.output_schema.version -cne '2'}).Count -gt 0) { throw 'native_protocol_required' }
        $registry.destinations[0].qualified=$false
        $registry.destinations[0].nebius.json_schema=$false
        $registry.destinations[0].nebius | Add-Member -NotePropertyName json_object -NotePropertyValue $true -Force
        Save-Json 'models.json' $registry
        Write-Output 'Transport JSON objet préparé ; registre non qualifié, budget et effets conservés.'
        return
    }
    if ($Action -eq 'Deliver') {
        if ($Container -cnotmatch '^kyro-p3-nvidia-[a-z0-9-]+$') { throw 'owned_controller_required' }
        $credential=Read-NebiusVault
        $process=[Diagnostics.Process]::new(); $process.StartInfo=[Diagnostics.ProcessStartInfo]::new('docker')
        foreach ($argument in @('exec','-i',$Container,'sh','-c','umask 077; cat > /run/kyro-p3-secrets/key')) { $process.StartInfo.ArgumentList.Add($argument) }
        $process.StartInfo.UseShellExecute=$false; $process.StartInfo.RedirectStandardInput=$true; $process.StartInfo.RedirectStandardOutput=$true; $process.StartInfo.RedirectStandardError=$true; $process.StartInfo.CreateNoWindow=$true
        try { $process.Start() | Out-Null; $process.StandardInput.BaseStream.Write($credential,0,$credential.Length); $process.StandardInput.Close(); if (-not $process.WaitForExit(10000)) { $process.Kill(); throw 'credential_delivery_timeout' }; if ($process.ExitCode -ne 0) { throw 'credential_delivery_failed' } }
        finally { [Array]::Clear($credential,0,$credential.Length); $process.Dispose() }
        Write-Output 'Clé chargée dans le tmpfs du contrôleur dédié.'
        return
    }
    if ($Action -ne 'Qualify') { throw 'unsupported_campaign_action' }
    $registry=Read-Json 'models.json'
    $credential=Read-NebiusVault
    $handler=[Net.Http.HttpClientHandler]::new(); $handler.UseProxy=$false; $handler.AllowAutoRedirect=$false
    $client=[Net.Http.HttpClient]::new($handler); $client.Timeout=[TimeSpan]::FromSeconds(120)
    try {
        $client.DefaultRequestHeaders.Authorization=[Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer',$utf8.GetString($credential))
        foreach ($model in $registry.destinations[0].models) {
            if ($model.output_schema.version -cne '2') { throw 'native_protocol_required' }
            $objectMode=$registry.destinations[0].nebius.PSObject.Properties['json_object'] -and $registry.destinations[0].nebius.json_object
            $id=$(if ($objectMode) {'qualify-object-v2-'} else {'qualify-native-v2-'})+($model.id -replace '[^A-Za-z0-9-]','-')
            $prior=@($budget.entries | Where-Object id -CEQ $id)
            if ($prior.Count -gt 0) { if ($prior[0].status -ne 'qualified') { throw 'qualification_already_attempted' }; continue }
            $reserved=[long][Math]::Ceiling(([decimal]$model.max_input_tokens*$model.pricing.input_units_per_million_tokens+[decimal]8192*$model.pricing.output_units_per_million_tokens)/1000000)
            Reserve-Entry $budget $id $reserved 1 'qualification'
            $envelope=@{type='object';required=@('schema_id','schema_version','data');additionalProperties=$false;properties=@{schema_id=@{type='string';const='kyro-agent-contract';maxLength=128};schema_version=@{type='string';const='2';maxLength=64};data=$model.output_schema.schema}}
            $expectedDigest='0'*64
            $wireExample=@{schema_id='kyro-agent-contract';schema_version='2';data=@{contract=@{candidate_digest=$expectedDigest;approved=$false;findings=@()}}} | ConvertTo-Json -Depth 5 -Compress
            $body=@{model=$model.id;messages=@(@{role='system';content='Return only the JSON object matching the supplied schema. No tools.'},@{role='user';content=('Synthetic wire protocol qualification only, not an approval of any application. Select the Review branch. Return this native JSON object exactly: '+$wireExample)});max_tokens=8192;max_completion_tokens=8192;n=1;stream=$false;store=$false;response_format=@{type='json_schema';json_schema=@{name='kyro-agent-contract';strict=$true;schema=$envelope}}}
            if ($objectMode) {
                $body.response_format=@{type='json_object'}
                $body.messages[0].content='Return one JSON object matching this trusted output schema: '+($envelope | ConvertTo-Json -Depth 100 -Compress)+'. No tools.'
            }
            $payload=[Net.Http.StringContent]::new(($body | ConvertTo-Json -Depth 100),$utf8,'application/json')
            $started=[DateTimeOffset]::Now
            try {
                $response=$client.PostAsync('https://api.tokenfactory.nebius.com/v1/chat/completions',$payload).GetAwaiter().GetResult()
                $raw=$response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
                if ($raw.Length -gt 48000 -or $raw.Contains($utf8.GetString($credential))) { throw 'qualification_response_invalid' }
                if (-not $response.IsSuccessStatusCode) { Save-Json ($id+'.json') @{status='refused';model=$model.id;response=(Get-QualificationReceipt ([int]$response.StatusCode) $model.id $raw $false ($utf8.GetString($credential)))}; throw 'qualification_provider_refused' }
                $answer=try { $raw | ConvertFrom-Json } catch { throw 'qualification_response_not_json' }
                $receipt=Get-QualificationReceipt ([int]$response.StatusCode) $model.id $answer $false ($utf8.GetString($credential))
                $entry=@($budget.entries | Where-Object id -CEQ $id)[0]
                $entry.estimated_units=Get-QualificationEstimate $model $receipt
                $entry.status='observed'; Save-Json 'campaign.json' $budget
                Save-Json ($id+'.json') @{status='observed';started_at=$started.ToString('o');completed_at=[DateTimeOffset]::Now.ToString('o');provider='nebius';model=$model.id;retention_seconds=$null;request=$body;response=$receipt;estimated_units=$entry.estimated_units;invoiced_units=$null}
                $output=try { $answer.choices[0].message.content | ConvertFrom-Json } catch { throw 'qualification_contract_not_json' }
                if ($answer.model -cne $model.id -or $answer.choices[0].finish_reason -cne 'stop' -or $output.schema_id -cne 'kyro-agent-contract' -or $output.schema_version -cne '2') { throw 'qualification_contract_invalid' }
                $contractObject=$output.data.contract
                if (@($output.PSObject.Properties).Count -ne 3 -or @($output.data.PSObject.Properties).Count -ne 1 -or $contractObject -isnot [pscustomobject] -or @($contractObject.PSObject.Properties).Count -ne 3 -or $contractObject.candidate_digest -cne $expectedDigest -or $contractObject.approved -isnot [bool] -or $contractObject.approved -ne $false -or $contractObject.findings -isnot [array] -or @($contractObject.findings).Count -ne 0) { throw 'qualification_json_object_required' }
                Save-Json ($id+'.json') @{status='qualified';started_at=$started.ToString('o');completed_at=[DateTimeOffset]::Now.ToString('o');elapsed_ms=([DateTimeOffset]::Now-$started).TotalMilliseconds;provider='nebius';model=$model.id;weights_version=$null;retention_seconds=$null;request=$body;response=(Get-QualificationReceipt ([int]$response.StatusCode) $model.id $answer $true ($utf8.GetString($credential)));estimated_units=$entry.estimated_units;invoiced_units=$null}
                $entry.status='qualified'; Save-Json 'campaign.json' $budget
                Write-Output ('Contrat structuré réel qualifié : '+$model.id)
            } catch {
                $observedEntry=@($budget.entries | Where-Object id -CEQ $id)[0]
                if ($observedEntry.estimated_units -ne $null) { $observedEntry.status='contract_refused_known_usage' }
                else { $budget.uncertain=$true }
                Save-Json 'campaign.json' $budget; throw
            }
            finally { $payload.Dispose() }
        }
        $registry.destinations[0].qualified=$true
        if (-not $objectMode) { $registry.destinations[0].nebius.json_schema=$true }
        Save-Json 'models.json' $registry
        Write-Output 'Capacité structurée des modèles vérifiée ; contrats des sept rôles à vérifier par la recette P3.'
    } finally { [Array]::Clear($credential,0,$credential.Length); $client.Dispose(); $handler.Dispose() }
} finally { $campaignLock.Dispose() }
