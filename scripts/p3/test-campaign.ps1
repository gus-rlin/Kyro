Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'nebius-campaign.ps1') -Action Library
$p3TestRoot=Join-Path ([IO.Path]::GetTempPath()) ('kyro-p3-budget-test-'+[Guid]::NewGuid().ToString('N'))
$state=[IO.Directory]::CreateDirectory($p3TestRoot).FullName
$checks=0
function Reject([scriptblock]$Operation,[string]$Message) {
    $rejected=$false
    try { & $Operation } catch {
        if (-not $_.Exception.Message.Contains($Message)) { throw }
        $rejected=$true
    }
    if (-not $rejected) { throw 'expected_rejection_missing' }
    $script:checks++
}
try {
    $budget=[pscustomobject]@{limit_units=100;max_calls=20;uncertain=$false;entries=@(
        [pscustomobject]@{id='known';kind='qualification';reserved_units=40;estimated_units=10;max_calls=1},
        [pscustomobject]@{id='unknown';kind='runtime-phase';reserved_units=30;estimated_units=$null;max_calls=7}
    )}
    if ((Available-Units $budget) -ne 60) { throw 'unknown_hold_lost' }; $checks++
    Reject { Reserve-Entry $budget 'over-money' 61 4 'runtime-phase' } 'campaign_hard_ceiling'
    Reject { Reserve-Entry $budget 'known' 1 4 'runtime-phase' } 'campaign_uncertain_or_duplicate'
    $budget.max_calls=8
    Reject { Reserve-Entry $budget 'over-slots' 1 4 'runtime-phase' } 'campaign_hard_ceiling'
    $budget.max_calls=20; $budget.uncertain=$true
    Reject { Reserve-Entry $budget 'uncertain' 1 4 'runtime-phase' } 'campaign_uncertain_or_duplicate'
    $budget.uncertain=$false
    $budget.entries[1] | Add-Member -NotePropertyName sealed_units -NotePropertyValue 30
    $budget.entries[1] | Add-Member -NotePropertyName seal -NotePropertyValue ([pscustomobject]@{external_sends_disabled=$false;currency='USD';unit_scale=1000000000})
    Reject { Available-Units $budget } 'invalid_phase_seal'
    $budget.entries[1].seal.external_sends_disabled=$true
    if ((Available-Units $budget) -ne 60) { throw 'seal_released_unknown_hold' }; $checks++
    Reserve-Entry $budget 'once' 50 7 'runtime-phase'
    Reject { Start-Phase $budget 'once' 4 } 'phase_not_reserved'
    $first=Start-Phase $budget 'once' 7
    if ($first.status -cne 'running' -or (Read-Json 'campaign.json').entries[-1].status -cne 'running') { throw 'phase_claim_not_durable' }; $checks++
    Reject { Start-Phase $budget 'once' 7 } 'phase_not_reserved'
    # A stale ledger (crash before save) still cannot execute the phase twice.
    $first.status='reserved'
    Reject { Start-Phase $budget 'once' 7 } 'already exists'
    $wire=@'
{"id":"chatcmpl-synthetic","model":"nvidia/test","choices":[{"finish_reason":"stop","message":{"content":"SYNTHETIC_PRIVATE_VALUE","reasoning_content":"SYNTHETIC_PRIVATE_VALUE"},"extra":"SYNTHETIC_PRIVATE_VALUE"}],"usage":{"prompt_tokens":20,"completion_tokens":10,"prompt_tokens_details":{"cached_tokens":5,"extra":"SYNTHETIC_PRIVATE_VALUE"},"extra":"SYNTHETIC_PRIVATE_VALUE"},"prompt_text":"SYNTHETIC_PRIVATE_VALUE","unknown":"SYNTHETIC_PRIVATE_VALUE"}
'@ | ConvertFrom-Json
    $syntheticSecret='SYNTHETIC_PROVIDER_KEY_123'
    $receipt=Get-QualificationReceipt 200 'nvidia/test' $wire $true $syntheticSecret
    Save-Json 'receipt.json' $receipt
    $serialized=Get-Content -Raw -LiteralPath (Join-Path $state 'receipt.json')
    if ($serialized.Contains('SYNTHETIC_PRIVATE_VALUE') -or $serialized.Contains('reasoning_content') -or $serialized.Contains('prompt_text') -or $serialized.Contains('unknown') -or $serialized.Contains('choices')) { throw 'provider_fields_not_redacted' }; $checks++
    if ($receipt.id -cne 'chatcmpl-synthetic' -or $receipt.usage.prompt_tokens -ne 20 -or $receipt.usage.completion_tokens -ne 10 -or $receipt.usage.cached_prompt_tokens -ne 5 -or -not $receipt.contract_verified -or $receipt.contract.candidate_digest -cne ('0'*64) -or $receipt.contract.approved) { throw 'qualification_measurements_lost' }; $checks++
    $refusal=Get-QualificationReceipt 429 'nvidia/test' 'SYNTHETIC_PRIVATE_VALUE' $false $syntheticSecret
    if (($refusal | ConvertTo-Json -Compress) -cne '{"http_status":429}') { throw 'provider_error_body_not_redacted' }; $checks++
    $wire.model='SYNTHETIC_PRIVATE_VALUE'; $wire.id='invalid SYNTHETIC_PRIVATE_VALUE'; $wire.choices[0].finish_reason='SYNTHETIC_PRIVATE_VALUE'
    $mismatch=Get-QualificationReceipt 200 'nvidia/test' $wire $false $syntheticSecret
    if ($mismatch.model_matches_expected -or $mismatch.finish_reason_is_stop -or $mismatch.id -ne $null -or $mismatch.model -ne $null -or ($mismatch | ConvertTo-Json -Depth 8).Contains('SYNTHETIC_PRIVATE_VALUE')) { throw 'untrusted_receipt_metadata_not_redacted' }; $checks++
    Reject { Get-QualificationReceipt 200 'nvidia/test' $wire $true $syntheticSecret } 'qualification_contract_invalid'
    $wire.usage.prompt_tokens='SYNTHETIC_PRIVATE_VALUE'
    Reject { Get-QualificationReceipt 200 'nvidia/test' $wire $false $syntheticSecret } 'qualification_usage_invalid'
    $escapedWire='{"id":"\u0053YNTHETIC_PROVIDER_KEY_123","model":"nvidia/test","usage":{"prompt_tokens":20,"completion_tokens":10}}'
    if ($escapedWire.Contains($syntheticSecret)) { throw 'escaped_secret_fixture_not_escaped' }; $checks++
    $decodedWire=$escapedWire | ConvertFrom-Json
    Reject { Get-QualificationReceipt 200 'nvidia/test' $decodedWire $false $syntheticSecret } 'qualification_response_invalid'
    Reject { Get-QualificationReceipt 200 'nvidia/test' $decodedWire $false '' } 'qualification_secret_guard_required'
    $model=[pscustomobject]@{id='nvidia/test';max_input_tokens=262144;pricing=[pscustomobject]@{input_units_per_million_tokens=60000000;output_units_per_million_tokens=240000000}}
    if ((Get-QualificationEstimate $model $receipt) -ne 3600) { throw 'qualified_usage_estimate_invalid' }; $checks++
    $pending=[pscustomobject]@{limit_units=100000;max_calls=1;uncertain=$false;entries=@([pscustomobject]@{id='unbound';kind='qualification';reserved_units=10000;estimated_units=$null;max_calls=1})}
    Reject { $pending.entries[0].estimated_units=Get-QualificationEstimate $model $mismatch } 'qualification_model_invalid'
    if ($pending.entries[0].estimated_units -ne $null -or (Available-Units $pending) -ne 90000) { throw 'unbound_model_released_reserve' }; $checks++
    $decodedWire.id='chatcmpl-synthetic'; $decodedWire.model=@('nvidia/test')
    $arrayModel=Get-QualificationReceipt 200 'nvidia/test' $decodedWire $false $syntheticSecret
    Reject { Get-QualificationEstimate $model $arrayModel } 'qualification_model_invalid'
    Write-Output ('PASS '+$checks+' contrôles budget/claim/expurgation ; aucun coffre, réseau ni appel fournisseur utilisé.')
} finally {
    $resolved=[IO.Path]::GetFullPath($state)
    if ($resolved -cne [IO.Path]::GetFullPath($p3TestRoot) -or -not $resolved.StartsWith([IO.Path]::GetFullPath([IO.Path]::GetTempPath()),[StringComparison]::OrdinalIgnoreCase)) { throw 'test_cleanup_path_invalid' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
